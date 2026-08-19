// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Offline user authentication: a PIN, verified locally, with a lockout (ADR-0030).
//!
//! The cloud syncs each employee's **Argon2id** PIN hash to the edge as configuration
//! ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md)); the edge verifies a PIN against
//! that hash with no network at all ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)).
//! Because a PIN is short, the Argon2id cost **and a local lockout** — five consecutive failures lock
//! an employee out for five minutes — are the brute-force defence, not the PIN's entropy.
//!
//! # No secrets in logs
//!
//! A PIN and its hash are secrets: they never enter a log, a span, an event, or the fan-out. Only the
//! employee id (an identifier, not PII) and the outcome are observable. See
//! [`crate::telemetry`].
//!
//! # Why the lockout is separable from the hashing
//!
//! [`Lockout::record`] is a pure state machine over `(employee, verified, now)` — no Argon2, no
//! clock of its own — so the five-minute window is unit-tested in microseconds with a fixed
//! [`Timestamp`]. [`verify_pin`] is the thin Argon2 wrapper. [`Lockout::authenticate`] combines them.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordVerifier};
use pos_proto::ids::EmployeeId;
use pos_proto::time::Timestamp;

/// Consecutive wrong PINs that trigger a lockout.
pub const MAX_FAILURES: u32 = 5;

/// How long a locked-out employee must wait.
pub const LOCKOUT: Duration = Duration::from_secs(5 * 60);

/// The outcome of a sign-in attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    /// The PIN matched; the failure count is now reset.
    Ok,
    /// Wrong PIN, with how many attempts remain before a lockout.
    Wrong {
        /// Attempts left before the next failure locks the employee out.
        remaining: u32,
    },
    /// The employee is locked out until this instant (milliseconds since the Unix epoch). The UI
    /// shows a countdown rather than rejecting silently (ADR-0030).
    LockedOut {
        /// When the lockout ends, in milliseconds since the Unix epoch.
        until_ms: i64,
    },
}

