//! Full-protocol scenarios against an in-process mock adapter, over a real
//! WebSocket on a loopback TCP socket.
//!
//! Gated on `MCRL2_PATH` like `tests/model_test.rs`, since a real
//! [`ModelState`] needs a compiled LPS.
//!
//! Thread placement is a correctness requirement, not a stylistic one: the
//! [`MbtSession`] under test runs on the test's **main** thread, because the
//! mCRL2 types it owns (`ExplicitLinearProcessSpecification`,
//! `LearnSuccessorsContext`) are thread-affine (`PhantomUnsend`) and the
//! aterm pool they draw on is thread-local. The mock adapter thread handles
//! only JSON and sockets and must never construct an mCRL2 type.

use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use mcrl2::read_lps;
use merc_io::temp_dir;
use merc_io::traced_command;
use merc_lps::ExplicitLinearProcessSpecification;
use merc_mbt::LpsInfo;
use merc_mbt::MbtSession;
use merc_mbt::ModelState;
use merc_mbt::PROTOCOL_VERSION;
use merc_mbt::PeerInfo;
use merc_mbt::SessionLimits;
use merc_mbt::ToolHello;
use merc_mbt::ToolMessage;
use merc_mbt::connect_adapter;
use merc_mbt::parse_partition;
use serde_json::Value;
use serde_json::json;
use tungstenite::Message;

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

    let partition = parse_partition(PARTITION).expect("Failed to parse partition");
    partition
        .validate_against_lps(&explicit)
        .expect("Partition does not cover the LPS");

    Some(ModelState::new(explicit, partition, 0, 0))
}

/// One step of a scripted adapter conversation.
enum Step {
    /// Sends a raw JSON value as a text frame.
    Send(Value),
    /// Reads the next non-heartbeat frame from the tool and asserts its
    /// `type`.
    Expect(&'static str),
}

/// Runs `script` against one accepted connection on a loopback listener,
/// returning the port to connect to and a handle yielding every frame
/// received from the tool (in arrival order, heartbeats included).
fn spawn_mock_adapter(script: Vec<Step>) -> (SocketAddr, JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();

        let mut received = Vec::new();
        for step in script {
            match step {
                Step::Send(value) => {
                    socket.send(Message::text(value.to_string())).unwrap();
                }
                Step::Expect(expected_type) => loop {
                    match socket.read().unwrap() {
                        Message::Text(text) => {
                            let value: Value = serde_json::from_str(&text).unwrap();
                            let actual_type = value["type"].as_str().unwrap_or("").to_string();
                            received.push(value);
                            if actual_type == "heartbeat" {
                                continue;
                            }
                            assert_eq!(actual_type, expected_type, "unexpected message type");
                            break;
                        }
                        Message::Ping(_) | Message::Pong(_) => continue,
                        other => panic!("unexpected frame: {other:?}"),
                    }
                },
            }
        }

        // Gives the tool a chance to flush its close reply before this
        // thread drops the socket (and with it the TCP connection).
        let _ = socket.read();
        received
    });

    (addr, handle)
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

/// The adapter's `hello`, generous on timing so no spurious heartbeat or
/// peer-timeout fires mid-test.
fn adapter_hello(tau_closure_depth: u64, early_output_timeout_ms: u64) -> Value {
    json!({
        "type": "hello",
        "id": "adapter-hello",
        "role": "adapter",
        "protocol_version": "0.2",
        "config": {
            "tau_closure_depth": tau_closure_depth,
            "early_output_timeout_ms": early_output_timeout_ms,
            "heartbeat_interval_ms": 60_000,
            "heartbeat_timeout_ms": 60_000,
        }
    })
}

