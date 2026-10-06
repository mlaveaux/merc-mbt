use std::collections::VecDeque;
use std::io;
use std::io::Read;
use std::io::Write;
use std::time::Duration;
use std::time::Instant;

use tungstenite::Error as WsError;
use tungstenite::Message;
use tungstenite::WebSocket;

use crate::action::MultiActionKey;
use crate::action::describe_multi_action;
use crate::early_set::EarlyEntry;
use crate::early_set::EarlySet;
use crate::error::ErrorCode;
use crate::error::MbtError;
use crate::model::ModelState;
use crate::protocol::AckKind;
use crate::protocol::AdapterHello;
use crate::protocol::AdapterMessage;
use crate::protocol::Close;
use crate::protocol::DecodeError;
use crate::protocol::Enabled;
use crate::protocol::ErrorMessage;
use crate::protocol::GetEnabled;
use crate::protocol::Heartbeat;
use crate::protocol::MessageId;
use crate::protocol::Observation;
use crate::protocol::PROTOCOL_VERSION;
use crate::protocol::QuiescenceReport;
use crate::protocol::Reset;
use crate::protocol::SessionConfig;
use crate::protocol::ToolMessage;
use crate::protocol::Warning;
use crate::protocol::WarningCode;
use crate::protocol::decode_frame;
use crate::transport::ReadDeadline;
use crate::transport::send_message;

/// Floor under the socket read deadline: `TcpStream::set_read_timeout(Some(Duration::ZERO))`
/// is rejected outright with `InvalidInput`, so the loop never asks for a
/// zero timeout.
const MIN_READ_TIMEOUT: Duration = Duration::from_millis(1);

/// Caps the session enforces on top of the adapter's own `hello.config`,
/// independent of the model's own `--max-state-set-size` bound (enforced
/// inside [`crate::model::ModelState`]).
#[derive(Debug, Clone, Copy)]
pub struct SessionLimits {
    /// Reject an adapter-requested `tau_closure_depth` above this with
    /// `unsupported_config`. `0` disables the check.
    pub max_tau_closure_depth: usize,
}

/// Where the session is in its lifecycle, mirroring the protocol's handshake
/// state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPhase {
    AwaitingHello,
    Ready,
    Closing,
}

/// A running MBT session: the WebSocket, the model, and the timers, driven to
/// completion by [`MbtSession::run`].
///
/// Generic over the underlying stream so the same event loop drives both a
/// real adapter connection (`WebSocket<MaybeTlsStream<TcpStream>>`, via
/// [`crate::transport::connect_adapter`]) and, in tests, an in-process mock
/// adapter over a plain `WebSocket<TcpStream>`.
pub struct MbtSession<S: Read + Write + ReadDeadline> {
    socket: WebSocket<S>,
    model: ModelState,
    limits: SessionLimits,
    config: SessionConfig,
    phase: SessionPhase,
    /// The spec's single FIFO processing queue. With one thread it is
    /// drained to empty after every read, so it holds at most one entry —
    /// the FIFO ordering is structurally guaranteed rather than enforced.
    /// Kept as an explicit field regardless, because `reset` must discard it
    /// as a real operation and it makes the invariant assertable.
    queue: VecDeque<AdapterMessage>,
    early: EarlySet,
    /// `None` when `config.heartbeat_interval_ms == 0`, disabling the tool's
    /// automatic sending.
    next_heartbeat_send: Option<Instant>,
    /// `None` when `config.heartbeat_timeout_ms == 0`, disabling the
    /// peer-timeout check.
    peer_deadline: Option<Instant>,
    hello: ToolMessage,
}

impl<S: Read + Write + ReadDeadline> MbtSession<S> {
    /// Builds a session ready to run. `hello` is the tool's own `hello`
    /// message, sent as the first thing [`MbtSession::run`] does.
    pub fn new(socket: WebSocket<S>, model: ModelState, limits: SessionLimits, hello: ToolMessage) -> Self {
        let now = Instant::now();
        let config = SessionConfig::default();
        MbtSession {
            socket,
            model,
            limits,
            next_heartbeat_send: deadline_after(config.heartbeat_interval_ms, now),
            peer_deadline: deadline_after(config.heartbeat_timeout_ms, now),
            config,
            phase: SessionPhase::AwaitingHello,
            queue: VecDeque::new(),
            early: EarlySet::default(),
            hello,
        }
    }