/// Verifies a PIN against a stored Argon2id PHC hash.
///
/// A malformed stored hash verifies nothing — it returns `false` rather than erroring, because a
/// corrupt synced hash must not become a way in.
#[must_use]
pub fn verify_pin(phc_hash: &str, pin: &str) -> bool {
    match PasswordHash::new(phc_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(pin.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Per-employee failure tracking for the offline PIN lockout.
///
/// One instance lives in the edge's application state. It holds counts only — never a PIN or a hash.
#[derive(Debug, Default)]
pub struct Lockout {
    state: Mutex<HashMap<EmployeeId, Record>>,
}

/// One employee's failure state.
#[derive(Debug, Default, Clone, Copy)]
struct Record {
    /// Consecutive failures since the last success or served lockout.
    failures: u32,
    /// When the current lockout ends, if any (ms since epoch).
    locked_until_ms: Option<i64>,
}

impl Lockout {
    /// A fresh tracker with no failures recorded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the outcome of a verification and returns what the caller may do.
    ///
    /// Pure but for the internal map: given whether the PIN `verified` and the current instant
    /// `now`, it advances the lockout state machine. A correct PIN while locked out is still refused
    /// — serving the lockout is the point.
    pub fn record(&self, employee: EmployeeId, verified: bool, now: Timestamp) -> SignIn {
        let now_ms = now.as_milliseconds_since_epoch();
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let record = guard.entry(employee).or_default();

        // A lockout that has elapsed is cleared before this attempt is judged.
        if let Some(until) = record.locked_until_ms
            && now_ms >= until
        {
            record.locked_until_ms = None;
            record.failures = 0;
        }

        if let Some(until) = record.locked_until_ms {
            return SignIn::LockedOut { until_ms: until };
        }

        if verified {
            *record = Record::default();
            return SignIn::Ok;
        }

        record.failures = record.failures.saturating_add(1);
        if record.failures >= MAX_FAILURES {
            let lockout_ms = i64::try_from(LOCKOUT.as_millis()).unwrap_or(i64::MAX);
            let until = now_ms.saturating_add(lockout_ms);
            record.locked_until_ms = Some(until);
            SignIn::LockedOut { until_ms: until }
        } else {
            SignIn::Wrong {
                remaining: MAX_FAILURES - record.failures,
            }
        }
    }

    /// Verifies `pin` against `phc_hash` and records the outcome under the lockout policy.
    pub fn authenticate(
        &self,
        employee: EmployeeId,
        phc_hash: &str,
        pin: &str,
        now: Timestamp,
    ) -> SignIn {
        // A locked-out employee is refused before the (costly) Argon2 verification even runs.
        {
            let now_ms = now.as_milliseconds_since_epoch();
            let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(record) = guard.get_mut(&employee)
                && let Some(until) = record.locked_until_ms
                && now_ms < until
            {
                return SignIn::LockedOut { until_ms: until };
            }
        }
        self.record(employee, verify_pin(phc_hash, pin), now)
    }
}

#[cfg(test)]
mod tests {
    use super::{Lockout, MAX_FAILURES, SignIn, verify_pin};
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString};
    use pos_proto::ids::EmployeeId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid instant")
    }

    fn employee() -> EmployeeId {
        EmployeeId::new(Ulid::from_u128(7))
    }

    /// A real Argon2id PHC hash of `pin`, computed with a fixed salt so the test needs no RNG.
    fn hash_of(pin: &str) -> String {
        let salt = SaltString::encode_b64(b"fixed-test-salt!").expect("salt");
        Argon2::default()
            .hash_password(pin.as_bytes(), &salt)
            .expect("hash")
            .to_string()
    }

    #[test]
    fn a_correct_pin_verifies_and_a_wrong_one_does_not() {
        let hash = hash_of("2468");
        assert!(verify_pin(&hash, "2468"));
        assert!(!verify_pin(&hash, "1357"));
    }

    #[test]
    fn a_malformed_stored_hash_is_never_a_way_in() {
        assert!(!verify_pin("not-a-phc-string", "2468"));
        assert!(!verify_pin("", ""));
    }

    #[test]
    fn five_failures_lock_the_employee_out_for_five_minutes() {
        let lockout = Lockout::new();
        let who = employee();

        // Four wrong attempts count down without locking.
        for expected_remaining in (1..MAX_FAILURES).rev() {
            match lockout.record(who, false, at(0)) {
                SignIn::Wrong { remaining } => assert_eq!(remaining, expected_remaining),
                other => panic!("expected Wrong, got {other:?}"),
            }
        }
        // The fifth locks out, five minutes from now.
        match lockout.record(who, false, at(1_000)) {
            SignIn::LockedOut { until_ms } => assert_eq!(until_ms, 1_000 + 5 * 60 * 1_000),
            other => panic!("expected LockedOut, got {other:?}"),
        }
    }

    #[test]
    fn a_correct_pin_while_locked_out_is_still_refused() {
        let lockout = Lockout::new();
        let who = employee();
        for _ in 0..MAX_FAILURES {
            let _ = lockout.record(who, false, at(0));
        }
        // Even a correct PIN one second later is refused: the lockout must be served.
        assert!(matches!(
            lockout.record(who, true, at(1_000)),
            SignIn::LockedOut { .. }
        ));
    }

    #[test]
    fn the_lockout_lifts_after_the_window_and_the_count_resets() {
        let lockout = Lockout::new();
        let who = employee();
        for _ in 0..MAX_FAILURES {
            let _ = lockout.record(who, false, at(0));
        }
        // Just past five minutes, a correct PIN is accepted again.
        let after = 5 * 60 * 1_000 + 1;
        assert_eq!(lockout.record(who, true, at(after)), SignIn::Ok);
        // And the counter reset: one failure now reports four remaining, not zero.
        assert_eq!(
            lockout.record(who, false, at(after + 1)),
            SignIn::Wrong {
                remaining: MAX_FAILURES - 1
            }
        );
    }

    #[test]
    fn a_success_resets_the_failure_count() {
        let lockout = Lockout::new();
        let who = employee();
        let _ = lockout.record(who, false, at(0));
        let _ = lockout.record(who, false, at(0));
        assert_eq!(lockout.record(who, true, at(0)), SignIn::Ok);
        // Back to a full allowance.
        assert_eq!(
            lockout.record(who, false, at(0)),
            SignIn::Wrong {
                remaining: MAX_FAILURES - 1
            }
        );
    }

    #[test]
    fn authenticate_combines_verification_and_lockout() {
        let lockout = Lockout::new();
        let who = employee();
        let hash = hash_of("2468");

        assert_eq!(lockout.authenticate(who, &hash, "2468", at(0)), SignIn::Ok);
        assert_eq!(
            lockout.authenticate(who, &hash, "0000", at(0)),
            SignIn::Wrong {
                remaining: MAX_FAILURES - 1
            }
        );
    }
}
