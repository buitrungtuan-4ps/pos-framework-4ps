// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! How configuration reaches this store, and how fast
//! ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md) decision 6
//! and consequences): whether the store hears the cloud's wake or runs on the poll alone, and the
//! time from each wake to applying its version, for the heartbeat to report.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

/// The most wake-to-apply times kept: the latest ones, so the report follows the store as it is now.
pub const WAKE_SAMPLES_MAX: usize = 128;

/// How a published change reaches the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigDelivery {
    /// On the 30-second poll alone: no bus, or the bus is down.
    #[default]
    Poll,
    /// On the cloud's wake, with the poll behind it.
    Wake,
}

impl ConfigDelivery {
    /// The wire name.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Poll => "CONFIG_DELIVERY_POLL",
            Self::Wake => "CONFIG_DELIVERY_WAKE",
        }
    }
}

/// What the heartbeat says about config delivery.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigWakeReport {
    /// How changes reach the store now.
    pub delivery: ConfigDelivery,
    /// How many wake-to-apply times the figures below are drawn from, at most [`WAKE_SAMPLES_MAX`].
    pub samples: usize,
    /// The 95th percentile of those times, in milliseconds; `None` with no samples.
    pub p95_ms: Option<u64>,
    /// The longest of them.
    pub max_ms: Option<u64>,
}

#[derive(Debug, Default)]
struct Inner {
    delivery: ConfigDelivery,
    samples: VecDeque<u64>,
}

/// The delivery standing, shared between the config loop that records it and the heartbeat.
#[derive(Debug, Default)]
pub struct ConfigWakeStanding {
    inner: Mutex<Inner>,
}

impl ConfigWakeStanding {
    /// Records how changes reach the store now.
    pub fn set_delivery(&self, delivery: ConfigDelivery) {
        self.lock().delivery = delivery;
    }

    /// Records one wake-to-apply time, dropping the oldest past [`WAKE_SAMPLES_MAX`].
    pub fn record_apply_ms(&self, ms: u64) {
        let mut inner = self.lock();
        if inner.samples.len() == WAKE_SAMPLES_MAX {
            inner.samples.pop_front();
        }
        inner.samples.push_back(ms);
    }

    /// The standing as the heartbeat reports it.
    #[must_use]
    pub fn report(&self) -> ConfigWakeReport {
        let inner = self.lock();
        let mut sorted: Vec<u64> = inner.samples.iter().copied().collect();
        sorted.sort_unstable();
        // The nearest-rank percentile: the smallest sample at or above 95% of them.
        let rank = (sorted.len() * 95).div_ceil(100);
        ConfigWakeReport {
            delivery: inner.delivery,
            samples: sorted.len(),
            p95_ms: rank.checked_sub(1).and_then(|at| sorted.get(at).copied()),
            max_ms: sorted.last().copied(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigDelivery, ConfigWakeReport, ConfigWakeStanding, WAKE_SAMPLES_MAX};

    #[test]
    fn the_report_is_the_nearest_rank_p95_of_the_latest_samples() {
        let standing = ConfigWakeStanding::default();
        assert_eq!(
            standing.report(),
            ConfigWakeReport::default(),
            "the poll, no samples"
        );

        standing.set_delivery(ConfigDelivery::Wake);
        for ms in 1..=100 {
            standing.record_apply_ms(ms);
        }
        let report = standing.report();
        assert_eq!(report.delivery, ConfigDelivery::Wake);
        assert_eq!(
            (report.samples, report.p95_ms, report.max_ms),
            (100, Some(95), Some(100))
        );

        for _ in 0..WAKE_SAMPLES_MAX {
            standing.record_apply_ms(7);
        }
        let report = standing.report();
        assert_eq!(
            (report.samples, report.p95_ms, report.max_ms),
            (WAKE_SAMPLES_MAX, Some(7), Some(7)),
            "bounded, and the oldest went first"
        );
    }
}
