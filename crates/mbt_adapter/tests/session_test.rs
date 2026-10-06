//! Full-protocol scenarios against a typed, in-process mock adapter (see
//! [`merc_mbt_adapter::testing::MockAdapter`]), over a real WebSocket on a
//! loopback TCP socket.
//!
//! Gated on `MCRL2_PATH`, since a real [`ModelState`] needs a compiled LPS.
//!
//! Thread placement is a correctness requirement, not a stylistic one: the
//! [`MbtSession`] under test runs on the test's **main** thread, because the
//! mCRL2 types it owns (`ExplicitLinearProcessSpecification`,
//! `LearnSuccessorsContext`) are thread-affine (`PhantomUnsend`) and the
//! aterm pool they draw on is thread-local. The mock adapter thread handles
//! only typed protocol messages and sockets, and must never construct an
//! mCRL2 type.

use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use mcrl2::read_lps;
use merc_io::temp_dir;
use merc_io::traced_command;
use merc_lps::ExplicitLinearProcessSpecification;
use merc_mbt::AckKind;
use merc_mbt::AdapterHello;
use merc_mbt::AdapterMessage;
use merc_mbt::Close;
use merc_mbt::ErrorCode;
use merc_mbt::GetEnabled;
use merc_mbt::LpsInfo;
use merc_mbt::MbtSession;
use merc_mbt::ModelState;
use merc_mbt::Observation;
use merc_mbt::PROTOCOL_VERSION;
use merc_mbt::PeerInfo;
use merc_mbt::Reset;
use merc_mbt::SerializableAction;
use merc_mbt::SessionConfig;
use merc_mbt::SessionLimits;
use merc_mbt::ToolHello;
use merc_mbt::ToolMessage;
use merc_mbt::WarningCode;
use merc_mbt::connect_adapter;
use merc_mbt::parse_partition;
use merc_mbt_adapter::testing::MockAdapter;

/// A small model: `req` (input) enables `resp` (output); both loop.
const SPEC: &str = "
    act req, resp;
    proc P = req . resp . P;
    init P;
";
const PARTITION: &str = "
    input
      req;
    output
      resp;
";

/// Bounds every [`MockAdapter::expect`] call in these tests; generous enough
/// that it's never the bottleneck, short enough that a tool bug which drops
/// a reply fails the test instead of hanging it.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Compiles [`SPEC`] with `mcrl22lps` and builds a [`ModelState`], or `None`
/// if `MCRL2_PATH` is unset.
fn simple_model(name: &str) -> Option<ModelState> {
    let mcrl2_path = std::env::var("MCRL2_PATH").ok()?;
    let mcrl22lps = Path::new(&mcrl2_path).join("mcrl22lps");

    let dir = temp_dir(name).unwrap();
    let spec_path = dir.path().join("spec.mcrl2");
    let lps_path = dir.path().join("spec.lps");
    std::fs::write(&spec_path, SPEC).expect("Failed to write spec");

    let status =
        traced_command(Command::new(&mcrl22lps).arg(&spec_path).arg(&lps_path)).expect("Failed to execute mcrl22lps");
    assert!(status.success(), "mcrl22lps failed with status: {status}");

    let lps = read_lps(lps_path.to_str().unwrap()).expect("Failed to read LPS");
    let explicit = ExplicitLinearProcessSpecification::new(lps).expect("Failed to build explicit LPS");

    let partition = parse_partition(PARTITION.as_bytes()).expect("Failed to parse partition");
    partition
        .validate_against_lps(&explicit)
        .expect("Partition does not cover the LPS");

    Some(ModelState::new(explicit, partition, 0, 0))
}

/// Connects to `addr` and runs an [`MbtSession`] over `model` to completion
/// on the calling (main) thread.
fn run_tool(addr: SocketAddr, model: ModelState, limits: SessionLimits) {
    let socket = connect_adapter(&format!("ws://{addr}/mbt")).expect("Failed to connect to the mock adapter");

    let hello = ToolMessage::Hello(ToolHello {
        role: "mbt_tool".to_string(),
        protocol_version: PROTOCOL_VERSION.to_string(),
        tool: PeerInfo {
            name: "merc-mbt-test".to_string(),
            version: "0.0.0".to_string(),
        },
        lps: LpsInfo {
            identifier: "test".to_string(),
            hash: None,
        },
    });

    MbtSession::new(socket, model, limits, hello)
        .run()
        .expect("Session must run to completion without error");
}

/// The adapter's `hello` config, generous on timing so no spurious heartbeat
/// or peer-timeout fires mid-test.
fn adapter_hello(tau_closure_depth: usize, early_output_timeout_ms: u64) -> AdapterHello {
    AdapterHello {
        role: "adapter".to_string(),
        protocol_version: PROTOCOL_VERSION.to_string(),
        adapter: None,
        config: SessionConfig {
            tau_closure_depth,
            early_output_timeout_ms,
            heartbeat_interval_ms: 60_000,
            heartbeat_timeout_ms: 60_000,
        },
    }
}

