use std::rc::Rc;

use merc_explore::LPS;
use merc_explore::Summand;
use merc_lps::ExplicitLinearProcessSpecification;
use merc_lps::explore_explicit::ExplicitContext;
use merc_utilities::MercError;
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;

use crate::action::MultiActionKey;
use crate::action::SerializableMultiAction;
use crate::error::MbtError;
use crate::partition::ActionClass;
use crate::partition::ActionPartition;

/// A state vector of an [`ExplicitLinearProcessSpecification`]: one interned
/// value index per process parameter, in the encoding `merc_explore::LPS`
/// enumerates over.
pub type StateVector = Vec<usize>;

/// A symbolic state set. The IOCO tool never tracks a single concrete LPS
/// state, only the (possibly many) states consistent with the trace of
/// accepted observations so far.
pub type StateSet = FxHashSet<StateVector>;

/// Everything one enumeration pass over a single state yields: the tau
/// successors, the classified non-tau steps, and whether the state is
/// quiescent. Cached by [`ModelState::summarise`] since the transition
/// relation is static and the same states are revisited repeatedly by
/// `tau_closure`/`enabled`/`post`.
struct StateSummary {
    tau_targets: Vec<StateVector>,
    steps: Vec<(MultiActionKey, ActionClass, StateVector)>,
    /// No tau successor and no output-classified step, per the spec's
    /// quiescence formula: `∃ s ∈ τ*ₖ(S). ∀ a ∈ Act_out ∪ {τ}. ¬(s →ᵃ)`. Input
    /// transitions do not disqualify a state from being quiescent.
    quiescent: bool,
}

/// The classified enabled set of a state set, as reported by `get_enabled`.
pub struct EnabledSet {
    pub inputs: Vec<SerializableMultiAction>,
    pub outputs: Vec<SerializableMultiAction>,
    pub quiescence: bool,
}

/// The IOCO model layer: an [`ExplicitLinearProcessSpecification`] driven
/// per-state through [`merc_explore::LPS`], classified by an
/// [`ActionPartition`], with a memoised per-state transition summary and the
/// current symbolic state set.
///
/// `current` is always stored *not* tau-closed: it holds exactly the active
/// set produced by the last `reset`/`post`/quiescence-filter, never the
/// result of closing it. Every read (`get_enabled`, `accept_*`) computes
/// `τ*ₖ(current)` fresh against that active set instead. Closing an
/// already-closed set again is not idempotent — a state at the previous
/// closure's frontier gets `k` further steps on top of the `k` it already
/// had — so storing the closure back into `current` would let the effective
/// depth grow without bound across observations. Keeping `current` raw
/// pins every closure to exactly `k` steps from the true active set.
pub struct ModelState {
    context: ExplicitContext,
    lps: ExplicitLinearProcessSpecification,
    partition: ActionPartition,
    current: StateSet,
    cache: FxHashMap<StateVector, Rc<StateSummary>>,
    /// Maximum number of cached summaries; `0` disables the cache. On
    /// overflow the cache is cleared wholesale, which is always safe since it
    /// is a pure memo over a static transition relation.
    cache_limit: usize,
    /// Upper bound on the size of any tau closure computed, a partial
    /// mitigation against an LPS whose enabled set is genuinely infinite
    /// (`docs/merc-mbt-implementation-plan.md` §"Open design questions",
    /// item 3); `0` disables the check.
    max_state_set_size: usize,
    /// Reusable buffer holding the source state passed to `prepare`/
    /// `enumerate`, avoiding an allocation per call to `summarise`.
    scratch: StateVector,
}

impl ModelState {
    /// Builds a model over `lps`, classified by `partition`, starting at the
    /// LPS's initial state. `partition` must already have passed
    /// [`ActionPartition::validate_against_lps`] against `lps` — this is not
    /// re-checked here.
    pub fn new(
        lps: ExplicitLinearProcessSpecification,
        partition: ActionPartition,
        cache_limit: usize,
        max_state_set_size: usize,
    ) -> Self {
        let context = lps.create_context();
        let mut current = StateSet::default();
        current.insert(lps.initial_state());
        ModelState {
            context,
            lps,
            partition,
            current,
            cache: FxHashMap::default(),
            cache_limit,
            max_state_set_size,
            scratch: Vec::new(),
        }
    }

