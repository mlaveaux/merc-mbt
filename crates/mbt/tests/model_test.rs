//! Model-layer tests against real mCRL2 specifications, compiled with
//! `mcrl22lps`. Mirrors `crates/merc_lps/tests/explore_lps_test.rs`: every
//! test is gated on `MCRL2_PATH` and skips (rather than fails) when it is
//! unset.

use std::path::Path;
use std::process::Command;

use mcrl2::read_lps;
use merc_io::temp_dir;
use merc_io::traced_command;
use merc_lps::ExplicitLinearProcessSpecification;
use merc_mbt::ModelState;
use merc_mbt::MultiActionKey;
use merc_mbt::WireAction;
use merc_mbt::parse_partition;

/// Compiles `mcrl2_text` with `mcrl22lps`, parses `partition_text` against
/// it, and builds a [`ModelState`] — or `None` if `MCRL2_PATH` is unset, in
/// which case the caller should skip the test.
fn load_model(name: &str, mcrl2_text: &str, partition_text: &str) -> Option<ModelState> {
    let mcrl2_path = std::env::var("MCRL2_PATH").ok()?;
    let mcrl22lps = Path::new(&mcrl2_path).join("mcrl22lps");

    let dir = temp_dir(name).unwrap();
    let spec_path = dir.path().join("spec.mcrl2");
    let lps_path = dir.path().join("spec.lps");
    std::fs::write(&spec_path, mcrl2_text).expect("Failed to write spec");

    let status =
        traced_command(Command::new(&mcrl22lps).arg(&spec_path).arg(&lps_path)).expect("Failed to execute mcrl22lps");
    assert!(status.success(), "mcrl22lps failed with status: {status}");

    let lps = read_lps(lps_path.to_str().unwrap()).expect("Failed to read LPS");
    let explicit = ExplicitLinearProcessSpecification::new(lps).expect("Failed to build explicit LPS");

    let partition = parse_partition(partition_text).expect("Failed to parse partition");
    partition
        .validate_against_lps(&explicit)
        .expect("Partition does not cover the LPS");

    Some(ModelState::new(explicit, partition, 0, 0))
}

fn action(name: &str, args: &[&str]) -> WireAction {
    WireAction {
        name: name.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
    }
}

/// A chain of exactly five tau steps before the only visible action `a`
/// becomes enabled, pinning `tau_closure`'s depth bound: `a` must be absent
/// from the enabled set below `k = 5` and present from `k = 5` onward.
#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_tau_closure_depth_is_pinned() {
    let spec = "
        act a;
        proc P(n: Nat) = (n < 5) -> tau . P(n + 1) + (n == 5) -> a . P(0);
        init P(0);
    ";
    let partition = "
        input
        output
          a;
    ";
    let Some(mut model) = load_model("test_mbt_tau_chain", spec, partition) else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    for k in 0..5 {
        let enabled = model.get_enabled(k).expect("get_enabled failed");
        assert!(
            enabled.outputs.is_empty(),
            "`a` must not be enabled yet at tau_closure_depth = {k}"
        );
    }

    for k in 5..=8 {
        let enabled = model.get_enabled(k).expect("get_enabled failed");
        assert_eq!(
            enabled.outputs,
            vec![vec![action("a", &[])]],
            "`a` must be enabled at tau_closure_depth = {k}"
        );
    }
}

/// A single `a`-summand pair from the same source state lands in two
/// different next states; accepting `a` must keep both in the state set.
#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_nondeterministic_branch_keeps_both_states() {
    let spec = "
        act a, x, y;
        proc P = x . P;
        proc R = y . R;
        init a . P + a . R;
    ";
    let partition = "
        input
          a;
        output
          x;
          y;
    ";
    let Some(mut model) = load_model("test_mbt_branch", spec, partition) else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let accepted = model
        .accept_input(&MultiActionKey::from_wire(&[action("a", &[])]), 10)
        .expect("accept_input failed");
    assert!(accepted, "`a` must be enabled from the initial state");

    let enabled = model.get_enabled(10).expect("get_enabled failed");
    let mut outputs = enabled.outputs.clone();
    outputs.sort();
    assert_eq!(
        outputs,
        vec![vec![action("x", &[])], vec![action("y", &[])]],
        "both branches' outputs must be visible after accepting `a`"
    );
}

