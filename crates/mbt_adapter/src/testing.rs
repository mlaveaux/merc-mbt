//! A typed, programmatic stand-in for the human-driven REPL: scripts an
//! adapter-role conversation against a real `mbt` tool connection, using the
//! same typed [`AdapterMessage`]/[`ToolMessage`] wire types the REPL sends,
//! instead of hand-written JSON text.
//!
//! Exists so integration tests (in this crate's own `tests/`, or anyone
//! else's) can drive a scripted adapter without re-deriving the wire format
//! by hand.

use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::time::Duration;
use std::time::Instant;

use merc_mbt::AdapterMessage;
use merc_mbt::ToolMessage;
use tungstenite::Message;
use tungstenite::WebSocket;

use crate::error::AdapterError;

/// One accepted connection, playing the adapter role with typed messages.
pub struct MockAdapter {
    socket: WebSocket<TcpStream>,
    /// The total time [`expect`](Self::expect) waits for a non-heartbeat
    /// reply, across however many heartbeat/ping/pong frames arrive first —
    /// not a per-syscall bound, since a peer that keeps emitting those faster
    /// than this interval must still not be able to stall the wait forever.
    read_timeout: Duration,
}

impl MockAdapter {
    /// Binds a loopback listener on an OS-assigned port, returning the
    /// address to point the tool at and the listener to [`accept`](Self::accept)
    /// on — typically from a different thread than the one driving the
    /// tool, since [`merc_mbt::MbtSession::run`] blocks its caller.
    pub fn bind() -> Result<(SocketAddr, TcpListener), AdapterError> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        Ok((addr, listener))
    }

    /// Accepts the one connection `listener` will receive and completes the
    /// WebSocket handshake. `read_timeout` bounds every subsequent
    /// [`expect`](Self::expect) call, so a tool that never replies fails the
    /// test instead of hanging it.
    pub fn accept(listener: TcpListener, read_timeout: Duration) -> Result<Self, AdapterError> {
        let (stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(read_timeout))?;
        let socket = tungstenite::accept(stream).map_err(crate::error::handshake_error)?;
        Ok(Self { socket, read_timeout })
    }

    /// Sends one typed message to the tool.
    pub fn send(&mut self, message: AdapterMessage) -> Result<(), AdapterError> {
        let text = serde_json::to_string(&message)?;
        self.socket.send(Message::text(text))?;
        Ok(())
    }

    /// Reads the next non-heartbeat message from the tool, decoded as a
    /// typed [`ToolMessage`]. Bounded by `read_timeout` as a single overall
    /// deadline rather than per-frame: each `read()` call is re-armed with
    /// however much of the deadline remains, so a peer that keeps sending
    /// heartbeats/pings faster than `read_timeout` still times out instead of
    /// looping forever.
    pub fn expect(&mut self) -> Result<ToolMessage, AdapterError> {
        let deadline = Instant::now() + self.read_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AdapterError::Io(std::io::ErrorKind::TimedOut.into()));
            }
            self.socket.get_ref().set_read_timeout(Some(remaining))?;

            match self.socket.read()? {
                Message::Text(text) => {
                    let message: ToolMessage = serde_json::from_str(&text)?;
                    if matches!(message, ToolMessage::Heartbeat(_)) {
                        continue;
                    }
                    return Ok(message);
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected frame from the tool: {other:?}"),
            }
        }
    }

    /// Sends `text` verbatim as a single WebSocket text frame, bypassing
    /// `AdapterMessage` entirely — the escape hatch for a frame with no
    /// typed representation, such as a deliberately unrecognised `type`.
    pub fn send_raw(&mut self, text: &str) -> Result<(), AdapterError> {
        Ok(self.socket.send(Message::text(text))?)
    }

    /// Tears down the connection at the WebSocket level directly — a raw
    /// close frame, without an application-level `close` message first. For
    /// exercising how the tool reacts to a peer that disconnects without
    /// following the application protocol's own close handshake.
    pub fn close_raw(&mut self) -> Result<(), AdapterError> {
        Ok(self.socket.close(None)?)
    }

    /// Drains frames until the connection closes or a read errors out,
    /// giving the tool a chance to flush its own close frame before this
    /// side drops the TCP connection.
    pub fn drain(&mut self) {
        while self.socket.read().is_ok() {}
    }
}