    /// Rewinds to the initial state, per the `reset` message: `S := { s₀ }`,
    /// deliberately not tau-closed (the spec says "rewinds to the initial
    /// state", not to its tau closure).
    pub fn reset(&mut self) {
        self.current = StateSet::default();
        self.current.insert(self.lps.initial_state());
    }

    /// The enabled set of the current state set's tau closure, for
    /// `get_enabled`. Does not mutate `current`.
    pub fn get_enabled(&mut self, tau_closure_depth: usize) -> Result<EnabledSet, MbtError> {
        let closure = self.tau_closure(&self.current.clone(), tau_closure_depth)?;
        self.enabled(&closure)
    }

    /// Accepts an observed input `key` if it is enabled from the current
    /// state set's tau closure, applying `S := post_a(τ*ₖ(S))` (stored
    /// un-closed; see [`ModelState`]'s docs) and returning whether it was
    /// accepted. `current` is left unchanged when it is not.
    pub fn accept_input(&mut self, key: &MultiActionKey, tau_closure_depth: usize) -> Result<bool, MbtError> {
        self.accept_observation(key, tau_closure_depth, ActionClass::Input)
    }

    /// As [`ModelState::accept_input`], for an observed output.
    pub fn accept_output(&mut self, key: &MultiActionKey, tau_closure_depth: usize) -> Result<bool, MbtError> {
        self.accept_observation(key, tau_closure_depth, ActionClass::Output)
    }

    fn accept_observation(
        &mut self,
        key: &MultiActionKey,
        tau_closure_depth: usize,
        class: ActionClass,
    ) -> Result<bool, MbtError> {
        let closure = self.tau_closure(&self.current.clone(), tau_closure_depth)?;
        if !self.is_enabled(&closure, key, class)? {
            return Ok(false);
        }
        self.current = self.post(&closure, key)?;
        Ok(true)
    }

    /// Accepts a reported quiescence if the current state set's tau closure
    /// has a quiescent state, applying `S := { s ∈ τ*ₖ(S) | quiescent(s) }`
    /// (not re-closed: a quiescent state has no tau successor by
    /// definition). Returns whether it was accepted.
    pub fn accept_quiescence(&mut self, tau_closure_depth: usize) -> Result<bool, MbtError> {
        let closure = self.tau_closure(&self.current.clone(), tau_closure_depth)?;
        let quiescent = self.quiescent_subset(&closure)?;
        if quiescent.is_empty() {
            return Ok(false);
        }
        self.current = quiescent;
        Ok(true)
    }

    /// Layered BFS: expands `set` along tau transitions for at most `k`
    /// rounds, stopping early once the frontier empties (the usual case; `k`
    /// is a safety valve, not an iteration target).
    fn tau_closure(&mut self, set: &StateSet, k: usize) -> Result<StateSet, MbtError> {
        let mut closure = set.clone();
        let mut frontier = set.clone();

        for _ in 0..k {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier = StateSet::default();
            for state in &frontier {
                let summary = self.summarise(state)?;
                for target in &summary.tau_targets {
                    if closure.insert(target.clone()) {
                        next_frontier.insert(target.clone());
                    }
                }
            }
            frontier = next_frontier;

            if self.max_state_set_size != 0 && closure.len() > self.max_state_set_size {
                return Err(MbtError::Model(MercError::from(format!(
                    "tau closure exceeded --max-state-set-size ({} states); \
                     the LPS may have a genuinely infinite enabled set (e.g. `sum n: Nat . in(n) . P(n)`)",
                    self.max_state_set_size
                ))));
            }
        }

        debug_assert!(closure.is_superset(set), "tau closure must include the original set");
        Ok(closure)
    }

    /// One pass over `closure` collecting the distinct classified
    /// multi-actions and whether any state in it is quiescent.
    fn enabled(&mut self, closure: &StateSet) -> Result<EnabledSet, MbtError> {
        let mut seen_inputs = FxHashSet::default();
        let mut seen_outputs = FxHashSet::default();
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut quiescence = false;

        for state in closure {
            let summary = self.summarise(state)?;
            if summary.quiescent {
                quiescence = true;
            }
            for (key, class, _) in &summary.steps {
                let (seen, out) = match class {
                    ActionClass::Input => (&mut seen_inputs, &mut inputs),
                    ActionClass::Output => (&mut seen_outputs, &mut outputs),
                };
                if seen.insert(key.clone()) {
                    out.push(key.as_wire());
                }
            }
        }

        Ok(EnabledSet {
            inputs,
            outputs,
            quiescence,
        })
    }

