//! The interactive loop: read a [`Command`] from the user, turn the
//! request-shaped ones into an [`AdapterMessage`], send it, and print
//! whatever the tool sends back.

use std::io::BufRead;
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

use merc_mbt::AdapterHello;
use merc_mbt::AdapterMessage;
use merc_mbt::Close;
use merc_mbt::GetEnabled;
use merc_mbt::Heartbeat;
use merc_mbt::MessageId;
use merc_mbt::Observation;
use merc_mbt::PROTOCOL_VERSION;
use merc_mbt::PeerInfo;
use merc_mbt::QuiescenceReport;
use merc_mbt::Reset;
use merc_mbt::SessionConfig;
use merc_tools::Version;
use merc_mbt::SerializableAction;
use tungstenite::Message;
use tungstenite::WebSocket;

use crate::command::Command;
use crate::command::parse;
use crate::error::AdapterError;

/// How long a request-shaped command waits for the tool's reply before
/// giving up and returning to the prompt; generous enough that it's never
/// the bottleneck in a human-paced session.
const AUTO_RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `poll` waits before reporting nothing pending; short, since the
/// whole point of `poll` is a non-blocking check.
const POLL_TIMEOUT: Duration = Duration::from_millis(200);

/// Assigns request ids (`1`, `2`, ...), one per request-shaped command sent,
/// so the tool's `in_reply_to` can be matched back to what was asked.
struct IdGenerator(MessageId);

impl IdGenerator {
    fn next(&mut self) -> MessageId {
        self.0 += 1;
        self.0
    }
}

/// Drives the REPL to completion: reads commands from `input` until `quit`
/// or EOF, sending/receiving over `socket` and writing all output to
/// `output`.
pub fn run<B: BufRead, W: Write>(
    mut socket: WebSocket<TcpStream>,
    mut input: B,
    output: &mut W,
) -> Result<(), AdapterError> {
    writeln!(
        output,
        "adapter connected. Type `help` for the command list, `quit` to exit."
    )?;

    let mut ids = IdGenerator(0);
    let mut line = String::new();
    loop {
        write!(output, "> ")?;
        output.flush()?;

        line.clear();
        if input.read_line(&mut line)? == 0 {
            writeln!(output)?;
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let command = match parse(trimmed) {
            Ok(command) => command,
            Err(message) => {
                // `message` is already fully formatted (clap's own errors
                // read as "error: ..." plus usage text; `parse`'s own
                // errors are written the same way), so it's printed as-is
                // rather than wrapped again.
                writeln!(output, "{message}")?;
                continue;
            }
        };

        let outcome = match command {
            Command::Help => {
                print_help(output)?;
                None
            }
            Command::Quit => break,
            Command::Recv => Some((
                try_print_one(&mut socket, output, AUTO_RECV_TIMEOUT)?,
                format!("(no frame arrived within {AUTO_RECV_TIMEOUT:?})"),
            )),
            Command::Poll => Some((
                try_print_one(&mut socket, output, POLL_TIMEOUT)?,
                "(nothing pending)".to_string(),
            )),
            Command::Raw(text) => {
                // No auto-recv here, unlike the typed commands below: `raw`
                // exists to send purposely-malformed frames, which have no
                // well-defined reply to wait for. Use `recv`/`poll`
                // afterwards to check for one.
                send_text(&mut socket, &text)?;
                writeln!(output, "-> {text}")?;
                None
            }
            other => {
                let message = to_message(other, &mut ids);
                let text = serde_json::to_string_pretty(&message)?;
                send_text(&mut socket, &text)?;
                writeln!(output, "-> {text}")?;
                Some((
                    try_print_one(&mut socket, output, AUTO_RECV_TIMEOUT)?,
                    format!("(no frame arrived within {AUTO_RECV_TIMEOUT:?})"),
                ))
            }
        };

        match outcome {
            Some((RecvOutcome::Timeout, message)) => writeln!(output, "{message}")?,
            Some((RecvOutcome::Closed, _)) => {
                writeln!(output, "(connection closed)")?;
                break;
            }
            Some((RecvOutcome::Frame, _)) | None => {}
        }
    }

    let _ = socket.close(None);
    Ok(())
}

/// Builds the `AdapterMessage` for every [`Command`] variant that represents
/// one. Only called for those variants (`Help`/`Quit`/`Recv`/`Poll`/`Raw`
/// are handled directly in [`run`]), so the `unreachable!()` never fires.
fn to_message(command: Command, ids: &mut IdGenerator) -> AdapterMessage {
    match command {
        Command::Hello { protocol_version } => AdapterMessage::Hello(AdapterHello {
            role: "adapter".to_string(),
            protocol_version: protocol_version.unwrap_or_else(|| PROTOCOL_VERSION.to_string()),
            adapter: Some(PeerInfo {
                name: "merc-adapter".to_string(),
                version: Version.to_string(),
            }),
            // A human at the keyboard paces far slower than any fixed
            // heartbeat interval and has no reason to send `heartbeat`
            // explicitly, so disable the tool's peer-timeout check
            // (`heartbeat_timeout_ms: 0`) rather than get disconnected
            // mid-thought. Also disable the tool's own automatic sending
            // (`heartbeat_interval_ms: 0`): otherwise an unsolicited
            // heartbeat can land between a command and its reply, and every
            // `recv`/`poll` would have to loop to drain it off the socket.
            config: SessionConfig {
                heartbeat_interval_ms: 0,
                heartbeat_timeout_ms: 0,
                ..SessionConfig::default()
            },
        }),
        Command::Input { name, args } => AdapterMessage::Input(Observation {
            id: ids.next(),
            multi_action: vec![SerializableAction { name, args }],
        }),
        Command::Output { name, args } => AdapterMessage::Output(Observation {
            id: ids.next(),
            multi_action: vec![SerializableAction { name, args }],
        }),
        Command::Quiescence => AdapterMessage::Quiescence(QuiescenceReport { id: ids.next() }),
        Command::GetEnabled => AdapterMessage::GetEnabled(GetEnabled { id: ids.next() }),
        Command::Reset => AdapterMessage::Reset(Reset { id: ids.next() }),
        Command::Heartbeat => AdapterMessage::Heartbeat(Heartbeat { seq: None }),
        Command::Close { reason } => AdapterMessage::Close(Close {
            reason: if reason.is_empty() { None } else { Some(reason.join(" ")) },
        }),
        Command::Help | Command::Quit | Command::Recv | Command::Poll | Command::Raw(_) => {
            unreachable!("handled directly in `run`")
        }
    }
}

fn send_text(socket: &mut WebSocket<TcpStream>, text: &str) -> Result<(), AdapterError> {
    Ok(socket.send(Message::text(text))?)
}

/// What [`try_print_one`] found while waiting for the next frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecvOutcome {
    /// A frame arrived and was printed.
    Frame,
    /// Nothing arrived within the timeout.
    Timeout,
    /// The peer closed the connection.
    Closed,
}

