use std::collections::BTreeSet;

use mcrl2::ATerm;
use mcrl2::ATermList;
use mcrl2::DataExpression;
use merc_collections::VecBag;
use merc_explore::LPS;
use merc_lps::ExplicitLinearProcessSpecification;
use serde::Deserialize;
use serde::Serialize;

/// A single action that can be serialized, or send over a protocol.
///
/// `Ord` sorts by `(name, args)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SerializableAction {
    pub name: String,
    pub args: Vec<String>,
}

/// A multi-action as an ordered list of actions. The single-action case is a
/// list of length 1; tau is the empty list.
pub type SerializableMultiAction = Vec<SerializableAction>;

/// A multi-action normalised for matching.
///
/// mCRL2 multi-actions are multisets, but JSON arrays are ordered, so both the
/// wire form and the model form are sorted by `(name, args)` before comparison.
/// Duplicate actions are preserved (a multi-action can contain the same action
/// twice), only order is normalised.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MultiActionKey(VecBag<SerializableAction>);

impl MultiActionKey {
    /// Builds a key from a wire-encoded multi-action, canonicalising order.
    pub fn from_wire(actions: &[SerializableAction]) -> Self {
        MultiActionKey(VecBag::from_vec(actions.to_vec()))
    }

    /// Decomposes a rewritten mCRL2 multi-action term into a canonical key.
    ///
    /// `term` must be a `TimedMultAct(actions, time)` term as produced by the
    /// LPS enumerator, with each action an `Action(ActId(name, sorts), args)`
    /// term whose `args` are already in rewriter normal form (the enumerator
    /// rewrites the multi-action template under the current substitution
    /// before invoking its callback). This is a structural decomposition of
    /// the real term rather than a re-parse of its pretty-printed `Display`
    /// form, which is required for correctness: `Display` flattens the whole
    /// multi-action into one string with arguments separated by bare commas,
    /// so splitting it back apart is ambiguous for any argument that itself
    /// contains a comma (lists, pairs, nested applications).
    pub fn from_model(term: &ATerm) -> Self {
        let head = term.get_head_symbol();
        debug_assert_eq!(head.name(), "TimedMultAct", "Expected a TimedMultAct term");
        debug_assert_eq!(head.arity(), 2, "Expected a TimedMultAct term");

        let action_list: ATermList<ATerm> = term.arg(0).protect().into();

        let actions: Vec<SerializableAction> = action_list.iter().map(decompose_action).collect();
        MultiActionKey(VecBag::from_vec(actions))
    }

    /// Returns true iff this key represents the empty multi-action, i.e. tau.
    pub fn is_tau(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the canonicalised multi-action as it should appear on the wire.
    pub fn as_wire(&self) -> SerializableMultiAction {
        self.0.iter().cloned().collect()
    }
}

/// A human-readable rendering of a wire multi-action, for error and warning
/// messages (e.g. `MbtError::InputNotEnabled`) — not used on the wire itself,
/// where the structured [`WireMultiAction`] form is sent as-is.
pub fn describe_multi_action(actions: &[SerializableAction]) -> String {
    if actions.is_empty() {
        return "tau".to_string();
    }
    actions
        .iter()
        .map(|action| {
            if action.args.is_empty() {
                action.name.clone()
            } else {
                format!("{}({})", action.name, action.args.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Yields the canonicalised [`MultiActionKey`] of every summand of `lps`, in
/// declaration order, read directly from the unrewritten multi-action
/// templates. A tau summand yields a key for which [`MultiActionKey::is_tau`]
/// is true, never filtered out here, so callers that care decide for
/// themselves.
///
/// Shared by [`collect_lps_actions`] and
/// [`crate::partition::ActionPartition::validate_against_lps`], which both
/// classify the same per-summand multi-actions and must stay in sync about
/// what counts as "the LPS's actions".
pub fn lps_action_keys(lps: &ExplicitLinearProcessSpecification) -> impl Iterator<Item = MultiActionKey> + '_ {
    lps.summands()
        .iter()
        .map(|summand| MultiActionKey::from_model(&summand.multi_action().as_aterm()))
}

/// Every distinct `(name, arity)` action pattern occurring in `lps`'s
/// summands, sorted by name then arity. Tau summands contribute nothing.
///
/// Used by the `info` subcommand to list the actions a partition file needs
/// to classify, so no enumeration or adapter connection is needed.
pub fn collect_lps_actions(lps: &ExplicitLinearProcessSpecification) -> Vec<(String, usize)> {
    let mut actions = BTreeSet::new();

    for key in lps_action_keys(lps) {
        for action in key.as_wire() {
            actions.insert((action.name, action.args.len()));
        }
    }

    actions.into_iter().collect()
}

/// Decomposes a single `Action(ActId(name, sorts), args)` term.
fn decompose_action(action: ATerm) -> SerializableAction {
    let action_head = action.get_head_symbol();
    debug_assert_eq!(action_head.name(), "Action", "Expected an Action term");
    debug_assert_eq!(action_head.arity(), 2, "Expected an Action term");

    let act_id = action.arg(0);
    let act_id_head = act_id.get_head_symbol();
    debug_assert_eq!(act_id_head.name(), "ActId", "Expected an ActId term");
    debug_assert_eq!(act_id_head.arity(), 2, "Expected an ActId term");

    // The action name is itself an mCRL2 identifier string: a 0-arity
    // function symbol whose *name* is the identifier, so no further
    // conversion is needed beyond reading the symbol's name.
    let name = act_id.arg(0).get_head_symbol().name().to_string();

    let arg_list: ATermList<ATerm> = action.arg(1).protect().into();
    let args = arg_list
        .iter()
        .map(|arg_term| DataExpression::from(arg_term).pretty_print())
        .collect();

    SerializableAction { name, args }
}

#[cfg(test)]
mod tests {
    use super::MultiActionKey;
    use super::SerializableAction;

    fn action(name: &str, args: &[&str]) -> SerializableAction {
        SerializableAction {
            name: name.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn from_wire_canonicalises_order() {
        let a = MultiActionKey::from_wire(&[action("b", &[]), action("a", &["3"])]);
        let b = MultiActionKey::from_wire(&[action("a", &["3"]), action("b", &[])]);
        assert_eq!(a, b);
    }

    #[test]
    fn from_wire_preserves_duplicates() {
        let key = MultiActionKey::from_wire(&[action("a", &[]), action("a", &[])]);
        assert_eq!(key.as_wire().len(), 2);
    }

    #[test]
    fn empty_multi_action_is_tau() {
        assert!(MultiActionKey::from_wire(&[]).is_tau());
        assert!(!MultiActionKey::from_wire(&[action("a", &[])]).is_tau());
    }
}
