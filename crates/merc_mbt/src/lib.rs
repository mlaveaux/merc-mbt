//! Model-based testing for mCRL2 linear process specifications.
//!
//! Implements the MBT tool side of the mCRL2 MBT <-> Adapter protocol: a
//! synchronous WebSocket client that maintains a symbolic state set over an
//! LPS and checks IOCO conformance against a test adapter. See
//! `docs/merc-mbt-implementation-plan.md` at the workspace root for the
//! protocol summary and the phased implementation plan this crate follows.

pub mod action;
pub mod error;
pub mod partition;
pub mod protocol;
pub mod transport;

pub use action::MultiActionKey;
pub use action::WireAction;
pub use action::WireMultiAction;
pub use error::ErrorCode;
pub use error::MbtError;
pub use partition::ActionClass;
pub use partition::ActionPartition;
pub use partition::ActionPattern;
pub use partition::parse_partition;
pub use protocol::AckKind;
pub use protocol::AdapterHello;
pub use protocol::AdapterMessage;
pub use protocol::Close;
pub use protocol::DecodeError;
pub use protocol::Enabled;
pub use protocol::ErrorMessage;
pub use protocol::GetEnabled;
pub use protocol::Heartbeat;
pub use protocol::LpsInfo;
pub use protocol::Observation;
pub use protocol::PeerInfo;
pub use protocol::QuiescenceReport;
pub use protocol::Reset;
pub use protocol::SessionConfig;
pub use protocol::ToolHello;
pub use protocol::ToolMessage;
pub use protocol::Warning;
pub use protocol::WarningCode;
pub use protocol::decode_frame;
pub use transport::close_session;
pub use transport::connect_adapter;
pub use transport::read_frame;
pub use transport::send_message;
