// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Fleet-scale OTA rollout, over the framework's real decision.
//!
//! `docs/roadmap.md` P12 requires the simulator to prove a ring rollout, and P9's exit wants the
//! canary ramp, the failed self-test rolling back, and the kill switch shown across a whole fleet — not
//! one device. [`simulate_rollout`] folds [`pos_core::ota::decide_rollout`] over a fleet and returns the
//! aggregate, so the scenarios exercise the *shipped* pure decision ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md))
//! rather than a re-implementation that could quietly disagree with it. `crates/pos-core/tests/ota_rollout.rs`
//! seeded these against a hand-built fleet while pos-core was the only crate; this is their home.

use pos_core::ota::{DeviceState, PublishedUpdate, RolloutDecision, SigningKeyId, decide_rollout};

/// How a whole fleet responded to one published update: a count per [`RolloutDecision`].
///
/// The counts partition the fleet — every device lands in exactly one bucket — so
/// [`Self::total`] equals the fleet size, which is the invariant the scenarios lean on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RolloutSummary {
    /// Devices that install the target.
    pub install: usize,
    /// Devices reverting after a failed self-test.
    pub roll_back: usize,
    /// Devices holding because the kill switch is engaged.
    pub halt: usize,
    /// Devices refusing a revoked-key signature.
    pub refuse: usize,
    /// Devices for which it is simply not this update's turn.
    pub skip: usize,
}

impl RolloutSummary {
    /// The fleet size — every device is counted in exactly one bucket.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.install + self.roll_back + self.halt + self.refuse + self.skip
    }
}

/// Folds [`decide_rollout`] over `fleet` and returns the aggregate.
#[must_use]
pub fn simulate_rollout(
    fleet: &[DeviceState],
    update: &PublishedUpdate,
    revoked_keys: &[SigningKeyId],
) -> RolloutSummary {
    let mut summary = RolloutSummary::default();
    for device in fleet {
        match decide_rollout(device, update, revoked_keys) {
            RolloutDecision::Install => summary.install += 1,
            RolloutDecision::RollBack => summary.roll_back += 1,
            RolloutDecision::Halt => summary.halt += 1,
            RolloutDecision::Refuse => summary.refuse += 1,
            RolloutDecision::Skip(_reason) => summary.skip += 1,
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::{RolloutSummary, simulate_rollout};
    use pos_core::ota::{
        DeviceState, PublishedUpdate, ReleaseVersion, Ring, SelfTest, SigningKeyId,
    };

    const KEY: SigningKeyId = [0xA1; 8];
    const RETIRED_KEY: SigningKeyId = [0xB2; 8];

    fn version(major: u16, minor: u16, patch: u16) -> ReleaseVersion {
        ReleaseVersion::new(major, minor, patch)
    }

    /// A healthy device on 1.0.0 in `ring` at canary `bucket`, never self-tested.
    fn healthy(ring: Ring, bucket: u8) -> DeviceState {
        DeviceState {
            current: version(1, 0, 0),
            ring,
            canary_bucket: bucket,
            last_self_test: None,
        }
    }

    /// The three rings as one fleet: 5 lab, 5 pilot, and `fleet_size` fleet devices spread across the
    /// canary buckets `0..fleet_size`.
    fn mixed_fleet(fleet_size: u8) -> Vec<DeviceState> {
        let mut fleet: Vec<DeviceState> = (0..5).map(|b| healthy(Ring::Lab, b)).collect();
        fleet.extend((0..5).map(|b| healthy(Ring::Pilot, b)));
        fleet.extend((0..fleet_size).map(|b| healthy(Ring::Fleet, b)));
        fleet
    }

    fn update(fleet_rollout_percent: u8, halted: bool) -> PublishedUpdate {
        PublishedUpdate {
            target: version(1, 1, 0),
            min_ring: Ring::Lab,
            fleet_rollout_percent,
            signing_key_id: KEY,
            halted,
        }
    }

    #[test]
    fn the_canary_gates_the_fleet_while_lab_and_pilot_take_it_whole() {
        // 100 fleet devices at buckets 0..100, rollout at 25% → 25 fleet installs; lab (5) and pilot
        // (5) ignore the canary and take it → 35 installs, 65 waiting on the ramp.
        let fleet = mixed_fleet(100);
        let summary = simulate_rollout(&fleet, &update(25, false), &[]);
        assert_eq!(
            summary,
            RolloutSummary {
                install: 35,
                skip: 75,
                ..RolloutSummary::default()
            }
        );
        assert_eq!(summary.total(), 110, "every device is accounted for");
    }

    #[test]
    fn a_full_ramp_installs_across_the_whole_fleet() {
        let fleet = mixed_fleet(100);
        let summary = simulate_rollout(&fleet, &update(100, false), &[]);
        assert_eq!(summary.install, 110, "at 100% the whole fleet is eligible");
        assert_eq!(summary.skip, 0);
    }

    #[test]
    fn the_kill_switch_halts_every_ring_at_once() {
        let fleet = mixed_fleet(100);
        let summary = simulate_rollout(&fleet, &update(100, true), &[]);
        assert_eq!(summary.halt, 110, "the brake reaches lab and pilot too");
        assert_eq!(summary.install, 0);
    }

    #[test]
    fn a_revoked_key_is_refused_fleet_wide_even_at_full_ramp() {
        let fleet = mixed_fleet(100);
        // Signed by the retired key, which is on the revocation list.
        let published = PublishedUpdate {
            signing_key_id: RETIRED_KEY,
            ..update(100, false)
        };
        let summary = simulate_rollout(&fleet, &published, &[RETIRED_KEY]);
        assert_eq!(summary.refuse, 110, "a retired key installs on no one");
        assert_eq!(summary.install, 0);
    }

    #[test]
    fn a_failed_self_test_rolls_a_device_back_over_everything_else() {
        // One fleet device bricked on its current version; the rest are healthy at full ramp.
        let mut fleet = mixed_fleet(10);
        fleet.push(DeviceState {
            current: version(1, 0, 0),
            ring: Ring::Fleet,
            canary_bucket: 0,
            last_self_test: Some(SelfTest {
                version: version(1, 0, 0),
                passed: false,
            }),
        });
        let summary = simulate_rollout(&fleet, &update(100, false), &[]);
        assert_eq!(summary.roll_back, 1, "the bricked device reverts");
        assert_eq!(summary.install, 20, "the healthy 20 install");
        assert_eq!(summary.total(), 21);
    }
}
