// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The state-machine framework: transitions that are data, not code.
//!
//! `docs/architecture.md` §5 requires an explicit *state × event → new state* table for Order,
//! Bill, Shift and Table, plus the order line — and requires that table to be the machine-readable
//! twin of the specification, with property tests bound to it. That phrasing is a design
//! instruction, not prose: the transitions have to be **enumerable at runtime** so the documentation
//! table is generated from the code rather than kept in sync by hand, and so a single generic test
//! can prove every machine sound rather than each machine re-proving it.
//!
//! # What a machine is
//!
//! A type implementing [`StateMachine`] declares its states (a `pos-proto` wire enum), its triggers
//! (an internal enum naming the transitions), and three total functions: [`StateMachine::next`] (the
//! transition table), [`StateMachine::is_terminal`], and [`StateMachine::rank`]. Everything else —
//! stepping, merging, the documentation table, and the whole invariant suite — is derived here,
//! once.
//!
//! # Why `rank`, and how it makes merge correct
//!
//! [ADR-0029](../../../docs/adr/0029-append-command-merge-semantics.md) requires that when two
//! devices edit the same line concurrently, the line-state halves merge with **terminal states
//! winning** — a `VOIDED` line is never resurrected — and that the merge is **commutative and
//! associative**, or two devices diverge by sync order.
//!
//! Both fall out of one decision: give every state a unique `rank`, a linear extension of the
//! lifecycle with terminal states ranked above every non-terminal, and define [`StateMachine::merge`]
//! as *the higher-ranked of the two states*. Maximum over a total order is commutative, associative
//! and idempotent by construction, so the hard property is free; and because terminals outrank
//! everything, a terminal always wins. There is nothing to get subtly wrong at a call site, because
//! there is no logic at the call site.

use std::collections::BTreeSet;

use pos_proto::wire_enum::WireEnum;

/// A trigger was not valid in the current state.
///
/// Carries the machine, the state, and the trigger's name, so a domain error can say *which*
/// transition was refused rather than only that one was — the same reasoning the contract suites
/// use for their obligations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    /// The machine that refused the transition, e.g. `"bill"`.
    pub machine: &'static str,
    /// The state the machine was in, as its wire token.
    pub from: &'static str,
    /// The trigger that was refused, as its name.
    pub trigger: &'static str,
}

impl core::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}: {} is not a valid transition from {}",
            self.machine, self.trigger, self.from
        )
    }
}

impl core::error::Error for TransitionError {}

/// A domain state machine whose transitions are data.
///
/// Implementors supply the table; this trait derives the behaviour and the proofs. The bounds on
/// `State` are what let the generic invariant checker enumerate exhaustively: a machine's state set
/// is tiny (three or four), so the tests below are exhaustive rather than sampled.
pub trait StateMachine {
    /// The states, a `pos-proto` wire enum. Its `UNSPECIFIED` zero value is **not** a machine state
    /// — see [`Self::states`], which excludes it.
    type State: Copy + Eq + Ord + core::hash::Hash + core::fmt::Debug + WireEnum;

    /// What drives a transition. An internal enum, one variant per named transition; not a wire
    /// type, because a trigger is a domain action rather than something that crosses a boundary.
    type Trigger: Copy + Eq + Ord + core::hash::Hash + core::fmt::Debug + 'static;

    /// The machine's name, lower-case, for error messages and the generated table.
    const NAME: &'static str;

    /// The state a fresh entity starts in.
    fn initial() -> Self::State;

    /// Every trigger, for enumeration. The generated documentation table and the exhaustive tests
    /// both iterate this.
    fn triggers() -> &'static [Self::Trigger];

    /// A trigger's name, for the table and for [`TransitionError`].
    fn trigger_name(trigger: Self::Trigger) -> &'static str;

    /// The transition table. `Some(to)` when the trigger is valid in `from`, `None` when it is not.
    ///
    /// Must never return `UNSPECIFIED` and never return a state outside [`Self::states`]; a test
    /// proves both.
    fn next(from: Self::State, trigger: Self::Trigger) -> Option<Self::State>;

    /// Whether a state is terminal: it accepts no trigger and wins a merge
    /// ([ADR-0029](../../../docs/adr/0029-append-command-merge-semantics.md)).
    fn is_terminal(state: Self::State) -> bool;

    /// A state's rank: a linear extension of the lifecycle, unique per state, with every terminal
    /// state ranked above every non-terminal one. This is the whole basis of [`Self::merge`]; the
    /// invariant checker proves the uniqueness and the terminal-above-non-terminal property so that
    /// merge is provably a commutative, associative join.
    fn rank(state: Self::State) -> u8;

    /// Every real state, `UNSPECIFIED` excluded.
    ///
    /// `UNSPECIFIED` is the wire enum's "absent or unrecognised" marker, not a lifecycle state, so a
    /// machine never starts there, never transitions there, and never treats it as terminal. The
    /// invariant checker asserts all three.
    #[must_use]
    fn states() -> Vec<Self::State> {
        Self::State::ALL
            .iter()
            .copied()
            .filter(|state| *state != Self::State::UNSPECIFIED)
            .collect()
    }

