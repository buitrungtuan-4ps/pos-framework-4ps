// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A sliding-window rate limiter for the interactive sign-in
//! ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 5).
//!
//! Online password/TOTP guessing is throttled here, before the expensive Argon2id verify even runs,
//! so a flood of attempts costs the attacker a `429` rather than the server a hashing storm. The
//! window is *sliding* (a deque of recent attempt instants per key, pruned to the window on each
//! check) rather than fixed, so an attacker cannot burst a fresh allowance the instant a fixed window
//! rolls over.
//!
//! The limiter is keyed by an opaque string, and a single check may present several keys — the
//! attempt is refused if *any* key is over its limit. Today the `/admin/login` route presents only
//! the client IP; when per-admin email login lands, the same call adds an `email:…` key, so the
//! "per email and per IP" limit the ADR calls for needs no change here. State is in-process (the
//! cloud is a single box, P8) and ephemeral — a restart clears it, which fails open, never closed.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use pos_proto::time::Timestamp;

/// Above this many tracked keys, a check first sweeps fully-expired keys out of the map, so a source
/// rotating through many IPs cannot grow it without bound. Cheap because it only runs once the map is
/// already large; a normal console sees a handful of keys and never triggers it.
const SWEEP_LIMIT: usize = 4096;

/// A per-key sliding-window limiter over a shared, in-process map. Cloneable: every clone shares the
/// same window state (an `Arc`), so all request handlers throttle against one counter.
#[derive(Clone, Debug)]
pub struct LoginRateLimiter {
    /// key → the instants (Unix ms) of the attempts still inside the window.
    inner: Arc<Mutex<HashMap<String, VecDeque<i64>>>>,
    /// The most attempts allowed per key within one window.
    max_attempts: usize,
    /// The sliding window, in milliseconds.
    window_ms: i64,
}

impl LoginRateLimiter {
    /// A limiter allowing `max_attempts` per key within a `window_secs` sliding window.
    #[must_use]
    pub fn new(max_attempts: usize, window_secs: u64) -> Self {
        let window_ms = i64::try_from(window_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_attempts,
            window_ms,
        }
    }

    /// Registers an attempt against every key in `keys` as of `now`, unless doing so would exceed the
    /// limit on any of them.
    ///
    /// On success the attempt is recorded; a refusal records nothing, so a refused attempt neither
    /// counts against the caller nor extends their window — it drains purely by the passage of time.
    ///
    /// # Errors
    ///
    /// `Err(retry_secs)` when at least one key is already at its limit — how many whole seconds until
    /// the soonest slot frees (never below 1), suitable for a `Retry-After` header.
    ///
    /// # Panics
    ///
    /// If the internal lock is poisoned, which happens only if another thread panicked while holding
    /// it — the limiter's own code never panics under the lock, so in practice it does not.
    pub fn check_and_record(&self, keys: &[String], now: Timestamp) -> Result<(), u64> {
        let now_ms = now.as_milliseconds_since_epoch();
        let cutoff = now_ms.saturating_sub(self.window_ms);
        let mut map = self
            .inner
            .lock()
            .expect("the rate-limiter lock is not poisoned");

        // Opportunistic global sweep, only once the map is already large — bounds memory against a
        // rotating-IP flood without scanning on every ordinary check.
        if map.len() > SWEEP_LIMIT {
            map.retain(|_key, times| {
                prune(times, cutoff);
                !times.is_empty()
            });
        }

        // Decide first: prune each presented key and see whether any is at its limit. Nothing is
        // recorded yet, so a refusal below leaves the window untouched.
        let mut retry_ms: Option<i64> = None;
        for key in keys {
            let times = map.entry(key.clone()).or_default();
            prune(times, cutoff);
            if times.len() >= self.max_attempts {
                // The soonest this key frees a slot is when its oldest kept attempt leaves the window.
                let oldest = times.front().copied().unwrap_or(now_ms);
                let wait = oldest
                    .saturating_add(self.window_ms)
                    .saturating_sub(now_ms)
                    .max(0);
                retry_ms = Some(retry_ms.map_or(wait, |current| current.max(wait)));
            }
        }
        if let Some(wait_ms) = retry_ms {
            // Refused — record nothing; drop any empty deques the `entry` calls just created.
            map.retain(|_key, times| !times.is_empty());
            // Ceil-divide ms → whole seconds (`div_ceil` is unstable for signed integers), never
            // below 1 so a sub-second wait still tells the client to hold off.
            let secs = (wait_ms.saturating_add(999) / 1000).max(1);
            return Err(u64::try_from(secs).unwrap_or(u64::MAX));
        }
        for key in keys {
            map.entry(key.clone()).or_default().push_back(now_ms);
        }
        Ok(())
    }
}