    /// Runs the session to completion: sends the tool's `hello`, then
    /// services timers and inbound frames until the connection closes.
    pub fn run(&mut self) -> Result<(), MbtError> {
        self.send(self.hello.clone())?;

        loop {
            if self.phase == SessionPhase::Closing {
                break;
            }

            let now = Instant::now();
            if self.service_timers(now)? {
                continue;
            }

            let timeout = self
                .next_deadline()
                .map(|deadline| deadline.saturating_duration_since(now).max(MIN_READ_TIMEOUT));
            self.socket
                .get_mut()
                .set_read_deadline(timeout)
                .map_err(|err| MbtError::Transport(WsError::Io(err)))?;

            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    self.note_inbound();
                    match decode_frame(&text) {
                        Ok(msg) => {
                            self.queue.push_back(msg);
                            self.drain_queue()?;
                        }
                        Err(decode_err) => self.reply_decode_error(decode_err)?,
                    }
                }
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => self.note_inbound(),
                Ok(Message::Close(frame)) => {
                    self.note_inbound();
                    let reason = frame.and_then(|f| (!f.reason.is_empty()).then(|| f.reason.to_string()));
                    self.begin_close(reason)?;
                }
                Ok(Message::Binary(_) | Message::Frame(_)) => {
                    // The protocol mandates text frames only.
                    self.fail(MbtError::MalformedMessage(
                        "expected a text frame, got a binary frame".into(),
                    ))?;
                }
                Err(WsError::Io(err))
                    if err.kind() == io::ErrorKind::WouldBlock || err.kind() == io::ErrorKind::TimedOut =>
                {
                    // The read deadline expired with nothing to read; loop
                    // back around to `service_timers`.
                    continue;
                }
                Err(WsError::ConnectionClosed) | Err(WsError::AlreadyClosed) => break,
                Err(err) => {
                    self.fail(MbtError::Transport(err))?;
                    break;
                }
            }
        }

        Ok(())
    }

    /// Fires every timer already due as of `now`, in the protocol's mandated
    /// order (peer timeout, then early-set expiries, then a due heartbeat
    /// send), and returns whether anything fired — the caller re-checks
    /// timers before blocking on the socket again rather than assuming a
    /// single pass drained them all.
    fn service_timers(&mut self, now: Instant) -> Result<bool, MbtError> {
        if self.peer_deadline.is_some_and(|deadline| now >= deadline) {
            log::warn!("Adapter heartbeat timed out; closing the session.");
            self.begin_close(Some("peer lost".to_string()))?;
            return Ok(true);
        }

        let expired = self.early.take_expired(now);
        if !expired.is_empty() {
            for entry in expired {
                self.send(ToolMessage::Error(ErrorMessage {
                    in_reply_to: Some(entry.id),
                    code: ErrorCode::OutputUnprocessed,
                    message: format!(
                        "output `{}` was not matched within early_output_timeout_ms ({} ms)",
                        describe_multi_action(&entry.key.as_wire()),
                        self.config.early_output_timeout_ms
                    ),
                }))?;
            }
            return Ok(true);
        }

        if self.next_heartbeat_send.is_some_and(|deadline| now >= deadline) {
            self.send(ToolMessage::Heartbeat(Heartbeat::default()))?;
            return Ok(true);
        }

        Ok(false)
    }

    /// The next instant the loop must wake up by, absent any inbound frame:
    /// the nearest of the next scheduled heartbeat send, the peer deadline,
    /// and the earliest pending early-set expiry — or `None` if all three
    /// are disabled/empty, in which case the loop simply blocks on the
    /// socket until a frame arrives.
    fn next_deadline(&self) -> Option<Instant> {
        next_deadline(
            self.next_heartbeat_send,
            self.peer_deadline,
            self.early.earliest_deadline(),
        )
    }

    /// Sends `message` and resets the heartbeat send timer — the protocol
    /// requires at least one message of any kind every
    /// `heartbeat_interval_ms`, so every outbound message, not only
    /// heartbeats, counts. A no-op on the timer when
    /// `heartbeat_interval_ms == 0` disables it.
    fn send(&mut self, message: ToolMessage) -> Result<(), MbtError> {
        send_message(&mut self.socket, &message)?;
        self.next_heartbeat_send = deadline_after(self.config.heartbeat_interval_ms, Instant::now());
        Ok(())
    }

    /// Refreshes the peer deadline on any inbound frame, including Ping/Pong.
    fn note_inbound(&mut self) {
        self.peer_deadline = deadline_after(self.config.heartbeat_timeout_ms, Instant::now());
    }

    fn drain_queue(&mut self) -> Result<(), MbtError> {
        debug_assert!(
            self.queue.len() <= 1,
            "the single-threaded loop drains to at most one queued message"
        );
        while let Some(msg) = self.queue.pop_front() {
            self.handle_message(msg)?;
            if self.phase == SessionPhase::Closing {
                break;
            }
        }
        Ok(())
    }

    fn handle_message(&mut self, msg: AdapterMessage) -> Result<(), MbtError> {
        match (self.phase, msg) {
            (SessionPhase::AwaitingHello, AdapterMessage::Hello(hello)) => self.handle_hello(hello),
            (SessionPhase::AwaitingHello, AdapterMessage::Heartbeat(_)) => {
                self.note_inbound();
                Ok(())
            }
            (SessionPhase::AwaitingHello, AdapterMessage::Close(close)) => self.begin_close(close.reason),
            (SessionPhase::AwaitingHello, other) => {
                // The adapter's `hello` may still be in flight; reply and
                // keep the connection rather than closing it.
                let id = message_id(&other);
                self.reply_error(id, MbtError::NotReady(message_kind(&other)))
            }
            (SessionPhase::Ready, AdapterMessage::Hello(_)) => {
                self.reply_error(None, MbtError::NotReady("hello (handshake already complete)"))
            }
            (SessionPhase::Ready, AdapterMessage::Heartbeat(_)) => {
                self.note_inbound();
                Ok(())
            }
            (SessionPhase::Ready, AdapterMessage::Close(close)) => self.begin_close(close.reason),
            (SessionPhase::Ready, AdapterMessage::Reset(reset)) => self.handle_reset(reset),
            (SessionPhase::Ready, AdapterMessage::GetEnabled(get)) => self.handle_get_enabled(get),
            (SessionPhase::Ready, AdapterMessage::Input(obs)) => self.handle_input(obs),
            (SessionPhase::Ready, AdapterMessage::Output(obs)) => self.handle_output(obs),
            (SessionPhase::Ready, AdapterMessage::Quiescence(q)) => self.handle_quiescence(q),
            (SessionPhase::Closing, _) => Ok(()),
        }
    }

    fn handle_hello(&mut self, hello: AdapterHello) -> Result<(), MbtError> {
        if hello.role != "adapter" {
            return self.reply_error(
                None,
                MbtError::UnsupportedConfig(format!("expected `hello.role` = \"adapter\", got \"{}\"", hello.role)),
            );
        }

        if major_minor(&hello.protocol_version) != major_minor(PROTOCOL_VERSION) {
            return self.reply_error(
                None,
                MbtError::ProtocolMismatch {
                    peer: hello.protocol_version,
                    expected: PROTOCOL_VERSION,
                },
            );
        }

        if self.limits.max_tau_closure_depth != 0 && hello.config.tau_closure_depth > self.limits.max_tau_closure_depth
        {
            return self.reply_error(
                None,
                MbtError::UnsupportedConfig(format!(
                    "adapter requested tau_closure_depth {}, exceeding --max-tau-closure-depth {}",
                    hello.config.tau_closure_depth, self.limits.max_tau_closure_depth
                )),
            );
        }

        self.config = hello.config;
        let now = Instant::now();
        self.next_heartbeat_send = deadline_after(self.config.heartbeat_interval_ms, now);
        self.peer_deadline = deadline_after(self.config.heartbeat_timeout_ms, now);
        self.phase = SessionPhase::Ready;
        log::info!(
            "Handshake complete with adapter {:?}; config = {:?}",
            hello.adapter,
            self.config
        );
        Ok(())
    }

    fn handle_reset(&mut self, reset: Reset) -> Result<(), MbtError> {
        self.model.reset();
        self.queue.clear();
        // Discarded silently: no ack/warning/error for entries dropped here.
        self.early.clear();
        self.send(ToolMessage::ResetAck { in_reply_to: reset.id })
    }

    fn handle_get_enabled(&mut self, get: GetEnabled) -> Result<(), MbtError> {
        let enabled_set = self.model.get_enabled(self.config.tau_closure_depth)?;
        self.send(ToolMessage::Enabled(Enabled {
            in_reply_to: get.id,
            inputs: enabled_set.inputs,
            outputs: enabled_set.outputs,
            quiescence: enabled_set.quiescence,
        }))
    }

    fn handle_input(&mut self, obs: Observation) -> Result<(), MbtError> {
        let key = MultiActionKey::from_wire(&obs.multi_action);
        if self.model.accept_input(&key, self.config.tau_closure_depth)? {
            self.send(ToolMessage::Ack {
                in_reply_to: obs.id,
                kind: AckKind::Input,
            })?;
            self.reevaluate_early()
        } else {
            self.reply_error(
                Some(obs.id),
                MbtError::InputNotEnabled(describe_multi_action(&obs.multi_action)),
            )
        }
    }

    fn handle_output(&mut self, obs: Observation) -> Result<(), MbtError> {
        let key = MultiActionKey::from_wire(&obs.multi_action);
        if self.model.accept_output(&key, self.config.tau_closure_depth)? {
            self.send(ToolMessage::Ack {
                in_reply_to: obs.id,
                kind: AckKind::Output,
            })?;
            self.reevaluate_early()
        } else {
            // Not currently enabled: hold it in the early set rather than
            // rejecting it outright, per the protocol's early-output
            // allowance. `early_output_timeout_ms = 0` needs no special
            // case: the deadline is already due, so `service_timers`'s next
            // pass (or the explicit sweep below) expires it immediately —
            // exactly one `warning` then one `error`, never an `ack`.
            let deadline = Instant::now() + Duration::from_millis(self.config.early_output_timeout_ms);
            self.early.push(EarlyEntry {
                id: obs.id,
                key,
                deadline,
            });
            self.send(ToolMessage::Warning(Warning {
                in_reply_to: obs.id,
                code: WarningCode::OutputEarly,
                message: "output not currently enabled; held pending a state change".to_string(),
            }))?;
            self.sweep_expired(Instant::now())
        }
    }

    fn handle_quiescence(&mut self, q: QuiescenceReport) -> Result<(), MbtError> {
        if self.model.accept_quiescence(self.config.tau_closure_depth)? {
            self.send(ToolMessage::Ack {
                in_reply_to: q.id,
                kind: AckKind::Quiescence,
            })
        } else {
            self.reply_error(Some(q.id), MbtError::QuiescenceUnexpected)
        }
    }

    /// Expires early-set entries already due as of `now`, without waiting for
    /// the next `service_timers` pass. Used right after inserting a fresh
    /// entry, so `early_output_timeout_ms = 0` resolves within the same
    /// message rather than the next loop iteration.
    fn sweep_expired(&mut self, now: Instant) -> Result<(), MbtError> {
        for entry in self.early.take_expired(now) {
            self.send(ToolMessage::Error(ErrorMessage {
                in_reply_to: Some(entry.id),
                code: ErrorCode::OutputUnprocessed,
                message: format!(
                    "output `{}` was not matched within early_output_timeout_ms ({} ms)",
                    describe_multi_action(&entry.key.as_wire()),
                    self.config.early_output_timeout_ms
                ),
            }))?;
        }
        Ok(())
    }

    /// Re-evaluates the early set against the current state set after every
    /// accepted observation: on the first entry (in insertion order) whose
    /// output is now enabled, applies it and restarts the scan — one match
    /// may enable another — until a full scan finds none.
    fn reevaluate_early(&mut self) -> Result<(), MbtError> {
        let depth = self.config.tau_closure_depth;
        loop {
            let before = self.early.len();

            let mut matched_key = None;
            for entry in self.early.iter() {
                if self.model.accept_output(&entry.key, depth)? {
                    matched_key = Some(entry.key.clone());
                    break;
                }
            }

            let Some(key) = matched_key else { break };
            let removed = self
                .early
                .take_matching(&key)
                .expect("the key that just matched must still be present in the early set");
            self.send(ToolMessage::Ack {
                in_reply_to: removed.id,
                kind: AckKind::Output,
            })?;

            debug_assert!(
                self.early.len() < before,
                "reevaluate_early must strictly shrink the early set"
            );
        }
        Ok(())
    }

    /// Sends `error` for a frame-level decode failure, echoing `in_reply_to`
    /// where recovered, then closes: `malformed_message` and `unknown_type`
    /// are both fatal per the protocol.
    fn reply_decode_error(&mut self, err: DecodeError) -> Result<(), MbtError> {
        log::warn!("{}", err.message);
        self.send(ToolMessage::Error(ErrorMessage {
            in_reply_to: err.in_reply_to,
            code: err.code,
            message: err.message,
        }))?;
        self.begin_close(None)
    }

    /// Sends `error` for `err`, then closes the connection iff
    /// [`MbtError::is_fatal`] says this error code requires it.
    fn reply_error(&mut self, in_reply_to: Option<MessageId>, err: MbtError) -> Result<(), MbtError> {
        log::warn!("{err}");
        let code = err.code();
        let message = err.to_string();
        let fatal = err.is_fatal();
        self.send(ToolMessage::Error(ErrorMessage {
            in_reply_to,
            code,
            message,
        }))?;
        if fatal {
            self.begin_close(None)?;
        }
        Ok(())
    }

    /// Begins a graceful shutdown: sends `close`, closes the WebSocket, and
    /// drains inbound frames until the peer confirms or a short grace period
    /// elapses (a non-responding peer must not hang the tool).
    fn begin_close(&mut self, reason: Option<String>) -> Result<(), MbtError> {
        if self.phase == SessionPhase::Closing {
            return Ok(());
        }
        self.phase = SessionPhase::Closing;
        if let Some(reason) = &reason {
            log::info!("Closing the session: {reason}");
        }
        // If the peer already sent a WebSocket-level close frame (this fires
        // from the `Message::Close` arm in `run`), tungstenite has already
        // queued its own close reply and taken the connection out of
        // `Active`; writing anything else, including our own `close`
        // message, would fail with `SendAfterClosing`.
        if self.socket.can_write() {
            self.send(ToolMessage::Close(Close { reason }))?;
        }
        self.socket.close(None)?;

        let _ = self.socket.get_mut().set_read_deadline(Some(Duration::from_millis(500)));
        while self.socket.read().is_ok() {}
        Ok(())
    }

    /// Reports a fatal internal failure and tears down the connection
    /// without propagating further send errors — the connection is already
    /// in a bad enough state that a failed send here is not actionable.
    fn fail(&mut self, err: MbtError) -> Result<(), MbtError> {
        log::error!("{err}");
        let _ = self.send(ToolMessage::Error(ErrorMessage {
            in_reply_to: None,
            code: err.code(),
            message: err.to_string(),
        }));
        self.phase = SessionPhase::Closing;
        let _ = self.socket.close(None);
        Ok(())
    }
}