    /// Whether `key`, classified as `class`, is enabled from some state in
    /// `closure`.
    fn is_enabled(&mut self, closure: &StateSet, key: &MultiActionKey, class: ActionClass) -> Result<bool, MbtError> {
        for state in closure {
            let summary = self.summarise(state)?;
            if summary
                .steps
                .iter()
                .any(|(step_key, step_class, _)| step_key == key && *step_class == class)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The states reachable from `closure` by the multi-action `key`,
    /// regardless of class (`post_a` in the spec's notation).
    fn post(&mut self, closure: &StateSet, key: &MultiActionKey) -> Result<StateSet, MbtError> {
        let mut result = StateSet::default();
        for state in closure {
            let summary = self.summarise(state)?;
            for (step_key, _, target) in &summary.steps {
                if step_key == key {
                    result.insert(target.clone());
                }
            }
        }
        Ok(result)
    }

    /// The quiescent subset of `closure`.
    fn quiescent_subset(&mut self, closure: &StateSet) -> Result<StateSet, MbtError> {
        let mut result = StateSet::default();
        for state in closure {
            let summary = self.summarise(state)?;
            if summary.quiescent {
                result.insert(state.clone());
            }
        }
        Ok(result)
    }

    /// Returns the cached transition summary for `state`, computing and
    /// caching it first if necessary.
    ///
    /// Mirrors the `prepare` → loop → `enumerate` driving pattern of
    /// `crates/explore/src/explore.rs`: `lps.prepare` returns an iterator of
    /// summand indices that borrows `lps` and `scratch` but not `context`, so
    /// the `enumerate` calls that follow may still borrow `context` mutably.
    fn summarise(&mut self, state: &StateVector) -> Result<Rc<StateSummary>, MbtError> {
        if let Some(summary) = self.cache.get(state) {
            return Ok(Rc::clone(summary));
        }

        if self.cache_limit != 0 && self.cache.len() >= self.cache_limit {
            self.cache.clear();
        }

        self.scratch.clear();
        self.scratch.extend_from_slice(state);

        let ModelState {
            lps,
            context,
            partition,
            scratch,
            ..
        } = self;

        let mut tau_targets = Vec::new();
        let mut steps: Vec<(MultiActionKey, ActionClass, StateVector)> = Vec::new();

        let summands_to_explore = lps.prepare(context, scratch);
        let summands = lps.summands();
        for index in summands_to_explore {
            summands[index].enumerate(context, scratch, |label, next_state| {
                // Structural decomposition, not `Display`: the multi-action's
                // arguments are already rewritten to normal form by the
                // enumerator, and re-parsing the pretty-printed form would be
                // ambiguous for any argument containing a comma. See
                // `MultiActionKey::from_model`.
                let key = MultiActionKey::from_model(&label.as_aterm());
                if key.is_tau() {
                    tau_targets.push(next_state.to_vec());
                } else {
                    let class = classify(partition, &key)?;
                    steps.push((key, class, next_state.to_vec()));
                }
                Ok(())
            })?;
        }

        let quiescent = tau_targets.is_empty() && !steps.iter().any(|(_, class, _)| *class == ActionClass::Output);

        let summary = Rc::new(StateSummary {
            tau_targets,
            steps,
            quiescent,
        });
        self.cache.insert(state.clone(), Rc::clone(&summary));
        Ok(summary)
    }
}

/// Classifies a non-tau multi-action key by its first constituent action.
///
/// `ActionPartition::validate_against_lps` runs at startup and rejects any
/// LPS action the partition does not cover, and any summand whose
/// multi-action mixes input- and output-classified actions, so every
/// constituent action of a non-tau key reachable here classifies to the same
/// class; the first is representative. A classification failure here means
/// that check was skipped, which is a caller bug rather than a runtime
/// condition — reported as a model error rather than panicking, since it
/// still crosses into the fallible enumeration callback.
fn classify(partition: &ActionPartition, key: &MultiActionKey) -> Result<ActionClass, MbtError> {
    let first = key
        .as_wire()
        .into_iter()
        .next()
        .expect("a non-tau key has at least one action");
    partition.classify(&first.name, first.args.len()).ok_or_else(|| {
        MbtError::Model(MercError::from(format!(
            "action `{}/{}` is not classified by the partition; \
             `ActionPartition::validate_against_lps` should have rejected this LPS at startup",
            first.name,
            first.args.len()
        )))
    })
}