/// Three states reached via a distinguishing input each demonstrate one leg
/// of the quiescence formula: tau-only is not quiescent, output-only is not
/// quiescent, input-only is quiescent.
#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_quiescence_formula() {
    let spec = "
        act out, inp, trigger;
        proc T = tau . T;
        proc Q = out . Q;
        proc R = inp . R;
        init trigger . T + out . Q + inp . R;
    ";
    let partition = "
        input
          trigger;
          inp;
        output
          out;
    ";

    // Tau-only: reached via `trigger`, then loops on tau forever.
    let Some(mut model) = load_model("test_mbt_quiescence_tau", spec, partition) else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };
    assert!(
        model
            .accept_input(&MultiActionKey::from_wire(&[action("trigger", &[])]), 10)
            .unwrap()
    );
    let enabled = model.get_enabled(10).unwrap();
    assert!(!enabled.quiescence, "a tau-only state must not be quiescent");
    assert!(enabled.inputs.is_empty());
    assert!(enabled.outputs.is_empty());

    // Output-only: reached via `out`.
    let Some(mut model) = load_model("test_mbt_quiescence_output", spec, partition) else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };
    assert!(
        model
            .accept_output(&MultiActionKey::from_wire(&[action("out", &[])]), 10)
            .unwrap()
    );
    let enabled = model.get_enabled(10).unwrap();
    assert!(
        !enabled.quiescence,
        "a state with an enabled output must not be quiescent"
    );

    // Input-only: reached via `inp`; only an input remains enabled, so the
    // state is quiescent per the spec's formula (it quantifies only over
    // Act_out ∪ {τ}).
    let Some(mut model) = load_model("test_mbt_quiescence_input", spec, partition) else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };
    assert!(
        model
            .accept_input(&MultiActionKey::from_wire(&[action("inp", &[])]), 10)
            .unwrap()
    );
    let enabled = model.get_enabled(10).unwrap();
    assert!(
        enabled.quiescence,
        "a state with only an input enabled must be quiescent"
    );
}

/// `sum n: Nat . (n < 3) -> out(n)`: the enabled set's argument strings are
/// the rewritten normal forms `"0"`, `"1"`, `"2"`, not unevaluated terms.
#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_data_carrying_action_arguments_are_normal_forms() {
    let spec = "
        act out: Nat;
        proc P = sum n: Nat . (n < 3) -> out(n) . P;
        init P;
    ";
    let partition = "
        input
        output
          out(n);
    ";
    let Some(mut model) = load_model("test_mbt_data_args", spec, partition) else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let mut outputs = model.get_enabled(0).expect("get_enabled failed").outputs;
    outputs.sort();
    assert_eq!(
        outputs,
        vec![
            vec![action("out", &["0"])],
            vec![action("out", &["1"])],
            vec![action("out", &["2"])],
        ]
    );
}

/// A genuine multi-action `a|b`: the canonical key does not depend on
/// argument order, and it round-trips through the wire form.
#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_multi_action_is_order_independent() {
    let spec = "
        act a, b;
        proc P = a|b . P;
        init P;
    ";
    let partition = "
        input
        output
          a;
          b;
    ";
    let Some(mut model) = load_model("test_mbt_multi_action", spec, partition) else {
        println!("Skipping test: MCRL2_PATH not set");
        return;
    };

    let outputs = model.get_enabled(0).expect("get_enabled failed").outputs;
    assert_eq!(outputs.len(), 1, "exactly one distinct multi-action must be enabled");
    let from_model = MultiActionKey::from_wire(&outputs[0]);

    // The wire round-trip must canonicalise to the same key regardless of
    // the order the adapter lists the constituent actions in.
    let forward = MultiActionKey::from_wire(&[action("a", &[]), action("b", &[])]);
    let reversed = MultiActionKey::from_wire(&[action("b", &[]), action("a", &[])]);
    assert_eq!(forward, reversed);
    assert_eq!(from_model, forward);
}