/// Waits up to `timeout` for the next frame and prints it (pretty-printed
/// JSON when it parses as such, the raw text otherwise), or reports why
/// nothing was printed.
///
/// Never tears down the socket on a protocol-level surprise (a non-text
/// frame, unparseable JSON): the entire point of this tool is to show
/// exactly what arrived, including when that's a bug in the peer. A closed
/// connection is likewise reported rather than propagated as an error, since
/// the tool closing the connection is an expected, not exceptional, outcome.
fn try_print_one(
    socket: &mut WebSocket<TcpStream>,
    output: &mut impl Write,
    timeout: Duration,
) -> Result<RecvOutcome, AdapterError> {
    socket.get_ref().set_read_timeout(Some(timeout))?;
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                print_frame(output, &text)?;
                return Ok(RecvOutcome::Frame);
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            Ok(other) => {
                writeln!(output, "<- (non-text frame: {other:?})")?;
                return Ok(RecvOutcome::Frame);
            }
            Err(tungstenite::Error::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(RecvOutcome::Timeout);
            }
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                return Ok(RecvOutcome::Closed);
            }
            Err(err) => return Err(err.into()),
        }
    }
}

fn print_frame(output: &mut impl Write, text: &str) -> std::io::Result<()> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => writeln!(
            output,
            "<- {}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string())
        ),
        Err(_) => writeln!(output, "<- {text}  (not valid JSON)"),
    }
}

fn print_help(output: &mut impl Write) -> std::io::Result<()> {
    writeln!(
        output,
        "\
hello [protocol_version]      send `hello` (role=adapter); defaults to this tool's own version
input <name> [arg...]         send `input` with a single-action multi-action
output <name> [arg...]        send `output` with a single-action multi-action
quiescence                    send `quiescence`
get_enabled                   send `get_enabled`
reset                         send `reset`
heartbeat                     send `heartbeat`
close [reason...]             send `close`
raw <json>                    send the given text verbatim, bypassing validation (no auto-recv; follow with `recv`/`poll`)
recv                          block (up to {:?}) for the next frame
poll                          check once, without blocking, for a pending frame
help                          show this message
quit                          close the connection and exit",
        AUTO_RECV_TIMEOUT
    )
}
