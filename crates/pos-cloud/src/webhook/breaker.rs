// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A per-endpoint circuit breaker with 24-hour auto-disable.
//!
//! A webhook receiver that is down must not be hammered, and a receiver that has been down for a
//! long time must stop being tried at all until someone fixes it (`docs/roadmap.md` P7). This is the
//! standard three-state breaker plus a disable rung:
//!
//!  * **Closed** — deliveries flow. Consecutive failures are counted.
//!  * **Open** — after [`BreakerConfig::failure_threshold`] consecutive failures, deliveries are
//!    suppressed for a cooldown, so a dead endpoint is polled once per cooldown, not in a hot loop.
//!  * **Half-open** — after the cooldown, one trial delivery is allowed. Success closes the breaker;
//!    failure re-opens it.
//!  * **Disabled** — once an endpoint has been failing *continuously* for
//!    [`BreakerConfig::auto_disable_after`], it is disabled and never tried again until a human
//!    re-enables it. This is the 24-hour auto-disable.
//!
//! Every transition takes `now` as an argument, so the whole thing is deterministic and tested
//! without a clock — the cloud passes a [`ClockSource`](pos_proto::determinism::ClockSource) time at
//! the call site.

use pos_proto::time::Timestamp;

/// How a breaker is tuned.
#[derive(Debug, Clone, Copy)]
pub struct BreakerConfig {
    /// Consecutive failures that trip the breaker from closed to open.
    pub failure_threshold: u32,
    /// How long the breaker stays open before allowing a half-open trial, in seconds.
    pub open_cooldown: i64,
    /// How long an endpoint may fail continuously before it is disabled outright, in seconds.
    pub auto_disable_after: i64,
}

impl Default for BreakerConfig {
    /// Five strikes, a one-minute cooldown, and disable after a day of continuous failure.
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            open_cooldown: 60,
            auto_disable_after: 24 * 60 * 60,
        }
    }
}

/// The breaker's observable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Deliveries flow.
    Closed,
    /// Deliveries suppressed until the cooldown elapses.
    Open,
    /// One trial delivery is allowed.
    HalfOpen,
    /// Disabled after prolonged failure; needs a manual re-enable.
    Disabled,
}

/// A per-endpoint circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    config: BreakerConfig,
    state: BreakerState,
    consecutive_failures: u32,
    /// When the current unbroken run of failures started, or `None` if the last outcome succeeded.
    failing_since: Option<Timestamp>,
    /// When an open breaker may next allow a trial.
    open_until: Option<Timestamp>,
}

impl CircuitBreaker {
    /// A fresh, closed breaker.
    #[must_use]
    pub fn new(config: BreakerConfig) -> Self {
        Self {
            config,
            state: BreakerState::Closed,
            consecutive_failures: 0,
            failing_since: None,
            open_until: None,
        }
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> BreakerState {
        self.state
    }

    /// Whether the endpoint has been auto-disabled.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.state == BreakerState::Disabled
    }

    /// Whether a delivery may be attempted now.
    ///
    /// Mutating, because an open breaker whose cooldown has elapsed transitions to half-open here and
    /// returns `true` for the one trial. A disabled breaker always returns `false`.
    pub fn allow(&mut self, now: Timestamp) -> bool {
        match self.state {
            BreakerState::Disabled => false,
            BreakerState::Closed | BreakerState::HalfOpen => true,
            BreakerState::Open => {
                let ready = self.open_until.is_none_or(|until| now >= until);
                if ready {
                    self.state = BreakerState::HalfOpen;
                    self.open_until = None;
                }
                ready
            }
        }
    }

    /// Records a delivered-successfully outcome: the breaker closes and the failure run resets.
    pub fn record_success(&mut self) {
        self.state = BreakerState::Closed;
        self.consecutive_failures = 0;
        self.failing_since = None;
        self.open_until = None;
    }

    /// Records a failed delivery at `now`.
    ///
    /// Trips the breaker open at the threshold, re-opens it if a half-open trial failed, and disables
    /// it once the failure run has lasted [`BreakerConfig::auto_disable_after`].
    pub fn record_failure(&mut self, now: Timestamp) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let since = *self.failing_since.get_or_insert(now);

        if elapsed_secs(since, now) >= self.config.auto_disable_after {
            self.state = BreakerState::Disabled;
            self.open_until = None;
            return;
        }

        let was_trial = self.state == BreakerState::HalfOpen;
        if was_trial || self.consecutive_failures >= self.config.failure_threshold {
            self.state = BreakerState::Open;
            self.open_until = Some(plus_secs(now, self.config.open_cooldown));
        }
    }

    /// Re-enables a disabled (or any) breaker, as an operator action.
    pub fn enable(&mut self) {
        *self = Self::new(self.config);
    }
}

