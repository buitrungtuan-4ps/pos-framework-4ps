// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's clock — the one sanctioned reader of the OS time.
//!
//! Everything in `pos_cloud` that needs the time takes it through the [`ClockSource`] port — the API
//! key check verifies expiry against it, for instance — so a test can drive time without a real
//! clock (`clippy.toml` bans `SystemTime::now` everywhere precisely to force this). [`SystemClock`]
//! is the single place that ban is lifted, because a real clock must read the OS somewhere. The
//! cloud runs on a well-maintained VPS with NTP, so it needs no drift correction of its own.

use std::time::{SystemTime, UNIX_EPOCH};

use pos_proto::ClockSource;
use pos_proto::time::Timestamp;

/// A [`ClockSource`] backed by the operating system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl ClockSource for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the ClockSource port's one real implementation in the cloud; the ban exists to route every other reader through here"
    )]
    fn now(&self) -> Timestamp {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Timestamp::from_milliseconds_since_epoch(millis).unwrap_or_else(|_| {
            Timestamp::from_milliseconds_since_epoch(0).expect("epoch is valid")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SystemClock;
    use pos_proto::ClockSource;

    #[test]
    fn the_system_clock_is_past_the_epoch_and_monotone_enough() {
        let clock = SystemClock;
        let first = clock.now().as_milliseconds_since_epoch();
        let second = clock.now().as_milliseconds_since_epoch();
        assert!(
            first > 1_700_000_000_000,
            "the clock is a real wall-clock time"
        );
        assert!(
            second >= first,
            "time does not go backwards between two reads"
        );
    }
}
