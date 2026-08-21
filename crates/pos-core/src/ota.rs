// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Over-the-air rollout decisions ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)).
//!
//! Once an update artifact is validly signed ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)),
//! this module answers the next question — should *this* device install it *now*? — as one pure,
//! total function, [`decide_rollout`], so the simulator (`docs/roadmap.md` P12) can exhaust it without
//! I/O or hardware. The verdict follows a fixed precedence that *is* the safety argument: recovering a
//! device that failed its self-test outranks the kill switch, and the kill switch outranks eligibility.
//!
//! The rollout shape — which rings exist and how much of the fleet is live — is **published data**
//! (`min_ring` + `fleet_rollout_percent`), not a constant, which is how the roadmap asks us to settle
//! the docs' inconsistent ring count: adding a "25% ring" is setting a number, not shipping a release.
//! The signing key id is the raw `[u8; 8]` minisign uses, because `pos-core` names no port type; the
//! edge maps `pos_ports::signer::KeyId` at the boundary.

/// A semantic release version of the `pos_edge` / `pos_cloud` binary. Ordered `major`, then `minor`,
/// then `patch`, so `<`/`>` are the version comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseVersion {
    /// The major component.
    pub major: u16,
    /// The minor component.
    pub minor: u16,
    /// The patch component.
    pub patch: u16,
}

impl ReleaseVersion {
    /// A version from its three components.
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

/// The deployment rings, ordered least to most exposed: an update reaches `Lab`, then `Pilot`, then
/// the `Fleet` (where the canary percent gates the rest of the ramp).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ring {
    /// Internal test machines — the first to take any update.
    Lab,
    /// A small set of real pilot stores.
    Pilot,
    /// Every other store, rolled out gradually by the canary percent.
    Fleet,
}

/// The signing key id, as the raw eight bytes minisign uses. Raw here — not `pos_ports::signer::KeyId`
/// — because `pos-core` names no port type ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)).
pub type SigningKeyId = [u8; 8];

/// The result of a device's self-test after it installed `version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfTest {
    /// The version the device installed and then tested.
    pub version: ReleaseVersion,
    /// Whether the self-test passed. A failure on the running version forces a rollback.
    pub passed: bool,
}

/// A device's persisted update state: what it runs, where it sits in the fleet, and its last
/// self-test. These are the only facts the edge must remember across a reboot; everything else about
/// an update arrives over the wire in a [`PublishedUpdate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceState {
    /// The version the device is currently running.
    pub current: ReleaseVersion,
    /// Which ring this device belongs to.
    pub ring: Ring,
    /// A stable `0..=99` bucket fixing this device's place in the fleet canary ramp, so its position
    /// does not jump between evaluations.
    pub canary_bucket: u8,
    /// The most recent self-test result, or `None` if it has never self-tested.
    pub last_self_test: Option<SelfTest>,
}

/// An update the cloud has published for the fleet — the target, how far the rollout has reached, the
/// key that signed it, and whether the kill switch is engaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishedUpdate {
    /// The version being rolled out.
    pub target: ReleaseVersion,
    /// The lowest ring currently eligible.
    pub min_ring: Ring,
    /// The fraction of the fleet ring (`0..=100`) currently eligible, ramped over the rollout's life.
    /// Lab and pilot ignore it; only fleet devices are gated by it.
    pub fleet_rollout_percent: u8,
    /// The key id the artifact is signed by, so revocation can be checked before installing.
    pub signing_key_id: SigningKeyId,
    /// The kill switch: when `true`, the rollout is halted and no device installs.
    pub halted: bool,
}

/// Why an update was skipped — the device is healthy, it is just not this update's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The target is not newer than the running version.
    AlreadyCurrent,
    /// The device's ring is below the update's minimum ring.
    NotInRing,
    /// A fleet device whose canary bucket is at or past the current rollout fraction.
    NotInCanaryYet,
}

/// The verdict for one device and one published update
/// ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutDecision {
    /// The running version failed its self-test; revert to the last-good version.
    RollBack,
    /// The kill switch is engaged; hold at the current version.
    Halt,
    /// The update is signed by a revoked key; never install it.
    Refuse,
    /// Install the target version.
    Install,
    /// Not this update's turn for this device.
    Skip(SkipReason),
}