fn multi_action(name: &str) -> Vec<SerializableAction> {
    vec![SerializableAction {
        name: name.to_string(),
        args: vec![],
    }]
}

fn default_limits() -> SessionLimits {
    SessionLimits {
        max_tau_closure_depth: 0,
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_handshake_and_get_enabled() {
    let Some(model) = simple_model("test_mbt_session_handshake") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));

        adapter.send(AdapterMessage::Hello(adapter_hello(20, 5_000))).unwrap();
        adapter
            .send(AdapterMessage::GetEnabled(GetEnabled { id: 1 }))
            .unwrap();

        let ToolMessage::Enabled(enabled) = adapter.expect().unwrap() else {
            panic!("expected an `enabled` reply");
        };
        assert_eq!(enabled.in_reply_to, 1);
        assert_eq!(enabled.inputs, vec![multi_action("req")]);
        assert_eq!(enabled.outputs, Vec::<Vec<SerializableAction>>::new());
        // Per the spec's quiescence formula (quantifies only over Act_out ∪
        // {τ}), a state with only an input enabled is quiescent — see
        // `test_mcrl2_quiescence_formula` in `model_test.rs`.
        assert!(enabled.quiescence);

        adapter.send(AdapterMessage::Close(Close { reason: None })).unwrap();
        adapter.drain();
    });

    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_get_enabled_before_hello_is_not_ready_and_survives() {
    let Some(model) = simple_model("test_mbt_session_not_ready") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));

        adapter.send(AdapterMessage::GetEnabled(GetEnabled { id: 2 })).unwrap();
        let ToolMessage::Error(error) = adapter.expect().unwrap() else {
            panic!("expected a `not_ready` error");
        };
        assert_eq!(error.code, ErrorCode::NotReady);
        assert_eq!(error.in_reply_to, Some(2));

        adapter.send(AdapterMessage::Hello(adapter_hello(20, 5_000))).unwrap();
        adapter
            .send(AdapterMessage::GetEnabled(GetEnabled { id: 1 }))
            .unwrap();
        let ToolMessage::Enabled(_) = adapter.expect().unwrap() else {
            panic!("the session must survive to answer the later `get_enabled`");
        };

        adapter.send(AdapterMessage::Close(Close { reason: None })).unwrap();
        adapter.drain();
    });

    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_unknown_type_closes_with_error() {
    let Some(model) = simple_model("test_mbt_session_unknown_type") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));
        adapter.send(AdapterMessage::Hello(adapter_hello(20, 5_000))).unwrap();

        // No `AdapterMessage` variant exists for an unrecognised `type` by
        // construction, so this one case keeps a raw frame.
        adapter.send_raw(r#"{"type": "not_a_real_type", "id": 99}"#).unwrap();
        let ToolMessage::Error(error) = adapter.expect().unwrap() else {
            panic!("expected an `unknown_type` error");
        };
        assert_eq!(error.code, ErrorCode::UnknownType);
        assert_eq!(error.in_reply_to, Some(99));

        let ToolMessage::Close(_) = adapter.expect().unwrap() else {
            panic!("expected the session to close after an unknown type");
        };
        adapter.drain();
    });

    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_protocol_mismatch_closes() {
    let Some(model) = simple_model("test_mbt_session_protocol_mismatch") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));

        adapter
            .send(AdapterMessage::Hello(AdapterHello {
                role: "adapter".to_string(),
                protocol_version: "0.1".to_string(),
                adapter: None,
                config: SessionConfig::default(),
            }))
            .unwrap();

        let ToolMessage::Error(error) = adapter.expect().unwrap() else {
            panic!("expected a `protocol_mismatch` error");
        };
        assert_eq!(error.code, ErrorCode::ProtocolMismatch);

        let ToolMessage::Close(_) = adapter.expect().unwrap() else {
            panic!("expected the session to close after a protocol mismatch");
        };
        adapter.drain();
    });

    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_tau_closure_depth_above_cap_is_rejected() {
    let Some(model) = simple_model("test_mbt_session_config_cap") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));

        adapter
            .send(AdapterMessage::Hello(adapter_hello(1_000_000, 5_000)))
            .unwrap();
        let ToolMessage::Error(error) = adapter.expect().unwrap() else {
            panic!("expected an `unsupported_config` error");
        };
        assert_eq!(error.code, ErrorCode::UnsupportedConfig);

        let ToolMessage::Close(_) = adapter.expect().unwrap() else {
            panic!("expected the session to close after an unsupported config");
        };
        adapter.drain();
    });

    run_tool(
        addr,
        model,
        SessionLimits {
            max_tau_closure_depth: 100,
        },
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_input_accepted_and_rejected() {
    let Some(model) = simple_model("test_mbt_session_input") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));
        adapter.send(AdapterMessage::Hello(adapter_hello(20, 5_000))).unwrap();

        // `resp` is never classified as an input, so this must be rejected
        // regardless of the current state set.
        adapter
            .send(AdapterMessage::Input(Observation {
                id: 1,
                multi_action: multi_action("resp"),
            }))
            .unwrap();
        let ToolMessage::Error(error) = adapter.expect().unwrap() else {
            panic!("expected an `input_not_enabled` error");
        };
        assert_eq!(error.code, ErrorCode::InputNotEnabled);
        assert_eq!(error.in_reply_to, Some(1));

        // `req` is enabled from the initial state.
        adapter
            .send(AdapterMessage::Input(Observation {
                id: 2,
                multi_action: multi_action("req"),
            }))
            .unwrap();
        let ToolMessage::Ack { in_reply_to, kind } = adapter.expect().unwrap() else {
            panic!("expected an `ack` for the accepted input");
        };
        assert_eq!(in_reply_to, 2);
        assert_eq!(kind, AckKind::Input);

        adapter.send(AdapterMessage::Close(Close { reason: None })).unwrap();
        adapter.drain();
    });

    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_output_early_then_enabling_input_acks_in_order() {
    let Some(model) = simple_model("test_mbt_session_early_output") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));
        adapter.send(AdapterMessage::Hello(adapter_hello(20, 5_000))).unwrap();

        // `resp` is not enabled until `req` has been accepted.
        adapter
            .send(AdapterMessage::Output(Observation {
                id: 1,
                multi_action: multi_action("resp"),
            }))
            .unwrap();
        let ToolMessage::Warning(warning) = adapter.expect().unwrap() else {
            panic!("expected a `warning` for the early output");
        };
        assert_eq!(warning.in_reply_to, 1);
        assert_eq!(warning.code, WarningCode::OutputEarly);

        adapter
            .send(AdapterMessage::Input(Observation {
                id: 2,
                multi_action: multi_action("req"),
            }))
            .unwrap();
        let ToolMessage::Ack {
            in_reply_to: first_ack,
            kind: first_kind,
        } = adapter.expect().unwrap()
        else {
            panic!("expected the input's own `ack` first");
        };
        assert_eq!(first_ack, 2);
        assert_eq!(first_kind, AckKind::Input);

        let ToolMessage::Ack {
            in_reply_to: second_ack,
            kind: second_kind,
        } = adapter.expect().unwrap()
        else {
            panic!("expected the early output's `ack` second");
        };
        assert_eq!(second_ack, 1, "the early output's ack must reference its own id");
        assert_eq!(second_kind, AckKind::Output);

        adapter.send(AdapterMessage::Close(Close { reason: None })).unwrap();
        adapter.drain();
    });

    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_reset_discards_pending_early_silently() {
    let Some(model) = simple_model("test_mbt_session_reset") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));
        adapter.send(AdapterMessage::Hello(adapter_hello(20, 5_000))).unwrap();

        adapter
            .send(AdapterMessage::Output(Observation {
                id: 1,
                multi_action: multi_action("resp"),
            }))
            .unwrap();
        let ToolMessage::Warning(warning) = adapter.expect().unwrap() else {
            panic!("expected a `warning` for the early output");
        };
        assert_eq!(warning.in_reply_to, 1);

        adapter.send(AdapterMessage::Reset(Reset { id: 2 })).unwrap();
        // If `reset` leaked a reply for the discarded early entry, it would
        // arrive here instead of the `reset_ack` — this panics with exactly
        // that frame's contents rather than silently passing.
        let ToolMessage::ResetAck { in_reply_to } = adapter.expect().unwrap() else {
            panic!("the discarded early entry must never be answered again");
        };
        assert_eq!(in_reply_to, 2);

        adapter
            .send(AdapterMessage::GetEnabled(GetEnabled { id: 1 }))
            .unwrap();
        let ToolMessage::Enabled(enabled) = adapter.expect().unwrap() else {
            panic!("expected an `enabled` reply");
        };
        assert_eq!(
            enabled.inputs,
            vec![multi_action("req")],
            "reset must return to the initial enabled set"
        );
        assert_eq!(enabled.outputs, Vec::<Vec<SerializableAction>>::new());

        adapter.send(AdapterMessage::Close(Close { reason: None })).unwrap();
        adapter.drain();
    });

    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_close_is_graceful() {
    let Some(model) = simple_model("test_mbt_session_close") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));
        adapter.send(AdapterMessage::Hello(adapter_hello(20, 5_000))).unwrap();
        adapter
            .send(AdapterMessage::Close(Close {
                reason: Some("test done".to_string()),
            }))
            .unwrap();
        adapter.drain();
    });

    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}

/// A peer that tears down the WebSocket directly (no application-level
/// `close` message first) must not make the tool try to write after the
/// connection is already closing — tungstenite rejects that as
/// `SendAfterClosing`.
#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_raw_websocket_close_is_graceful() {
    let Some(model) = simple_model("test_mbt_session_raw_close") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let (addr, listener) = MockAdapter::bind().unwrap();
    let handle = thread::spawn(move || {
        let mut adapter = MockAdapter::accept(listener, READ_TIMEOUT).unwrap();
        assert!(matches!(adapter.expect().unwrap(), ToolMessage::Hello(_)));
        adapter.send(AdapterMessage::Hello(adapter_hello(20, 5_000))).unwrap();
        adapter.close_raw().unwrap();
        adapter.drain();
    });

    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}
