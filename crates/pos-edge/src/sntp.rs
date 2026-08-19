// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! SNTP clock-drift monitoring.
//!
//! The edge's clock is the OS clock ([`crate::clock`]). An SNTP poll measures the **offset** between
//! it and a reference time server; a large offset means the store's clock has drifted, which is not
//! cosmetic — the business date is derived from the store's local time
//! ([ADR-0014](../../../docs/adr/0014-datetime-library.md)), so a clock two minutes fast can file a
//! sale under the wrong trading day.
//!
//! The **poll** is network I/O and, like mDNS ([`crate::discovery`]), lands with deployment — it
//! cannot be exercised in CI. The **decision** it feeds — is this offset an alarm? — is pure and
//! lives here, so the threshold is tested without a network.

use std::time::Duration;

/// The offset past which the clock is considered adrift and an alarm is raised. Two seconds is well
/// inside the tolerance business-date derivation needs and well outside ordinary NTP jitter.
pub const DRIFT_ALARM: Duration = Duration::from_secs(2);

/// The assessment of one measured offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drift {
    /// Within tolerance; the offset is reported for the metrics sink.
    Ok {
        /// The measured offset (edge clock minus reference), in milliseconds.
        offset_ms: i64,
    },
    /// Beyond [`DRIFT_ALARM`]; the operator and the cloud should be alerted.
    Alarm {
        /// The measured offset (edge clock minus reference), in milliseconds.
        offset_ms: i64,
    },
}

/// Assesses a measured offset (edge clock minus reference), in milliseconds.
#[must_use]
pub fn assess(offset_ms: i64) -> Drift {
    let threshold_ms = u64::try_from(DRIFT_ALARM.as_millis()).unwrap_or(u64::MAX);
    if offset_ms.unsigned_abs() > threshold_ms {
        Drift::Alarm { offset_ms }
    } else {
        Drift::Ok { offset_ms }
    }
}

#[cfg(test)]
mod tests {
    use super::{Drift, assess};

    #[test]
    fn a_small_offset_either_way_is_within_tolerance() {
        assert_eq!(assess(0), Drift::Ok { offset_ms: 0 });
        assert_eq!(assess(1_999), Drift::Ok { offset_ms: 1_999 });
        assert_eq!(assess(-1_999), Drift::Ok { offset_ms: -1_999 });
    }

    #[test]
    fn an_offset_past_two_seconds_either_way_alarms() {
        assert_eq!(assess(2_001), Drift::Alarm { offset_ms: 2_001 });
        assert_eq!(assess(-60_000), Drift::Alarm { offset_ms: -60_000 });
    }

    #[test]
    fn exactly_the_threshold_is_not_yet_an_alarm() {
        assert_eq!(assess(2_000), Drift::Ok { offset_ms: 2_000 });
    }
}