/// Decides what a device should do about a published update, given the revoked-key list
/// ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)).
///
/// Pure and total: exactly one verdict, by the fixed precedence — roll back, already-current, halt,
/// refuse, ring gate, canary gate, install. The order is the safety argument: recovering a broken
/// device outranks the kill switch, and the kill switch outranks eligibility.
#[must_use]
pub fn decide_rollout(
    device: &DeviceState,
    update: &PublishedUpdate,
    revoked_keys: &[SigningKeyId],
) -> RolloutDecision {
    // 1. A device that failed its self-test on the version it is running must revert, whatever else
    //    holds — a bricked store recovering outranks even the kill switch.
    if let Some(self_test) = device.last_self_test
        && self_test.version == device.current
        && !self_test.passed
    {
        return RolloutDecision::RollBack;
    }
    // 2. Nothing newer to weigh.
    if update.target <= device.current {
        return RolloutDecision::Skip(SkipReason::AlreadyCurrent);
    }
    // 3. The operator pulled the brake.
    if update.halted {
        return RolloutDecision::Halt;
    }
    // 4. A valid signature from a retired key is still untrusted (revocation is policy, ADR-0047).
    if revoked_keys.contains(&update.signing_key_id) {
        return RolloutDecision::Refuse;
    }
    // 5. Ring gate.
    if device.ring < update.min_ring {
        return RolloutDecision::Skip(SkipReason::NotInRing);
    }
    // 6. Canary gate — fleet only; lab and pilot are the test cohort and take everything in-ring.
    if device.ring == Ring::Fleet && device.canary_bucket >= update.fleet_rollout_percent {
        return RolloutDecision::Skip(SkipReason::NotInCanaryYet);
    }
    // 7. Eligible.
    RolloutDecision::Install
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceState, PublishedUpdate, ReleaseVersion, Ring, RolloutDecision, SelfTest,
        SigningKeyId, SkipReason, decide_rollout,
    };

    const KEY: SigningKeyId = [0xA1; 8];
    const OTHER_KEY: SigningKeyId = [0xB2; 8];

    fn v(major: u16, minor: u16, patch: u16) -> ReleaseVersion {
        ReleaseVersion::new(major, minor, patch)
    }

    /// A healthy fleet device running 1.0.0, fully inside the canary ramp.
    fn device(ring: Ring) -> DeviceState {
        DeviceState {
            current: v(1, 0, 0),
            ring,
            canary_bucket: 0,
            last_self_test: None,
        }
    }

    /// An update to 1.1.0 for the whole fleet, signed by `KEY`, brake off.
    fn update() -> PublishedUpdate {
        PublishedUpdate {
            target: v(1, 1, 0),
            min_ring: Ring::Lab,
            fleet_rollout_percent: 100,
            signing_key_id: KEY,
            halted: false,
        }
    }

    #[test]
    fn versions_order_by_major_then_minor_then_patch() {
        assert!(v(1, 0, 0) < v(1, 0, 1));
        assert!(v(1, 2, 0) < v(2, 0, 0));
        assert!(v(1, 10, 0) > v(1, 9, 9));
    }

    #[test]
    fn an_eligible_device_installs() {
        assert_eq!(
            decide_rollout(&device(Ring::Fleet), &update(), &[]),
            RolloutDecision::Install
        );
    }

    #[test]
    fn a_failed_self_test_on_the_running_version_rolls_back_over_everything() {
        let mut d = device(Ring::Fleet);
        d.last_self_test = Some(SelfTest {
            version: v(1, 0, 0),
            passed: false,
        });
        // Even with the kill switch on and a revoked key, recovery wins.
        let mut u = update();
        u.halted = true;
        assert_eq!(
            decide_rollout(&d, &u, &[KEY]),
            RolloutDecision::RollBack,
            "a store that failed self-test must revert whatever else is true"
        );
    }

    #[test]
    fn a_passed_self_test_does_not_roll_back() {
        let mut d = device(Ring::Fleet);
        d.last_self_test = Some(SelfTest {
            version: v(1, 0, 0),
            passed: true,
        });
        assert_eq!(decide_rollout(&d, &update(), &[]), RolloutDecision::Install);
    }

    #[test]
    fn a_failed_self_test_for_an_old_version_is_ignored() {
        // The failure was on 0.9.0; the device recovered to 1.0.0 since, so it is not rolling back now.
        let mut d = device(Ring::Fleet);
        d.last_self_test = Some(SelfTest {
            version: v(0, 9, 0),
            passed: false,
        });
        assert_eq!(decide_rollout(&d, &update(), &[]), RolloutDecision::Install);
    }

    #[test]
    fn already_current_skips_before_halt_or_refuse() {
        let mut u = update();
        u.target = v(1, 0, 0); // not newer than the device's 1.0.0
        u.halted = true;
        assert_eq!(
            decide_rollout(&device(Ring::Fleet), &u, &[KEY]),
            RolloutDecision::Skip(SkipReason::AlreadyCurrent),
            "nothing newer to install means Skip, not Halt or Refuse"
        );
    }

    #[test]
    fn the_kill_switch_halts_an_otherwise_eligible_device() {
        let mut u = update();
        u.halted = true;
        assert_eq!(
            decide_rollout(&device(Ring::Fleet), &u, &[]),
            RolloutDecision::Halt
        );
    }

    #[test]
    fn a_revoked_signing_key_is_refused() {
        let mut u = update();
        u.signing_key_id = OTHER_KEY;
        assert_eq!(
            decide_rollout(&device(Ring::Fleet), &u, &[OTHER_KEY]),
            RolloutDecision::Refuse,
            "a valid signature from a retired key is still untrusted"
        );
    }

    #[test]
    fn a_device_below_the_minimum_ring_waits() {
        let mut u = update();
        u.min_ring = Ring::Pilot;
        assert_eq!(
            decide_rollout(&device(Ring::Lab), &u, &[]),
            RolloutDecision::Skip(SkipReason::NotInRing)
        );
    }

    #[test]
    fn lab_and_pilot_ignore_the_canary_percent_but_fleet_obeys_it() {
        let mut u = update();
        u.fleet_rollout_percent = 0; // nobody in the fleet ring yet
        // Lab and pilot are the test cohort — they take it regardless of the fleet percent.
        assert_eq!(
            decide_rollout(&device(Ring::Lab), &u, &[]),
            RolloutDecision::Install
        );
        assert_eq!(
            decide_rollout(&device(Ring::Pilot), &u, &[]),
            RolloutDecision::Install
        );
        // A fleet device at bucket 0 with percent 0 is not in the ramp yet.
        assert_eq!(
            decide_rollout(&device(Ring::Fleet), &u, &[]),
            RolloutDecision::Skip(SkipReason::NotInCanaryYet)
        );
    }

    #[test]
    fn the_canary_bucket_gates_the_fleet_ramp() {
        for (bucket, percent, eligible) in [
            (0_u8, 25_u8, true),
            (24, 25, true),
            (25, 25, false),
            (99, 100, true),
            (0, 0, false),
        ] {
            let mut d = device(Ring::Fleet);
            d.canary_bucket = bucket;
            let mut u = update();
            u.fleet_rollout_percent = percent;
            let decision = decide_rollout(&d, &u, &[]);
            let installed = decision == RolloutDecision::Install;
            assert_eq!(
                installed, eligible,
                "bucket {bucket} at percent {percent} should install = {eligible}"
            );
        }
    }

    #[test]
    fn ring_eligibility_is_monotonic_ignoring_the_fleet_canary() {
        // A device at or above min_ring is never Skipped for NotInRing, at every ring.
        let mut u = update();
        u.min_ring = Ring::Pilot;
        for ring in [Ring::Pilot, Ring::Fleet] {
            let decision = decide_rollout(&device(ring), &u, &[]);
            assert_ne!(
                decision,
                RolloutDecision::Skip(SkipReason::NotInRing),
                "{ring:?} is at or above the minimum ring, so it is ring-eligible"
            );
        }
    }
}
