/// Every failure mode the debug adapter can encounter.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tungstenite::Error),

    /// Should not happen for the typed `AdapterMessage` variants this crate
    /// builds, but can happen for a `raw` command whose text was valid UTF-8
    /// but not valid JSON.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Converts a `tungstenite::accept`/`accept_hdr` handshake failure into an
/// [`AdapterError::WebSocket`], preserving the underlying `tungstenite::Error`
/// instead of flattening it into a generic I/O error. `Interrupted` (the
/// handshake would block) cannot occur against the blocking streams this
/// crate uses, but is mapped to `WouldBlock` rather than panicking, since the
/// type is generic over any `Read + Write` role.
pub fn handshake_error<Role: tungstenite::handshake::HandshakeRole>(
    err: tungstenite::handshake::HandshakeError<Role>,
) -> AdapterError {
    match err {
        tungstenite::handshake::HandshakeError::Failure(err) => AdapterError::WebSocket(err),
        tungstenite::handshake::HandshakeError::Interrupted(_) => {
            AdapterError::WebSocket(tungstenite::Error::Io(std::io::ErrorKind::WouldBlock.into()))
        }
    }
}