    /// Applies a trigger, or reports precisely which transition was refused.
    ///
    /// # Errors
    ///
    /// [`TransitionError`] naming the machine, state and trigger when the transition is not in the
    /// table — which is what a settled bill returns to a second settle, and a closed shift to any
    /// transaction.
    fn step(from: Self::State, trigger: Self::Trigger) -> Result<Self::State, TransitionError> {
        Self::next(from, trigger).ok_or(TransitionError {
            machine: Self::NAME,
            from: from.as_wire(),
            trigger: Self::trigger_name(trigger),
        })
    }

    /// Merges two observations of the same entity's state: the higher-ranked wins.
    ///
    /// This is the state half of the line merge in
    /// [ADR-0029](../../../docs/adr/0029-append-command-merge-semantics.md). Because terminals rank
    /// above non-terminals, a `VOIDED` or `SETTLED` state can never be overwritten by a concurrent
    /// non-terminal edit; because it is a maximum over a total order, it is commutative, associative
    /// and idempotent, so two devices converge to one state regardless of sync order. The non-state
    /// fields of a line (quantity, note-presence, modifiers) merge last-writer-wins elsewhere; this
    /// function is only the state.
    #[must_use]
    fn merge(left: Self::State, right: Self::State) -> Self::State {
        if Self::rank(right) > Self::rank(left) {
            right
        } else {
            left
        }
    }

    /// The states reachable from [`Self::initial`] by any sequence of triggers.
    ///
    /// Used by the invariant checker to prove there is no orphan state — one that appears in the enum
    /// and in no transition — which would be a state the specification names and the machine can
    /// never enter.
    #[must_use]
    fn reachable() -> BTreeSet<Self::State> {
        let mut seen = BTreeSet::new();
        let mut frontier = vec![Self::initial()];
        seen.insert(Self::initial());
        while let Some(state) = frontier.pop() {
            for trigger in Self::triggers() {
                if let Some(to) = Self::next(state, *trigger)
                    && seen.insert(to)
                {
                    frontier.push(to);
                }
            }
        }
        seen
    }

    /// The transition table as a GitHub-flavoured Markdown table.
    ///
    /// Rows are states (terminal ones marked), columns are triggers, a cell is the resulting state's
    /// wire token or `·` for "not allowed". This is the "documentation table is generated" half of
    /// `docs/architecture.md` §5: `docs/state-machines.md` is produced from this, and a test keeps
    /// the committed file identical to what the code renders, so the doc cannot drift from the
    /// machine.
    #[must_use]
    fn render_markdown() -> String {
        // Built with `push_str` rather than `write!`/`format!` on purpose: every piece is already a
        // `&str` (the wire tokens and trigger names both are), so there is nothing to format, and
        // `clippy::format_push_string` is denied under the workspace's `-D warnings`.
        let triggers = Self::triggers();
        let mut out = String::new();
        out.push_str("### `");
        out.push_str(Self::NAME);
        out.push_str("`\n\n| state |");
        for trigger in triggers {
            out.push(' ');
            out.push_str(Self::trigger_name(*trigger));
            out.push_str(" |");
        }
        out.push_str("\n|---|");
        for _ in triggers {
            out.push_str("---|");
        }
        out.push('\n');
        for state in Self::states() {
            out.push_str("| `");
            out.push_str(state.as_wire());
            out.push('`');
            if Self::is_terminal(state) {
                out.push_str(" *(terminal)*");
            }
            out.push_str(" |");
            for trigger in triggers {
                match Self::next(state, *trigger) {
                    Some(to) => {
                        out.push_str(" `");
                        out.push_str(to.as_wire());
                        out.push_str("` |");
                    }
                    None => out.push_str(" · |"),
                }
            }
            out.push('\n');
        }
        out
    }
}

/// Proves every invariant a [`StateMachine`] must satisfy, exhaustively.
///
/// Called from each machine's test module. Exhaustive rather than sampled because a machine's state
/// set is tiny, so this visits every state, every pair, and every triple — which is what makes
/// "no reachable undefined state and no panic path" (the P3 exit criterion) a checked fact rather
/// than a claim. It is split into four focused checks so each reads on its own and none is long
/// enough to hide a case.
///
/// # Panics
///
/// If any invariant fails, with a message naming the machine and the specific violation — which is
/// the point: a failure here is a state machine that would let a bill settle twice or a void come
/// back.
#[cfg(test)]
pub fn check_invariants<M: StateMachine>() {
    let states = M::states();
    assert!(
        !states.is_empty(),
        "{}: a machine has at least one state",
        M::NAME
    );
    check_unspecified_is_not_a_state::<M>();
    check_ranks::<M>(&states);
    check_transition_table::<M>(&states);
    check_reachability::<M>(&states);
    check_merge_laws::<M>(&states);
}

