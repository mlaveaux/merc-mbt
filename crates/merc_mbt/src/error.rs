use merc_utilities::MercError;
use serde::Deserialize;
use serde::Serialize;

/// The `error.code` values defined by the protocol.
///
/// Serialises to exactly the wire strings the protocol specifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    MalformedMessage,
    UnknownType,
    ProtocolMismatch,
    UnsupportedConfig,
    NotReady,
    InputNotEnabled,
    OutputUnprocessed,
    QuiescenceUnexpected,
    LpsUnavailable,
    Internal,
}

/// Every failure mode the MBT tool can encounter, from a malformed frame to
/// an mCRL2 model error.
///
/// This is a dedicated error type rather than [`MercError`] because the
/// error *identity* is a functional requirement here: the protocol's
/// `error.code` field is part of the wire contract, and `MercError` is a
/// type-erased catch-all with no discriminants to map back to a code.
#[derive(Debug, thiserror::Error)]
pub enum MbtError {
    #[error("malformed frame: {0}")]
    MalformedMessage(String),

    #[error("unknown message type `{0}`")]
    UnknownType(String),

    #[error("peer protocol version `{peer}`, expected `{expected}`")]
    ProtocolMismatch { peer: String, expected: &'static str },

    #[error("unsupported configuration: {0}")]
    UnsupportedConfig(String),

    #[error("received `{0}` before the handshake completed")]
    NotReady(&'static str),

    #[error("input `{0}` is not enabled from the current state set")]
    InputNotEnabled(String),

    #[error("early output `{0}` expired after {1} ms")]
    OutputUnprocessed(String, u64),

    #[error("quiescence reported while an output was expected")]
    QuiescenceUnexpected,

    #[error("could not load the LPS `{path}`: {cause}")]
    LpsUnavailable { path: String, cause: MercError },

    #[error("transport error: {0}")]
    Transport(#[from] tungstenite::Error),

    #[error("model error: {0}")]
    Model(MercError),
}

impl MbtError {
    /// The wire `error.code` this failure maps to.
    pub fn code(&self) -> ErrorCode {
        match self {
            MbtError::MalformedMessage(_) => ErrorCode::MalformedMessage,
            MbtError::UnknownType(_) => ErrorCode::UnknownType,
            MbtError::ProtocolMismatch { .. } => ErrorCode::ProtocolMismatch,
            MbtError::UnsupportedConfig(_) => ErrorCode::UnsupportedConfig,
            MbtError::NotReady(_) => ErrorCode::NotReady,
            MbtError::InputNotEnabled(_) => ErrorCode::InputNotEnabled,
            MbtError::OutputUnprocessed(_, _) => ErrorCode::OutputUnprocessed,
            MbtError::QuiescenceUnexpected => ErrorCode::QuiescenceUnexpected,
            MbtError::LpsUnavailable { .. } => ErrorCode::LpsUnavailable,
            MbtError::Transport(_) | MbtError::Model(_) => ErrorCode::Internal,
        }
    }

    /// Whether the connection must be closed after reporting this error, per
    /// the protocol's semantics for each error code.
    pub fn is_fatal(&self) -> bool {
        match self {
            MbtError::MalformedMessage(_)
            | MbtError::UnknownType(_)
            | MbtError::ProtocolMismatch { .. }
            | MbtError::UnsupportedConfig(_)
            | MbtError::LpsUnavailable { .. }
            | MbtError::Transport(_)
            | MbtError::Model(_) => true,
            MbtError::NotReady(_)
            | MbtError::InputNotEnabled(_)
            | MbtError::OutputUnprocessed(_, _)
            | MbtError::QuiescenceUnexpected => false,
        }
    }
}

// `MercError` does not implement `std::error::Error` (see `merc_utilities`),
// so it cannot be wrapped with `#[from]` (which requires `Error` for the
// implied `#[source]`). This hand-written conversion is the alternative the
// workspace convention calls for; it lets `?` turn a `MercError` from the
// model layer into `MbtError::Model` at call sites.
impl From<MercError> for MbtError {
    fn from(err: MercError) -> Self {
        MbtError::Model(err)
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorCode;
    use super::MbtError;

    #[test]
    fn every_variant_maps_to_its_wire_code() {
        assert_eq!(
            MbtError::MalformedMessage("x".into()).code(),
            ErrorCode::MalformedMessage
        );
        assert_eq!(MbtError::UnknownType("x".into()).code(), ErrorCode::UnknownType);
        assert_eq!(
            MbtError::ProtocolMismatch {
                peer: "0.1".into(),
                expected: "0.2"
            }
            .code(),
            ErrorCode::ProtocolMismatch
        );
        assert_eq!(
            MbtError::UnsupportedConfig("x".into()).code(),
            ErrorCode::UnsupportedConfig
        );
        assert_eq!(MbtError::NotReady("get_enabled").code(), ErrorCode::NotReady);
        assert_eq!(MbtError::InputNotEnabled("a".into()).code(), ErrorCode::InputNotEnabled);
        assert_eq!(
            MbtError::OutputUnprocessed("a".into(), 1000).code(),
            ErrorCode::OutputUnprocessed
        );
        assert_eq!(MbtError::QuiescenceUnexpected.code(), ErrorCode::QuiescenceUnexpected);
    }

    #[test]
    fn error_code_serialises_to_snake_case() {
        let json = serde_json::to_string(&ErrorCode::InputNotEnabled).unwrap();
        assert_eq!(json, "\"input_not_enabled\"");
        let json = serde_json::to_string(&ErrorCode::OutputUnprocessed).unwrap();
        assert_eq!(json, "\"output_unprocessed\"");
    }

    #[test]
    fn fatal_errors_close_the_connection() {
        assert!(MbtError::MalformedMessage("x".into()).is_fatal());
        assert!(MbtError::UnsupportedConfig("x".into()).is_fatal());
        assert!(!MbtError::NotReady("get_enabled").is_fatal());
        assert!(!MbtError::InputNotEnabled("a".into()).is_fatal());
    }
}
