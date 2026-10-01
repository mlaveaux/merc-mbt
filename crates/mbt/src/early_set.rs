use std::collections::VecDeque;
use std::time::Instant;

use crate::action::MultiActionKey;

/// One output reported by the adapter before the tool considered any output
/// enabled ("early"), held pending re-evaluation against later state
/// changes until it either matches or its own deadline expires.
#[derive(Debug, Clone)]
pub struct EarlyEntry {
    pub id: String,
    pub key: MultiActionKey,
    pub deadline: Instant,
}

/// The set of pending early outputs, in insertion order.
///
/// Deliberately holds `Instant`s rather than reading the clock itself: every
/// method here is pure bookkeeping over a `now` passed in by the caller, so
/// it can be exercised in tests with synthetic times instead of real sleeps.
/// See `docs/merc-mbt-implementation-plan.md` §6.4 ("Deterministic timer
/// testing").
#[derive(Debug, Default)]
pub struct EarlySet {
    entries: VecDeque<EarlyEntry>,
}

impl EarlySet {
    /// Inserts a new pending entry at the end of the queue.
    pub fn push(&mut self, entry: EarlyEntry) {
        self.entries.push_back(entry);
    }

    /// Whether the set has no pending entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of pending entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Discards every pending entry silently (no ack/warning/error), as
    /// `reset` requires.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The earliest deadline among all pending entries, if any, used to bound
    /// the event loop's next blocking read.
    pub fn earliest_deadline(&self) -> Option<Instant> {
        self.entries.iter().map(|e| e.deadline).min()
    }

    /// Removes and returns every entry whose deadline has passed as of `now`,
    /// in their original insertion order.
    pub fn take_expired(&mut self, now: Instant) -> Vec<EarlyEntry> {
        let mut expired = Vec::new();
        let mut remaining = VecDeque::with_capacity(self.entries.len());
        for entry in self.entries.drain(..) {
            if entry.deadline <= now {
                expired.push(entry);
            } else {
                remaining.push_back(entry);
            }
        }
        self.entries = remaining;
        expired
    }

    /// Iterates the pending entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &EarlyEntry> {
        self.entries.iter()
    }

    /// Finds the first pending entry (in insertion order) matching `key`,
    /// removing and returning it.
    ///
    /// Re-evaluation restarts the scan after each match (one match may enable
    /// another), so only the first match per call matters; the caller loops
    /// until this returns `None`.
    pub fn take_matching(&mut self, key: &MultiActionKey) -> Option<EarlyEntry> {
        let position = self.entries.iter().position(|e| &e.key == key)?;
        self.entries.remove(position)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::time::Instant;

    use super::EarlyEntry;
    use super::EarlySet;
    use crate::action::MultiActionKey;

    fn key(name: &str) -> MultiActionKey {
        MultiActionKey::from_wire(&[crate::action::WireAction {
            name: name.to_string(),
            args: Vec::new(),
        }])
    }

    fn entry(id: &str, name: &str, deadline: Instant) -> EarlyEntry {
        EarlyEntry {
            id: id.to_string(),
            key: key(name),
            deadline,
        }
    }

    #[test]
    fn earliest_deadline_is_none_when_empty() {
        let set = EarlySet::default();
        assert_eq!(set.earliest_deadline(), None);
    }

    #[test]
    fn earliest_deadline_picks_the_minimum() {
        let base = Instant::now();
        let mut set = EarlySet::default();
        set.push(entry("a", "x", base + Duration::from_millis(200)));
        set.push(entry("b", "y", base + Duration::from_millis(50)));
        set.push(entry("c", "z", base + Duration::from_millis(100)));
        assert_eq!(set.earliest_deadline(), Some(base + Duration::from_millis(50)));
    }

    #[test]
    fn take_expired_preserves_insertion_order_and_leaves_the_rest() {
        let base = Instant::now();
        let mut set = EarlySet::default();
        set.push(entry("a", "x", base + Duration::from_millis(10)));
        set.push(entry("b", "y", base + Duration::from_millis(20)));
        set.push(entry("c", "z", base + Duration::from_millis(30)));

        let expired = set.take_expired(base + Duration::from_millis(20));
        assert_eq!(
            expired.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(set.len(), 1);
        assert_eq!(set.earliest_deadline(), Some(base + Duration::from_millis(30)));
    }

    #[test]
    fn take_expired_at_exact_deadline_expires() {
        let base = Instant::now();
        let mut set = EarlySet::default();
        set.push(entry("a", "x", base));
        let expired = set.take_expired(base);
        assert_eq!(expired.len(), 1);
        assert!(set.is_empty());
    }

    #[test]
    fn take_expired_empty_set_yields_nothing() {
        let mut set = EarlySet::default();
        assert!(set.take_expired(Instant::now()).is_empty());
    }

    #[test]
    fn take_matching_removes_the_first_match_only() {
        let base = Instant::now();
        let mut set = EarlySet::default();
        set.push(entry("a", "x", base + Duration::from_millis(10)));
        set.push(entry("b", "x", base + Duration::from_millis(20)));

        let found = set.take_matching(&key("x")).unwrap();
        assert_eq!(found.id, "a");
        assert_eq!(set.len(), 1);
        assert_eq!(set.take_matching(&key("x")).unwrap().id, "b");
        assert!(set.take_matching(&key("x")).is_none());
    }

    #[test]
    fn take_matching_no_match_returns_none() {
        let base = Instant::now();
        let mut set = EarlySet::default();
        set.push(entry("a", "x", base + Duration::from_millis(10)));
        assert!(set.take_matching(&key("y")).is_none());
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn clear_discards_everything_silently() {
        let base = Instant::now();
        let mut set = EarlySet::default();
        set.push(entry("a", "x", base + Duration::from_millis(10)));
        set.push(entry("b", "y", base + Duration::from_millis(20)));
        set.clear();
        assert!(set.is_empty());
        assert_eq!(set.earliest_deadline(), None);
    }
}