/// Runs a full MBT session over `socket`: sends `hello` and processes
/// messages until the connection closes.
pub fn run_session<S: Read + Write + ReadDeadline>(
    socket: WebSocket<S>,
    model: ModelState,
    limits: SessionLimits,
    hello: ToolMessage,
) -> Result<(), MbtError> {
    MbtSession::new(socket, model, limits, hello).run()
}

/// `now + ms`, or `None` if `ms == 0` disables the timer. Shared by
/// `heartbeat_interval_ms` (the tool's own send schedule) and
/// `heartbeat_timeout_ms` (the peer-timeout deadline) — both are "fire after
/// this long of silence" timers that the same zero-disables convention
/// applies to.
fn deadline_after(ms: u64, now: Instant) -> Option<Instant> {
    (ms != 0).then(|| now + Duration::from_millis(ms))
}

/// The nearest of the next heartbeat send, the peer deadline, and the
/// earliest pending early-set expiry, considering only those that are
/// enabled/present — or `None` if none of the three apply. A free function
/// over plain `Instant`s so the deadline-selection logic is testable without
/// a live socket.
fn next_deadline(
    next_heartbeat_send: Option<Instant>,
    peer_deadline: Option<Instant>,
    earliest_early: Option<Instant>,
) -> Option<Instant> {
    [next_heartbeat_send, peer_deadline, earliest_early].into_iter().flatten().min()
}

