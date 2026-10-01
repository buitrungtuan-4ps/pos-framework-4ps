// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `session` config node: when an attended till locks, how long a sign-in lasts on an
//! idle device, and how many wrong PINs lock a person out
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2).
//!
//! Every field is a setting in [`crate::settings`], and every default is what the edge did before the
//! field existed: a till never locks on its own, a sign-in lapses after thirty idle minutes, and five
//! wrong PINs in a row lock a person out for five minutes. So a document with no `session` node, or a
//! node without a field, runs a store exactly as it ran before.
//!
//! # Security settings
//!
//! These bound how long an unattended till stays usable, how long a till carried off while signed in
//! keeps working, and how fast a PIN can be guessed
//! ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md),
//! [ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)). Each is held to bounds that keep
//! the defence on, and none of them can switch the PIN lockout off. The cloud refuses a value outside
//! the bounds when it is written ([`Setting::check`](crate::settings::Setting::check)). An edge that
//! is sent one anyway reads it as the default, not as what it says.
//!
//! The PIN's length is not on this node. The cloud checks it when a PIN is set, and the edge only
//! ever holds a PIN's hash, so it has no PIN to measure.

use core::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

/// The seconds an attended till may sit with no touch before it locks, both bounds included. `0`
/// never locks. An hour at the most, because a lock that waits longer than that is no lock.
pub const IDLE_LOCK_SECONDS: RangeInclusive<i64> = 0..=3600;

/// The minutes a signed-in device may sit idle, both bounds included. Five at the least, because a
/// shorter window signs a person out between two orders. Four hours at the most, because the window
/// is also how long a till carried off while signed in keeps trading as that person.
pub const SIGN_IN_IDLE_TIMEOUT_MINUTES: RangeInclusive<i64> = 5..=240;

/// The wrong PINs in a row that lock a person out, both bounds included. Three at the least, so a
/// mistyped PIN or two does not lock anyone out. Ten at the most, so the lockout stays a defence.
pub const LOCKOUT_ATTEMPTS: RangeInclusive<i64> = 3..=10;

/// The minutes a lockout lasts, both bounds included. One at the least, because no value may switch
/// the lockout off. An hour at the most, because a person locked out mid-service waits it out.
pub const LOCKOUT_MINUTES: RangeInclusive<i64> = 1..=60;

/// The idle lock when nothing sets one: `0`, never, as every till ran before the setting.
pub const DEFAULT_IDLE_LOCK_SECONDS: u32 = 0;

/// The sign-in idle timeout when nothing sets one: thirty minutes, the edge's window before the
/// setting existed ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)).
pub const DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES: u32 = 30;

/// The wrong PINs in a row that lock a person out when nothing sets a number: five, as before.
pub const DEFAULT_LOCKOUT_ATTEMPTS: u32 = 5;

/// The minutes a lockout lasts when nothing sets them: five, as before.
pub const DEFAULT_LOCKOUT_MINUTES: u32 = 5;

/// The `session` node.
///
/// Each field is the number as it travels, `None` when the node does not carry it. Read a field
/// through its accessor, which applies the bounds and the default. No `deny_unknown_fields`, as for
/// every published node: an edge on an older release applies a node that carries a field it does not
/// know, rather than refusing the whole document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedSession {
    /// How many seconds an attended till may sit with no touch before it locks. Read it through
    /// [`PublishedSession::idle_lock_seconds`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_lock_seconds: Option<i64>,
    /// How many minutes a signed-in device may sit idle before its sign-in lapses. Read it through
    /// [`PublishedSession::sign_in_idle_timeout_minutes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in_idle_timeout_minutes: Option<i64>,
    /// How many wrong PINs in a row lock a person out. Read it through
    /// [`PublishedSession::lockout_attempts`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lockout_attempts: Option<i64>,
    /// How many minutes a lockout lasts. Read it through [`PublishedSession::lockout_minutes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lockout_minutes: Option<i64>,
}

impl PublishedSession {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "session";

    /// The seconds an attended till may sit with no touch, key, click or scroll before it signs its
    /// person out and locks: the node's number when it is within [`IDLE_LOCK_SECONDS`], and
    /// [`DEFAULT_IDLE_LOCK_SECONDS`], never, otherwise. `0` never locks.
    ///
    /// The till applies it, and only where a person stands: never on the kitchen board, the pass or
    /// a screen before anyone signs in.
    #[must_use]
    pub fn idle_lock_seconds(&self) -> u32 {
        within(self.idle_lock_seconds, &IDLE_LOCK_SECONDS).unwrap_or(DEFAULT_IDLE_LOCK_SECONDS)
    }

    /// The minutes a signed-in device may sit idle before its sign-in lapses, or `None` when the node
    /// sets no value within [`SIGN_IN_IDLE_TIMEOUT_MINUTES`].
    ///
    /// `None` rather than the default, unlike the other fields, because a store may still carry the
    /// window in its local file, deprecated by ADR-0160 decision 6. The edge falls back to that
    /// value, and to [`DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES`] when the file sets none.
    #[must_use]
    pub fn sign_in_idle_timeout_minutes(&self) -> Option<u32> {
        within(
            self.sign_in_idle_timeout_minutes,
            &SIGN_IN_IDLE_TIMEOUT_MINUTES,
        )
    }

