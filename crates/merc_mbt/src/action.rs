use mcrl2::ATerm;
use mcrl2::ATermList;
use mcrl2::DataExpression;
use serde::Deserialize;
use serde::Serialize;

/// A single action as it appears on the wire: an mCRL2 action name together
/// with its arguments, each rendered in the canonical (pretty-printed,
/// rewritten normal form) string form the protocol requires.
///
/// `Ord` sorts by `(name, args)`, which is the canonicalisation the protocol
/// recommends for the otherwise-unordered elements of a multi-action.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WireAction {
    pub name: String,
    pub args: Vec<String>,
}

/// A multi-action as it appears on the wire: an ordered list of actions. The
/// single-action case is a list of length 1; tau is the empty list.
pub type WireMultiAction = Vec<WireAction>;

/// A multi-action normalised for matching.
///
/// mCRL2 multi-actions are multisets, but JSON arrays (and mCRL2 action
/// lists) are ordered, so both the wire form and the model form are sorted
/// by `(name, args)` before comparison. Duplicate actions are preserved (a
/// multi-action can contain the same action twice), only order is
/// normalised.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MultiActionKey(Vec<WireAction>);

impl MultiActionKey {
    /// Builds a key from a wire-encoded multi-action, canonicalising order.
    pub fn from_wire(actions: &[WireAction]) -> Self {
        let mut actions = actions.to_vec();
        actions.sort();
        MultiActionKey(actions)
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

        let mut actions: Vec<WireAction> = action_list.iter().map(decompose_action).collect();
        actions.sort();
        MultiActionKey(actions)
    }

    /// Returns true iff this key represents the empty multi-action, i.e. tau.
    pub fn is_tau(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the canonicalised multi-action as it should appear on the wire.
    pub fn as_wire(&self) -> WireMultiAction {
        self.0.clone()
    }
}

/// A human-readable rendering of a wire multi-action, for error and warning
/// messages (e.g. `MbtError::InputNotEnabled`) — not used on the wire itself,
/// where the structured [`WireMultiAction`] form is sent as-is.
pub fn describe_multi_action(actions: &[WireAction]) -> String {
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

/// Decomposes a single `Action(ActId(name, sorts), args)` term.
fn decompose_action(action: ATerm) -> WireAction {
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

    WireAction { name, args }
}

#[cfg(test)]
mod tests {
    use super::MultiActionKey;
    use super::WireAction;

    fn action(name: &str, args: &[&str]) -> WireAction {
        WireAction {
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