/// The `major.minor` prefix of a `major.minor[.patch]` version string, used
/// to compare protocol versions per the protocol's compatibility rule.
fn major_minor(version: &str) -> &str {
    match version.match_indices('.').nth(1) {
        Some((idx, _)) => &version[..idx],
        None => version,
    }
}

fn message_kind(msg: &AdapterMessage) -> &'static str {
    match msg {
        AdapterMessage::Hello(_) => "hello",
        AdapterMessage::Heartbeat(_) => "heartbeat",
        AdapterMessage::Close(_) => "close",
        AdapterMessage::Reset(_) => "reset",
        AdapterMessage::GetEnabled(_) => "get_enabled",
        AdapterMessage::Input(_) => "input",
        AdapterMessage::Output(_) => "output",
        AdapterMessage::Quiescence(_) => "quiescence",
    }
}

fn message_id(msg: &AdapterMessage) -> Option<MessageId> {
    match msg {
        AdapterMessage::Reset(r) => Some(r.id),
        AdapterMessage::GetEnabled(g) => Some(g.id),
        AdapterMessage::Input(o) | AdapterMessage::Output(o) => Some(o.id),
        AdapterMessage::Quiescence(q) => Some(q.id),
        AdapterMessage::Hello(_) | AdapterMessage::Heartbeat(_) | AdapterMessage::Close(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::Instant;

    use super::major_minor;
    use super::next_deadline;

    #[test]
    fn next_deadline_picks_the_earliest_of_all_three() {
        let base = Instant::now();
        let heartbeat = base + Duration::from_millis(100);
        let peer = base + Duration::from_millis(50);
        let early = Some(base + Duration::from_millis(200));
        assert_eq!(next_deadline(Some(heartbeat), Some(peer), early), Some(peer));
    }

    #[test]
    fn next_deadline_prefers_early_set_when_earliest() {
        let base = Instant::now();
        let heartbeat = base + Duration::from_millis(100);
        let peer = base + Duration::from_millis(200);
        let early = base + Duration::from_millis(10);
        assert_eq!(next_deadline(Some(heartbeat), Some(peer), Some(early)), Some(early));
    }

    #[test]
    fn next_deadline_ignores_absent_early_set() {
        let base = Instant::now();
        let heartbeat = base + Duration::from_millis(100);
        let peer = base + Duration::from_millis(200);
        assert_eq!(next_deadline(Some(heartbeat), Some(peer), None), Some(heartbeat));
    }

    #[test]
    fn next_deadline_ignores_disabled_peer_timeout() {
        let base = Instant::now();
        let heartbeat = base + Duration::from_millis(100);
        assert_eq!(next_deadline(Some(heartbeat), None, None), Some(heartbeat));
    }

    #[test]
    fn next_deadline_is_none_when_everything_disabled() {
        assert_eq!(next_deadline(None, None, None), None);
    }

    #[test]
    fn major_minor_compares_ignoring_patch() {
        assert_eq!(major_minor("0.2"), major_minor("0.2.1"));
        assert_ne!(major_minor("0.2"), major_minor("0.3"));
        assert_ne!(major_minor("0.2"), major_minor("1.2"));
    }
}
