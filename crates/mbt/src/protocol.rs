use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::action::SerializableMultiAction;
use crate::error::ErrorCode;

/// The protocol version this tool speaks.
pub const PROTOCOL_VERSION: &str = "0.2";

/// A message received from the adapter, dispatched on the envelope's `type`
/// field. Unknown top-level fields on any variant are ignored, per the
/// protocol's forward-compatibility rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterMessage {
    Hello(AdapterHello),
    Heartbeat(Heartbeat),
    Close(Close),
    Reset(Reset),
    GetEnabled(GetEnabled),
    Input(Observation),
    Output(Observation),
    Quiescence(QuiescenceReport),
}

/// A message sent to the adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolMessage {
    Hello(ToolHello),
    Heartbeat(Heartbeat),
    Close(Close),
    ResetAck { in_reply_to: String },
    Enabled(Enabled),
    Ack { in_reply_to: String, kind: AckKind },
    Warning(Warning),
    Error(ErrorMessage),
}

/// Identifies a peer by name and version, per `hello`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub name: String,
    pub version: String,
}

/// The LPS identification the MBT tool reports in its `hello`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LpsInfo {
    pub identifier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// `hello` sent by the adapter, carrying the authoritative session
/// configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterHello {
    pub role: String,
    pub protocol_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<PeerInfo>,
    #[serde(default)]
    pub config: SessionConfig,
}

/// `hello` sent by the MBT tool at the start of the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolHello {
    pub role: String,
    pub protocol_version: String,
    pub tool: PeerInfo,
    pub lps: LpsInfo,
}

/// Session configuration carried in the adapter's `hello`. Every field has a
/// protocol-defined default, used both when the adapter omits `config`
/// entirely and when it omits individual fields within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_tau_closure_depth")]
    pub tau_closure_depth: usize,
    #[serde(default = "default_early_output_timeout_ms")]
    pub early_output_timeout_ms: u64,
    /// How often the tool sends something (a `heartbeat`, or any other
    /// message, which counts just as well) to the adapter absent other
    /// traffic. `0` disables the tool's automatic sending entirely — useful
    /// for a human-driven adapter (e.g. the debug REPL), which would
    /// otherwise have to keep draining unsolicited heartbeat frames off the
    /// socket between commands.
    #[serde(default = "default_heartbeat_interval_ms")]
    pub heartbeat_interval_ms: u64,
    /// How long the tool waits without hearing anything from the adapter
    /// before declaring it lost and closing the session. `0` disables the
    /// check entirely — useful for a human-driven adapter (e.g. the debug
    /// REPL), which has no business pace to heartbeat on.
    #[serde(default = "default_heartbeat_timeout_ms")]
    pub heartbeat_timeout_ms: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            tau_closure_depth: default_tau_closure_depth(),
            early_output_timeout_ms: default_early_output_timeout_ms(),
            heartbeat_interval_ms: default_heartbeat_interval_ms(),
            heartbeat_timeout_ms: default_heartbeat_timeout_ms(),
        }
    }
}

fn default_tau_closure_depth() -> usize {
    1000
}

fn default_early_output_timeout_ms() -> u64 {
    1000
}

fn default_heartbeat_interval_ms() -> u64 {
    5000
}

fn default_heartbeat_timeout_ms() -> u64 {
    15000
}

/// `heartbeat`, sendable by either peer at any time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Heartbeat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
}

/// `close`, initiating a graceful shutdown; a present `reason` doubles as the
/// abort signal (there is no separate abort message).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Close {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `reset`, sent by the adapter to rewind the tool to the initial state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reset {
    pub id: String,
}

/// `get_enabled`, requesting the enabled set for the current state set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetEnabled {
    pub id: String,
}

/// The payload shared by `input` and `output`: a reported multi-action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub id: String,
    pub multi_action: SerializableMultiAction,
}

/// `quiescence`, reporting that the adapter's silence timer has expired.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuiescenceReport {
    pub id: String,
}

/// `enabled`, the reply to `get_enabled`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enabled {
    pub in_reply_to: String,
    pub inputs: Vec<SerializableMultiAction>,
    pub outputs: Vec<SerializableMultiAction>,
    pub quiescence: bool,
}

/// The observation kind an `ack` confirms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckKind {
    Input,
    Output,
    Quiescence,
}

/// The `warning.code` values the tool can emit. Currently a single code:
/// an unexpected output was held in the early set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    OutputEarly,
}

/// `warning`, emitted when an output reaches the head of the queue
/// unexpected and is held in the early set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    pub in_reply_to: String,
    pub code: WarningCode,
    pub message: String,
}

/// `error`. `in_reply_to` is omitted on the wire for errors not tied to a
/// specific request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    pub code: ErrorCode,
    pub message: String,
}

/// The message-type discriminators recognised on frames from the adapter,
/// used to tell an unknown `type` apart from an otherwise malformed frame.
const KNOWN_TYPES: &[&str] = &[
    "hello",
    "heartbeat",
    "close",
    "reset",
    "get_enabled",
    "input",
    "output",
    "quiescence",
];

/// A frame-level decoding failure, carrying enough information to reply with
/// the protocol's `error` message.
#[derive(Debug, Clone)]
pub struct DecodeError {
    pub code: ErrorCode,
    pub in_reply_to: Option<String>,
    pub message: String,
}

