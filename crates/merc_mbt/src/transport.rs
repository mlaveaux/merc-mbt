use std::io::Read;
use std::io::Write;
use std::net::TcpStream;

use tungstenite::Message;
use tungstenite::WebSocket;
use tungstenite::stream::MaybeTlsStream;

use crate::error::MbtError;
use crate::protocol::ToolMessage;

/// Connects to the adapter's WebSocket endpoint in blocking mode.
///
/// `url` is typically `ws://host:port/path`. A `wss://` URL fails here with
/// a clear [`MbtError::Transport`] unless this crate later gains a TLS
/// feature (an open question in the implementation plan) — this crate
/// intentionally does not enable one yet, since the protocol's examples
/// only ever show `ws://`.
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