/// `UNSPECIFIED` is the wire enum's "absent or unrecognised" marker, never a lifecycle state.
#[cfg(test)]
fn check_unspecified_is_not_a_state<M: StateMachine>() {
    assert!(
        !M::states().contains(&M::State::UNSPECIFIED),
        "{}: UNSPECIFIED is not a lifecycle state",
        M::NAME
    );
    assert_ne!(
        M::initial(),
        M::State::UNSPECIFIED,
        "{}: the initial state is not UNSPECIFIED",
        M::NAME
    );
    assert!(
        !M::is_terminal(M::State::UNSPECIFIED),
        "{}: UNSPECIFIED is not terminal",
        M::NAME
    );
}

/// Ranks are unique, and every terminal ranks above every non-terminal — the two facts that make
/// [`StateMachine::merge`] a well-defined, terminal-preserving join.
#[cfg(test)]
fn check_ranks<M: StateMachine>(states: &[M::State]) {
    let distinct: BTreeSet<u8> = states.iter().map(|state| M::rank(*state)).collect();
    assert_eq!(
        distinct.len(),
        states.len(),
        "{}: ranks must be unique per state",
        M::NAME
    );

    let max_non_terminal = states
        .iter()
        .filter(|state| !M::is_terminal(**state))
        .map(|state| M::rank(*state))
        .max();
    let min_terminal = states
        .iter()
        .filter(|state| M::is_terminal(**state))
        .map(|state| M::rank(*state))
        .min();
    if let (Some(non_terminal), Some(terminal)) = (max_non_terminal, min_terminal) {
        assert!(
            terminal > non_terminal,
            "{}: every terminal state must rank above every non-terminal one",
            M::NAME
        );
    }
}

/// A transition never targets `UNSPECIFIED` or an unlisted state, and a terminal state has no
/// outgoing transition — the property [ADR-0029] leans on.
#[cfg(test)]
fn check_transition_table<M: StateMachine>(states: &[M::State]) {
    for from in states {
        for trigger in M::triggers() {
            if let Some(to) = M::next(*from, *trigger) {
                assert_ne!(
                    to,
                    M::State::UNSPECIFIED,
                    "{}: transition to UNSPECIFIED",
                    M::NAME
                );
                assert!(
                    states.contains(&to),
                    "{}: transition to an unlisted state",
                    M::NAME
                );
                assert!(
                    !M::is_terminal(*from),
                    "{}: terminal state {} has an outgoing transition",
                    M::NAME,
                    from.as_wire()
                );
            }
        }
    }
}

/// No orphan state (all reachable from the initial state), and no deadlock (every non-terminal
/// reaches a terminal, unless the machine is cyclic and has none, as a table is).
#[cfg(test)]
fn check_reachability<M: StateMachine>(states: &[M::State]) {
    let reachable = M::reachable();
    for state in states {
        assert!(
            reachable.contains(state),
            "{}: state {} is unreachable from {}",
            M::NAME,
            state.as_wire(),
            M::initial().as_wire()
        );
    }
    if states.iter().any(|state| M::is_terminal(*state)) {
        for start in states.iter().filter(|state| !M::is_terminal(**state)) {
            assert!(
                reaches_terminal::<M>(*start),
                "{}: non-terminal state {} cannot reach any terminal state",
                M::NAME,
                start.as_wire()
            );
        }
    }
}

/// Merge is commutative, associative, idempotent, and terminal-preserving — exhaustively over every
/// pair and triple, which for a machine's tiny state set is the full property, not a sample.
#[cfg(test)]
fn check_merge_laws<M: StateMachine>(states: &[M::State]) {
    for a in states {
        assert_eq!(M::merge(*a, *a), *a, "{}: merge is idempotent", M::NAME);
        for b in states {
            assert_eq!(
                M::merge(*a, *b),
                M::merge(*b, *a),
                "{}: merge is commutative",
                M::NAME
            );
            if M::is_terminal(*a) {
                assert!(
                    M::is_terminal(M::merge(*a, *b)),
                    "{}: a terminal state must survive a merge",
                    M::NAME
                );
            }
            for c in states {
                assert_eq!(
                    M::merge(M::merge(*a, *b), *c),
                    M::merge(*a, M::merge(*b, *c)),
                    "{}: merge is associative",
                    M::NAME
                );
            }
        }
    }
}

/// Whether `start` can reach any terminal state by some sequence of triggers.
#[cfg(test)]
fn reaches_terminal<M: StateMachine>(start: M::State) -> bool {
    let mut seen = BTreeSet::new();
    let mut frontier = vec![start];
    seen.insert(start);
    while let Some(state) = frontier.pop() {
        if M::is_terminal(state) {
            return true;
        }
        for trigger in M::triggers() {
            if let Some(to) = M::next(state, *trigger)
                && seen.insert(to)
            {
                frontier.push(to);
            }
        }
    }
    false
}