/// Decodes one text frame from the adapter into a typed [`AdapterMessage`].
///
/// This is a deliberate two-stage parse rather than a single
/// `serde_json::from_str::<AdapterMessage>` call: an internally-tagged enum
/// reports an unrecognised `type` discriminator as an ordinary
/// deserialisation failure, indistinguishable from a malformed frame, but
/// the protocol requires `unknown_type` and `malformed_message` to be
/// reported as distinct error codes, and requires echoing `in_reply_to`
/// wherever an `id` can be recovered — including from frames that fail to
/// decode further.
pub fn decode_frame(text: &str) -> Result<AdapterMessage, DecodeError> {
    let value: Value = serde_json::from_str(text).map_err(|err| DecodeError {
        code: ErrorCode::MalformedMessage,
        in_reply_to: None,
        message: format!("frame is not valid JSON: {err}"),
    })?;

    let Value::Object(obj) = &value else {
        return Err(DecodeError {
            code: ErrorCode::MalformedMessage,
            in_reply_to: None,
            message: "frame is not a JSON object".to_string(),
        });
    };

    // Recovered eagerly so every later error in this function can carry it.
    let in_reply_to = obj.get("id").and_then(Value::as_str).map(str::to_string);

    let Some(type_field) = obj.get("type").and_then(Value::as_str) else {
        return Err(DecodeError {
            code: ErrorCode::MalformedMessage,
            in_reply_to,
            message: "missing or non-string `type` field".to_string(),
        });
    };

    if !KNOWN_TYPES.contains(&type_field) {
        return Err(DecodeError {
            code: ErrorCode::UnknownType,
            in_reply_to,
            message: format!("unknown message type `{type_field}`"),
        });
    }
    let type_field = type_field.to_string();

    serde_json::from_value(value).map_err(|err| DecodeError {
        code: ErrorCode::MalformedMessage,
        in_reply_to,
        message: format!("malformed `{type_field}` message: {err}"),
    })
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::AdapterMessage;
    use super::SessionConfig;
    use super::decode_frame;
    use crate::error::ErrorCode;

    #[test]
    fn config_defaults_when_absent() {
        let msg: AdapterMessage =
            serde_json::from_str(r#"{ "type": "hello", "role": "adapter", "protocol_version": "0.2" }"#).unwrap();
        let AdapterMessage::Hello(hello) = msg else {
            panic!("expected hello");
        };
        assert_eq!(hello.config, SessionConfig::default());
        assert_eq!(hello.config.tau_closure_depth, 1000);
        assert_eq!(hello.config.early_output_timeout_ms, 1000);
        assert_eq!(hello.config.heartbeat_interval_ms, 5000);
        assert_eq!(hello.config.heartbeat_timeout_ms, 15000);
    }

    #[test]
    fn config_defaults_per_missing_field() {
        let msg: AdapterMessage = serde_json::from_str(
            r#"{ "type": "hello", "role": "adapter", "protocol_version": "0.2",
                 "config": { "tau_closure_depth": 5 } }"#,
        )
        .unwrap();
        let AdapterMessage::Hello(hello) = msg else {
            panic!("expected hello");
        };
        assert_eq!(hello.config.tau_closure_depth, 5);
        assert_eq!(hello.config.heartbeat_interval_ms, 5000);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let result = decode_frame(r#"{ "type": "heartbeat", "seq": 3, "surprise": true }"#);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test_case("{", ErrorCode::MalformedMessage, None; "not json")]
    #[test_case("[1,2]", ErrorCode::MalformedMessage, None; "not an object")]
    #[test_case(r#"{"id":"x"}"#, ErrorCode::MalformedMessage, Some("x"); "missing type")]
    #[test_case(r#"{"type":"nope","id":"x"}"#, ErrorCode::UnknownType, Some("x"); "unknown type")]
    #[test_case(r#"{"type":"input","id":"i-1"}"#, ErrorCode::MalformedMessage, Some("i-1"); "missing multi_action")]
    fn decode_frame_error_cases(frame: &str, expected_code: ErrorCode, expected_in_reply_to: Option<&str>) {
        let err = decode_frame(frame).expect_err("expected a decode error");
        assert_eq!(err.code, expected_code);
        assert_eq!(err.in_reply_to.as_deref(), expected_in_reply_to);
    }

    #[test]
    fn enabled_reply_matches_spec_example() {
        use crate::action::SerializableAction;
        use crate::protocol::Enabled;
        use crate::protocol::ToolMessage;

        let msg = ToolMessage::Enabled(Enabled {
            in_reply_to: "q-1".to_string(),
            inputs: vec![
                vec![SerializableAction {
                    name: "login".to_string(),
                    args: vec!["3".to_string()],
                }],
                vec![SerializableAction {
                    name: "read".to_string(),
                    args: vec!["0".to_string()],
                }],
            ],
            outputs: vec![vec![SerializableAction {
                name: "ack".to_string(),
                args: vec!["3".to_string()],
            }]],
            quiescence: false,
        });

        let json: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "enabled");
        assert_eq!(json["in_reply_to"], "q-1");
        assert_eq!(json["inputs"][0][0]["name"], "login");
        assert_eq!(json["inputs"][0][0]["args"][0], "3");
        assert_eq!(json["outputs"][0][0]["name"], "ack");
        assert_eq!(json["quiescence"], false);
    }

    #[test]
    fn error_message_omits_in_reply_to_when_absent() {
        use crate::error::ErrorCode as Code;
        use crate::protocol::ErrorMessage;
        use crate::protocol::ToolMessage;

        let msg = ToolMessage::Error(ErrorMessage {
            in_reply_to: None,
            code: Code::NotReady,
            message: "get_enabled sent before hello".to_string(),
        });
        let json = serde_json::to_value(&msg).unwrap();
        assert!(json.get("in_reply_to").is_none());
        assert_eq!(json["code"], "not_ready");
    }
}
