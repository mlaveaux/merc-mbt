use std::io;
use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

use tungstenite::Message;
use tungstenite::WebSocket;
use tungstenite::stream::MaybeTlsStream;

use crate::error::MbtError;
use crate::protocol::ToolMessage;

/// Bounds the next blocking `WebSocket::read()` so the event loop can service
/// its timers (heartbeats, the peer deadline, early-set expiries) without a
/// dedicated thread.
///
/// `std::net::TcpStream::set_read_timeout(Some(Duration::ZERO))` is rejected
/// with `InvalidInput`, so callers floor `timeout` above zero themselves; this
/// trait does not re-check that.
pub trait ReadDeadline {
    fn set_read_deadline(&mut self, timeout: Duration) -> io::Result<()>;
}

impl ReadDeadline for TcpStream {
    fn set_read_deadline(&mut self, timeout: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }
}

impl ReadDeadline for MaybeTlsStream<TcpStream> {
    fn set_read_deadline(&mut self, timeout: Duration) -> io::Result<()> {
        match self {
            MaybeTlsStream::Plain(stream) => stream.set_read_deadline(timeout),
            // `rustls::StreamOwned::sock` is the underlying transport (a
            // `TcpStream` here); the deadline applies to the raw socket the
            // same way it does for the plain case, independent of the TLS
            // session layered on top of it.
            MaybeTlsStream::Rustls(stream) => stream.sock.set_read_deadline(timeout),
            // `MaybeTlsStream` is `#[non_exhaustive]`; kept exhaustive against
            // a TLS backend other than rustls (e.g. `native-tls`), which this
            // crate does not enable.
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "read deadlines are only supported on a plain or rustls-backed stream",
            )),
        }
    }
}

/// Connects to the adapter's WebSocket endpoint in blocking mode.
///
/// `url` is `ws://host:port/path` or `wss://host:port/path`; TLS is handled
/// by `rustls` (via tungstenite's `rustls-tls-webpki-roots` feature), using
/// the bundled Mozilla root store rather than the OS trust store.
pub fn connect_adapter(url: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, MbtError> {
    let (socket, response) = tungstenite::connect(url)?;
    log::debug!("Connected to `{url}` (HTTP status {}).", response.status());
    Ok(socket)
}

/// Serialises `message` and sends it as a single WebSocket text frame.
///
/// Generic over the underlying stream so the same helper drives both a real
/// adapter connection and an in-process mock adapter in tests.
pub fn send_message<S: Read + Write>(socket: &mut WebSocket<S>, message: &ToolMessage) -> Result<(), MbtError> {
    // `ToolMessage` is our own well-typed, fully populated structure, so
    // serialisation cannot fail in practice; a panic here would indicate a
    // bug in this crate rather than a runtime condition to recover from.
    let text = serde_json::to_string(message).expect("ToolMessage always serialises to JSON");
    socket.send(Message::text(text))?;
    Ok(())
}

/// Reads the next raw WebSocket message from `socket`, blocking until one
/// arrives (subject to whatever read deadline the caller has configured on
/// the underlying stream).
pub fn read_frame<S: Read + Write>(socket: &mut WebSocket<S>) -> Result<Message, MbtError> {
    Ok(socket.read()?)
}

/// Closes the WebSocket connection.
pub fn close_session<S: Read + Write>(socket: &mut WebSocket<S>) -> Result<(), MbtError> {
    Ok(socket.close(None)?)
}