    /// The wrong PINs in a row that lock a person out: the node's number when it is within
    /// [`LOCKOUT_ATTEMPTS`], and [`DEFAULT_LOCKOUT_ATTEMPTS`] otherwise. Never zero.
    #[must_use]
    pub fn lockout_attempts(&self) -> u32 {
        within(self.lockout_attempts, &LOCKOUT_ATTEMPTS).unwrap_or(DEFAULT_LOCKOUT_ATTEMPTS)
    }

    /// The minutes a lockout lasts: the node's number when it is within [`LOCKOUT_MINUTES`], and
    /// [`DEFAULT_LOCKOUT_MINUTES`] otherwise. Never zero.
    #[must_use]
    pub fn lockout_minutes(&self) -> u32 {
        within(self.lockout_minutes, &LOCKOUT_MINUTES).unwrap_or(DEFAULT_LOCKOUT_MINUTES)
    }
}

/// `value` when the node carries one within `bounds`, as the unsigned number every bound here is.
fn within(value: Option<i64>, bounds: &RangeInclusive<i64>) -> Option<u32> {
    value
        .filter(|number| bounds.contains(number))
        .and_then(|number| u32::try_from(number).ok())
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_LOCKOUT_ATTEMPTS, DEFAULT_LOCKOUT_MINUTES, LOCKOUT_ATTEMPTS, LOCKOUT_MINUTES,
        PublishedSession, SIGN_IN_IDLE_TIMEOUT_MINUTES,
    };

    fn node(text: &str) -> PublishedSession {
        serde_json::from_str(text).expect("the node parses")
    }

    #[test]
    fn an_absent_field_runs_the_store_as_before() {
        for empty in [node("{}"), PublishedSession::default()] {
            assert_eq!(
                empty.idle_lock_seconds(),
                0,
                "a till never locks on its own"
            );
            assert_eq!(empty.sign_in_idle_timeout_minutes(), None);
            assert_eq!(empty.lockout_attempts(), 5);
            assert_eq!(empty.lockout_minutes(), 5);
        }
    }

    #[test]
    fn a_published_value_within_its_bounds_is_read() {
        let set = node(
            r#"{ "sign_in_idle_timeout_minutes": 15, "lockout_attempts": 3, "lockout_minutes": 60 }"#,
        );
        assert_eq!(set.sign_in_idle_timeout_minutes(), Some(15));
        assert_eq!(set.lockout_attempts(), 3);
        assert_eq!(set.lockout_minutes(), 60);
    }

    #[test]
    fn a_value_outside_its_bounds_reads_as_the_default_and_never_switches_the_lockout_off() {
        for (attempts, minutes) in [(0, 0), (-1, -1), (2, 61), (11, i64::MAX)] {
            let set = node(&format!(
                r#"{{ "lockout_attempts": {attempts}, "lockout_minutes": {minutes} }}"#
            ));
            assert_eq!(
                set.lockout_attempts(),
                DEFAULT_LOCKOUT_ATTEMPTS,
                "{attempts}"
            );
            assert_eq!(set.lockout_minutes(), DEFAULT_LOCKOUT_MINUTES, "{minutes}");
        }
        for minutes in [0, 4, 241, i64::MIN] {
            let set = node(&format!(
                r#"{{ "sign_in_idle_timeout_minutes": {minutes} }}"#
            ));
            assert_eq!(set.sign_in_idle_timeout_minutes(), None, "{minutes}");
        }
    }

    #[test]
    fn an_idle_lock_is_read_within_its_bounds_and_otherwise_never_locks() {
        for (published, read) in [(0, 0), (120, 120), (3600, 3600), (3601, 0), (-1, 0)] {
            let set = node(&format!(r#"{{ "idle_lock_seconds": {published} }}"#));
            assert_eq!(set.idle_lock_seconds(), read, "{published}");
        }
    }

    #[test]
    fn the_bounds_hold_the_lockout_on() {
        assert!(*LOCKOUT_ATTEMPTS.start() > 0);
        assert!(*LOCKOUT_MINUTES.start() > 0);
        assert!(*SIGN_IN_IDLE_TIMEOUT_MINUTES.start() > 0);
    }

    #[test]
    fn a_field_this_release_does_not_know_is_ignored() {
        let set = node(r#"{ "lockout_attempts": 4, "pin_length": 6 }"#);
        assert_eq!(set.lockout_attempts(), 4);
    }

    #[test]
    fn a_number_of_another_type_refuses_the_node() {
        // The edge keeps the node it last applied when a new one does not parse, as for every node.
        for text in [
            r#"{ "lockout_attempts": 4.5 }"#,
            r#"{ "lockout_attempts": "4" }"#,
        ] {
            assert!(
                serde_json::from_str::<PublishedSession>(text).is_err(),
                "{text}"
            );
        }
    }

    #[test]
    fn the_node_round_trips_and_leaves_out_what_it_does_not_set() {
        let set = PublishedSession {
            lockout_attempts: Some(4),
            ..PublishedSession::default()
        };
        let text = serde_json::to_string(&set).expect("serialise");
        assert_eq!(text, r#"{"lockout_attempts":4}"#);
        let back: PublishedSession = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, set);
        assert_eq!(
            serde_json::to_string(&PublishedSession::default()).expect("serialise"),
            "{}"
        );
    }
}