/// Drops every attempt at or before `cutoff` from the front of the deque (they have left the window).
/// The deque is append-only in time order, so the expired ones are always a prefix.
fn prune(times: &mut VecDeque<i64>, cutoff: i64) {
    while times.front().is_some_and(|&at| at <= cutoff) {
        times.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::LoginRateLimiter;

    use pos_proto::time::Timestamp;

    const NOW_MS: i64 = 1_700_000_000_000;

    fn at(offset_ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(NOW_MS + offset_ms).expect("valid")
    }

    fn key(name: &str) -> Vec<String> {
        vec![name.to_owned()]
    }

    #[test]
    fn allows_up_to_the_limit_then_refuses_with_a_retry_after() {
        let limiter = LoginRateLimiter::new(3, 60);
        for _ in 0..3 {
            assert!(
                limiter.check_and_record(&key("ip:a"), at(0)).is_ok(),
                "the first three attempts are within the limit"
            );
        }
        let refused = limiter
            .check_and_record(&key("ip:a"), at(0))
            .expect_err("the fourth attempt is over the limit");
        assert!(
            (1..=60).contains(&refused),
            "retry-after is a positive number of seconds within the window, got {refused}"
        );
    }

    #[test]
    fn the_window_is_per_key() {
        let limiter = LoginRateLimiter::new(1, 60);
        limiter
            .check_and_record(&key("ip:a"), at(0))
            .expect("first for a");
        assert!(
            limiter.check_and_record(&key("ip:a"), at(0)).is_err(),
            "a second attempt for the same key is refused"
        );
        assert!(
            limiter.check_and_record(&key("ip:b"), at(0)).is_ok(),
            "a different key has its own budget"
        );
    }

    #[test]
    fn a_refusal_does_not_extend_the_window() {
        let limiter = LoginRateLimiter::new(2, 60);
        limiter
            .check_and_record(&key("ip:a"), at(0))
            .expect("first");
        limiter
            .check_and_record(&key("ip:a"), at(0))
            .expect("second");
        // Refused at 30s — this must NOT be recorded, or it would push the window out.
        assert!(limiter.check_and_record(&key("ip:a"), at(30_000)).is_err());
        // At 61s the two real attempts (at 0) have aged out, so a fresh attempt is allowed — proof
        // the refusal at 30s did not count.
        assert!(
            limiter.check_and_record(&key("ip:a"), at(61_000)).is_ok(),
            "the window drains by time alone; a refused attempt does not reset it"
        );
    }

    #[test]
    fn any_over_limit_key_refuses_the_attempt() {
        let limiter = LoginRateLimiter::new(2, 60);
        // Exhaust the shared "email" key via two IPs, then a third IP sharing that email is refused.
        limiter
            .check_and_record(&["ip:a".to_owned(), "email:x".to_owned()], at(0))
            .expect("first");
        limiter
            .check_and_record(&["ip:b".to_owned(), "email:x".to_owned()], at(0))
            .expect("second");
        assert!(
            limiter
                .check_and_record(&["ip:c".to_owned(), "email:x".to_owned()], at(0))
                .is_err(),
            "a fresh IP is still refused because the shared email key is over the limit"
        );
    }
}