/// Whole seconds between two timestamps, clamped at zero (a backwards clock is not negative time).
fn elapsed_secs(since: Timestamp, now: Timestamp) -> i64 {
    now.as_milliseconds_since_epoch()
        .saturating_sub(since.as_milliseconds_since_epoch())
        .div_euclid(1000)
        .max(0)
}

/// `now` plus `secs`, saturating to `now` if the arithmetic would overflow (it never does for real
/// cooldowns, so this only guards the impossible).
fn plus_secs(now: Timestamp, secs: i64) -> Timestamp {
    let millis = now
        .as_milliseconds_since_epoch()
        .saturating_add(secs.saturating_mul(1000));
    Timestamp::from_milliseconds_since_epoch(millis).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::{BreakerConfig, BreakerState, CircuitBreaker};

    use pos_proto::time::Timestamp;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(seconds.saturating_mul(1000)).expect("valid")
    }

    fn config() -> BreakerConfig {
        BreakerConfig {
            failure_threshold: 3,
            open_cooldown: 60,
            auto_disable_after: 24 * 60 * 60,
        }
    }

    #[test]
    fn it_opens_after_the_threshold_of_consecutive_failures() {
        let mut breaker = CircuitBreaker::new(config());
        assert!(breaker.allow(at(0)));
        breaker.record_failure(at(0));
        breaker.record_failure(at(1));
        assert_eq!(
            breaker.state(),
            BreakerState::Closed,
            "under the threshold, still closed"
        );
        assert!(breaker.allow(at(1)));
        breaker.record_failure(at(2));
        assert_eq!(
            breaker.state(),
            BreakerState::Open,
            "the third failure trips it"
        );
        assert!(!breaker.allow(at(2)), "an open breaker suppresses delivery");
    }

    #[test]
    fn it_half_opens_after_the_cooldown_and_a_success_closes_it() {
        let mut breaker = CircuitBreaker::new(config());
        for second in 0..3 {
            breaker.record_failure(at(second));
        }
        assert!(!breaker.allow(at(30)), "still cooling down");
        assert!(
            breaker.allow(at(62)),
            "cooldown elapsed: one trial is allowed"
        );
        assert_eq!(breaker.state(), BreakerState::HalfOpen);
        breaker.record_success();
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert!(breaker.allow(at(63)));
    }

    #[test]
    fn a_failed_half_open_trial_re_opens_immediately() {
        let mut breaker = CircuitBreaker::new(config());
        for second in 0..3 {
            breaker.record_failure(at(second));
        }
        assert!(breaker.allow(at(62)), "half-open trial");
        breaker.record_failure(at(62));
        assert_eq!(
            breaker.state(),
            BreakerState::Open,
            "a failed trial re-opens"
        );
        assert!(!breaker.allow(at(70)));
    }

    #[test]
    fn a_success_resets_the_failure_run_so_the_threshold_is_consecutive() {
        let mut breaker = CircuitBreaker::new(config());
        breaker.record_failure(at(0));
        breaker.record_failure(at(1));
        breaker.record_success();
        breaker.record_failure(at(2));
        breaker.record_failure(at(3));
        assert_eq!(
            breaker.state(),
            BreakerState::Closed,
            "the run reset, so two more failures do not trip it"
        );
    }

    #[test]
    fn it_disables_after_a_day_of_continuous_failure() {
        let mut breaker = CircuitBreaker::new(config());
        breaker.record_failure(at(0));
        // Still failing a whole day later.
        breaker.record_failure(at(24 * 60 * 60));
        assert!(
            breaker.is_disabled(),
            "24h of continuous failure disables the endpoint"
        );
        assert!(
            !breaker.allow(at(24 * 60 * 60 + 1)),
            "a disabled endpoint is never tried"
        );
        // Even long past the cooldown, it stays disabled.
        assert!(!breaker.allow(at(24 * 60 * 60 + 10_000)));
    }

    #[test]
    fn a_success_before_the_day_is_up_prevents_disabling() {
        let mut breaker = CircuitBreaker::new(config());
        breaker.record_failure(at(0));
        breaker.record_success();
        // A fresh failure run starts here, so the day counts from now, not from the first failure.
        breaker.record_failure(at(23 * 60 * 60));
        assert!(!breaker.is_disabled());
    }

    #[test]
    fn a_disabled_breaker_can_be_re_enabled_by_an_operator() {
        let mut breaker = CircuitBreaker::new(config());
        breaker.record_failure(at(0));
        breaker.record_failure(at(24 * 60 * 60));
        assert!(breaker.is_disabled());
        breaker.enable();
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert!(breaker.allow(at(24 * 60 * 60 + 1)));
    }
}
