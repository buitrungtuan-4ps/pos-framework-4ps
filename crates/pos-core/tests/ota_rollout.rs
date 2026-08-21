// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! OTA rollout scenarios over a virtual fleet ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)).
//!
//! `docs/roadmap.md` P9 requires that a simulator prove a ring rollout, a failed self-test rolling
//! back, and the kill switch. The full `pos-simulator` is P12; these are the same scenarios seeded now
//! against the pure decision — [`pos_core::ota::decide_rollout`] — with no I/O, no hardware, and no
//! clock, so the safety properties are pinned before the fleet plumbing exists to exercise them for
//! real. Each scenario drives a whole fleet of `DeviceState`s through one `PublishedUpdate` and asserts
//! the aggregate behaviour, not just one device.

use pos_core::ota::{
    DeviceState, PublishedUpdate, ReleaseVersion, Ring, RolloutDecision, SelfTest, SigningKeyId,
    SkipReason, decide_rollout,
};

/// The key the published updates are signed by.
const KEY: SigningKeyId = [0xA1; 8];
/// A retired key, for the revocation scenario.
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

/// An update to 1.1.0 with a given fleet ramp, minimum ring, and kill-switch state, signed by [`KEY`].
fn update(fleet_rollout_percent: u8, min_ring: Ring, halted: bool) -> PublishedUpdate {
    PublishedUpdate {
        target: version(1, 1, 0),
        min_ring,
        fleet_rollout_percent,
        signing_key_id: KEY,
        halted,
    }
}

fn installs(device: &DeviceState, published: &PublishedUpdate, revoked: &[SigningKeyId]) -> bool {
    decide_rollout(device, published, revoked) == RolloutDecision::Install
}

#[test]
fn the_fleet_canary_ramps_while_lab_and_pilot_take_it_immediately() {
    let lab: Vec<DeviceState> = (0..5_u8).map(|bucket| healthy(Ring::Lab, bucket)).collect();
    let pilot: Vec<DeviceState> = (0..5_u8)
        .map(|bucket| healthy(Ring::Pilot, bucket))
        .collect();
    let fleet: Vec<DeviceState> = (0..100_u8)
        .map(|bucket| healthy(Ring::Fleet, bucket))
        .collect();

    let mut previous_fleet_installs = 0_usize;
    for percent in [0_u8, 10, 25, 50, 100] {
        let published = update(percent, Ring::Lab, false);

        // Lab and pilot are the test cohort — canary-exempt, so they take the update at every stage,
        // including 0%. This is the "reaches lab and pilot first" half of the rollout.
        assert!(
            lab.iter().all(|device| installs(device, &published, &[])),
            "every lab device installs at {percent}%"
        );
        assert!(
            pilot.iter().all(|device| installs(device, &published, &[])),
            "every pilot device installs at {percent}%"
        );

        // The fleet ramps by canary bucket: a device installs iff its bucket is below the percent, and
        // one at or above it is explicitly waiting its turn — a healthy Skip, not a failure.
        let mut fleet_installs = 0_usize;
        for device in &fleet {
            let decision = decide_rollout(device, &published, &[]);
            if usize::from(device.canary_bucket) < usize::from(percent) {
                assert_eq!(decision, RolloutDecision::Install);
                fleet_installs += 1;
            } else {
                assert_eq!(decision, RolloutDecision::Skip(SkipReason::NotInCanaryYet));
            }
        }
        assert_eq!(
            fleet_installs,
            usize::from(percent),
            "exactly the buckets below {percent}% are live"
        );
        assert!(
            fleet_installs >= previous_fleet_installs,
            "the ramp only ever grows"
        );
        previous_fleet_installs = fleet_installs;
    }
    assert_eq!(
        previous_fleet_installs, 100,
        "at 100% the whole fleet is live"
    );
}

#[test]
fn raising_the_minimum_ring_holds_the_lower_rings_back() {
    // A targeted rollout with a pilot floor: lab waits, pilot and fleet (fully ramped) take it.
    let published = update(100, Ring::Pilot, false);
    assert_eq!(
        decide_rollout(&healthy(Ring::Lab, 0), &published, &[]),
        RolloutDecision::Skip(SkipReason::NotInRing),
        "a lab device below the pilot floor waits"
    );
    assert!(installs(&healthy(Ring::Pilot, 0), &published, &[]));
    assert!(installs(&healthy(Ring::Fleet, 0), &published, &[]));
}

#[test]
fn a_failed_self_test_rolls_back_over_everything() {
    let mut sick = healthy(Ring::Fleet, 0);
    sick.last_self_test = Some(SelfTest {
        version: version(1, 0, 0),
        passed: false,
    });
    // The kill switch is on and the key is revoked, and it still rolls back — recovery outranks all.
    let published = update(0, Ring::Lab, true);
    assert_eq!(
        decide_rollout(&sick, &published, &[RETIRED_KEY]),
        RolloutDecision::RollBack,
        "a device that failed self-test on its running version reverts, whatever else holds"
    );
    // A healthy peer under the same halted update merely holds — proving rollback is not the default.
    assert_eq!(
        decide_rollout(&healthy(Ring::Fleet, 0), &published, &[]),
        RolloutDecision::Halt
    );
}

#[test]
fn the_kill_switch_freezes_an_otherwise_eligible_fleet() {
    let fleet: Vec<DeviceState> = (0..20_u8)
        .map(|bucket| healthy(Ring::Fleet, bucket))
        .chain([healthy(Ring::Lab, 0), healthy(Ring::Pilot, 0)])
        .collect();

    let halted = update(100, Ring::Lab, true);
    assert!(
        fleet
            .iter()
            .all(|device| decide_rollout(device, &halted, &[]) == RolloutDecision::Halt),
        "with the brake on, every healthy device halts"
    );

    // With the brake off, the same fleet is fully eligible — so the freeze was the switch, not the ramp.
    let live = update(100, Ring::Lab, false);
    assert!(fleet.iter().all(|device| installs(device, &live, &[])));
}

#[test]
fn a_revoked_signing_key_is_refused_across_the_fleet() {
    let fleet: Vec<DeviceState> = (0..10_u8)
        .map(|bucket| healthy(Ring::Fleet, bucket))
        .collect();
    let published = update(100, Ring::Lab, false); // signed by KEY, fully ramped
    assert!(
        fleet
            .iter()
            .all(|device| decide_rollout(device, &published, &[KEY]) == RolloutDecision::Refuse),
        "a valid signature from a retired key is refused even at full ramp"
    );
}
