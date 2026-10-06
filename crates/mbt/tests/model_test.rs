//! Model-layer tests against hand-written linear process specifications
//! (LPS text), loaded directly with `mcrl2::read_lps_text`. Unlike
//! `crates/merc_lps/tests/explore_lps_test.rs`, these specs are already in
//! linear form (a single process equation, `cond -> action . P(...)`
//! summands), so no external `mcrl22lps` linearizer is required and the
//! tests are not gated on `MCRL2_PATH`.

use mcrl2::read_lps_text;
use merc_io::temp_dir;
use merc_lps::ExplicitLinearProcessSpecification;
use merc_mbt::ModelState;
use merc_mbt::MultiActionKey;
use merc_mbt::SerializableAction;
use merc_mbt::parse_partition;

/// Parses `lps_text` as a linear process specification, parses
/// `partition_text` against it, and builds a [`ModelState`].
fn load_model(name: &str, lps_text: &str, partition_text: &str) -> ModelState {
    let dir = temp_dir(name).unwrap();
    let lps_path = dir.path().join("spec.txt");
    std::fs::write(&lps_path, lps_text).expect("Failed to write LPS text");

    let lps = read_lps_text(lps_path.to_str().unwrap()).expect("Failed to parse LPS text");
    let explicit = ExplicitLinearProcessSpecification::new(lps).expect("Failed to build explicit LPS");

    let partition = parse_partition(partition_text.as_bytes()).expect("Failed to parse partition");
    partition
        .validate_against_lps(&explicit)
        .expect("Partition does not cover the LPS");

    ModelState::new(explicit, partition, 0, 0)
}

fn action(name: &str, args: &[&str]) -> SerializableAction {
    SerializableAction {
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
    let lps = "
        act a;
        proc P(n: Nat) = (n < 5) -> tau . P(n + 1) + (n == 5) -> a . P(0);
        init P(0);
    ";
    let partition = "
        input
        output
          a;
    ";
    let mut model = load_model("test_mbt_tau_chain", lps, partition);

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
///
/// This is the linear form of `init a . P + a . R` with `proc P = x . P`
/// and `proc R = y . R`: a control-state parameter `s` encodes which of the
/// three states (initial, `P`, `R`) the process is in.
#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_nondeterministic_branch_keeps_both_states() {
    let lps = "
        act a, x, y;
        proc P(s: Pos) = (s == 1) -> a . P(2) + (s == 1) -> a . P(3)
                        + (s == 2) -> x . P(2) + (s == 3) -> y . P(3);
        init P(1);
    ";
    let partition = "
        input
          a;
        output
          x;
          y;
    ";
    let mut model = load_model("test_mbt_branch", lps, partition);

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
///
/// This is the linear form of `init trigger . T + out . Q + inp . R` with
/// `proc T = tau . T`, `proc Q = out . Q`, `proc R = inp . R`: a
/// control-state parameter `s` encodes which of the four states (initial,
/// `T`, `Q`, `R`) the process is in.
#[test]
#[cfg_attr(miri, ignore)]
fn test_mcrl2_quiescence_formula() {
    let lps = "
        act out, inp, trigger;
        proc P(s: Pos) = (s == 1) -> trigger . P(2) + (s == 1) -> out . P(3) + (s == 1) -> inp . P(4)
                        + (s == 2) -> tau . P(2)
                        + (s == 3) -> out . P(3)
                        + (s == 4) -> inp . P(4);
        init P(1);
    ";
    let partition = "
        input
          trigger;
          inp;
        output
          out;
    ";

    // Tau-only: reached via `trigger`, then loops on tau forever.
    let mut model = load_model("test_mbt_quiescence_tau", lps, partition);
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
    let mut model = load_model("test_mbt_quiescence_output", lps, partition);
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
    let mut model = load_model("test_mbt_quiescence_input", lps, partition);
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
    let lps = "
        act out: Nat;
        proc P = sum n: Nat . (n < 3) -> out(n) . P;
        init P;
    ";
    let partition = "
        input
        output
          out(n);
    ";
    let mut model = load_model("test_mbt_data_args", lps, partition);

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
    let lps = "
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
    let mut model = load_model("test_mbt_multi_action", lps, partition);

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