fn multi_action(name: &str) -> Value {
    json!([{ "name": name, "args": [] }])
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

    let script = vec![
        Step::Expect("hello"),
        Step::Send(adapter_hello(20, 5_000)),
        Step::Send(json!({ "type": "get_enabled", "id": "g1" })),
        Step::Expect("enabled"),
        Step::Send(json!({ "type": "close", "id": "c1" })),
    ];
    let (addr, handle) = spawn_mock_adapter(script);
    run_tool(addr, model, default_limits());
    let received = handle.join().unwrap();

    let enabled = received
        .iter()
        .find(|m| m["type"] == "enabled")
        .expect("expected an `enabled` reply");
    assert_eq!(enabled["in_reply_to"], "g1");
    assert_eq!(enabled["inputs"], json!([multi_action("req")]));
    assert_eq!(enabled["outputs"], json!([]));
    assert_eq!(enabled["quiescence"], false);
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_get_enabled_before_hello_is_not_ready_and_survives() {
    let Some(model) = simple_model("test_mbt_session_not_ready") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let script = vec![
        Step::Expect("hello"),
        Step::Send(json!({ "type": "get_enabled", "id": "early" })),
        Step::Expect("error"),
        Step::Send(adapter_hello(20, 5_000)),
        Step::Send(json!({ "type": "get_enabled", "id": "g1" })),
        Step::Expect("enabled"),
        Step::Send(json!({ "type": "close", "id": "c1" })),
    ];
    let (addr, handle) = spawn_mock_adapter(script);
    run_tool(addr, model, default_limits());
    let received = handle.join().unwrap();

    let error = received
        .iter()
        .find(|m| m["type"] == "error")
        .expect("expected a `not_ready` error");
    assert_eq!(error["code"], "not_ready");
    assert_eq!(error["in_reply_to"], "early");
    assert!(
        received.iter().any(|m| m["type"] == "enabled"),
        "the session must survive to answer the later `get_enabled`"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_unknown_type_closes_with_error() {
    let Some(model) = simple_model("test_mbt_session_unknown_type") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let script = vec![
        Step::Expect("hello"),
        Step::Send(adapter_hello(20, 5_000)),
        Step::Send(json!({ "type": "not_a_real_type", "id": "x" })),
        Step::Expect("error"),
        Step::Expect("close"),
    ];
    let (addr, handle) = spawn_mock_adapter(script);
    run_tool(addr, model, default_limits());
    let received = handle.join().unwrap();

    let error = received.iter().find(|m| m["type"] == "error").unwrap();
    assert_eq!(error["code"], "unknown_type");
    assert_eq!(error["in_reply_to"], "x");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_protocol_mismatch_closes() {
    let Some(model) = simple_model("test_mbt_session_protocol_mismatch") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let script = vec![
        Step::Expect("hello"),
        Step::Send(json!({
            "type": "hello",
            "role": "adapter",
            "protocol_version": "0.1",
        })),
        Step::Expect("error"),
        Step::Expect("close"),
    ];
    let (addr, handle) = spawn_mock_adapter(script);
    run_tool(addr, model, default_limits());
    let received = handle.join().unwrap();

    let error = received.iter().find(|m| m["type"] == "error").unwrap();
    assert_eq!(error["code"], "protocol_mismatch");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_tau_closure_depth_above_cap_is_rejected() {
    let Some(model) = simple_model("test_mbt_session_config_cap") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let script = vec![
        Step::Expect("hello"),
        Step::Send(adapter_hello(1_000_000, 5_000)),
        Step::Expect("error"),
        Step::Expect("close"),
    ];
    let (addr, handle) = spawn_mock_adapter(script);
    run_tool(
        addr,
        model,
        SessionLimits {
            max_tau_closure_depth: 100,
        },
    );
    let received = handle.join().unwrap();

    let error = received.iter().find(|m| m["type"] == "error").unwrap();
    assert_eq!(error["code"], "unsupported_config");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_input_accepted_and_rejected() {
    let Some(model) = simple_model("test_mbt_session_input") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let script = vec![
        Step::Expect("hello"),
        Step::Send(adapter_hello(20, 5_000)),
        // `resp` is never classified as an input, so this must be rejected
        // regardless of the current state set.
        Step::Send(json!({ "type": "input", "id": "bad", "multi_action": multi_action("resp") })),
        Step::Expect("error"),
        // `req` is enabled from the initial state.
        Step::Send(json!({ "type": "input", "id": "good", "multi_action": multi_action("req") })),
        Step::Expect("ack"),
        Step::Send(json!({ "type": "close", "id": "c1" })),
    ];
    let (addr, handle) = spawn_mock_adapter(script);
    run_tool(addr, model, default_limits());
    let received = handle.join().unwrap();

    let error = received.iter().find(|m| m["type"] == "error").unwrap();
    assert_eq!(error["code"], "input_not_enabled");
    assert_eq!(error["in_reply_to"], "bad");

    let ack = received.iter().find(|m| m["type"] == "ack").unwrap();
    assert_eq!(ack["in_reply_to"], "good");
    assert_eq!(ack["kind"], "input");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_output_early_then_enabling_input_acks_in_order() {
    let Some(model) = simple_model("test_mbt_session_early_output") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let script = vec![
        Step::Expect("hello"),
        Step::Send(adapter_hello(20, 5_000)),
        // `resp` is not enabled until `req` has been accepted.
        Step::Send(json!({ "type": "output", "id": "o1", "multi_action": multi_action("resp") })),
        Step::Expect("warning"),
        Step::Send(json!({ "type": "input", "id": "i1", "multi_action": multi_action("req") })),
        Step::Expect("ack"),
        Step::Expect("ack"),
        Step::Send(json!({ "type": "close", "id": "c1" })),
    ];
    let (addr, handle) = spawn_mock_adapter(script);
    run_tool(addr, model, default_limits());
    let received = handle.join().unwrap();

    let warning = received.iter().find(|m| m["type"] == "warning").unwrap();
    assert_eq!(warning["in_reply_to"], "o1");
    assert_eq!(warning["code"], "output_early");

    let acks: Vec<&Value> = received.iter().filter(|m| m["type"] == "ack").collect();
    assert_eq!(
        acks.len(),
        2,
        "expected one ack for the input and one for the early output"
    );
    assert_eq!(acks[0]["in_reply_to"], "i1");
    assert_eq!(acks[0]["kind"], "input");
    assert_eq!(
        acks[1]["in_reply_to"], "o1",
        "the early output's ack must reference its own id"
    );
    assert_eq!(acks[1]["kind"], "output");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_reset_discards_pending_early_silently() {
    let Some(model) = simple_model("test_mbt_session_reset") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let script = vec![
        Step::Expect("hello"),
        Step::Send(adapter_hello(20, 5_000)),
        Step::Send(json!({ "type": "output", "id": "o1", "multi_action": multi_action("resp") })),
        Step::Expect("warning"),
        Step::Send(json!({ "type": "reset", "id": "r1" })),
        Step::Expect("reset_ack"),
        Step::Send(json!({ "type": "get_enabled", "id": "g1" })),
        Step::Expect("enabled"),
        Step::Send(json!({ "type": "close", "id": "c1" })),
    ];
    let (addr, handle) = spawn_mock_adapter(script);
    run_tool(addr, model, default_limits());
    let received = handle.join().unwrap();

    assert!(
        !received
            .iter()
            .any(|m| m["type"] != "warning" && m["in_reply_to"] == "o1"),
        "the discarded early entry must never be answered again: {received:#?}"
    );

    let enabled = received.iter().find(|m| m["type"] == "enabled").unwrap();
    assert_eq!(
        enabled["inputs"],
        json!([multi_action("req")]),
        "reset must return to the initial enabled set"
    );
    assert_eq!(enabled["outputs"], json!([]));
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_close_is_graceful() {
    let Some(model) = simple_model("test_mbt_session_close") else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let script = vec![
        Step::Expect("hello"),
        Step::Send(adapter_hello(20, 5_000)),
        Step::Send(json!({ "type": "close", "id": "c1", "reason": "test done" })),
    ];
    let (addr, handle) = spawn_mock_adapter(script);
    run_tool(addr, model, default_limits());
    handle.join().unwrap();
}
