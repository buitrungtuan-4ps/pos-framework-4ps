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
//!
//! # The rollout is published as configuration
//!
//! The cloud does not push an update; it publishes the rollout *shape* into the config tree
//! ([ADR-0033](../../../docs/adr/0033-config-tree.md), [ADR-0052](../../../docs/adr/0052-ota-rollout-config.md))
//! and each store pulls it. [`FleetUpdateConfig`] and [`DeviceOtaConfig`] are the typed views of the
//! two config keys — `fleet_update` (the fleet-wide target, ring gate, ramp, kill switch, and revoked
//! keys) and `device_ota` (this device's ring and canary bucket) — and their `validate` methods are
//! the *shared* rules the cloud runs before publishing and the edge runs before trusting, so the two
//! cannot disagree about what a legal rollout looks like. Parsing lives here, beside the decision it
//! feeds; deserialising the surrounding document is the caller's I/O.

use serde::Deserialize;

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

impl Ring {
    /// The `snake_case` wire token for this ring, as it appears in configuration.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Lab => "lab",
            Self::Pilot => "pilot",
            Self::Fleet => "fleet",
        }
    }

    /// The ring named by a wire token, or `None` if it names no ring.
    #[must_use]
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "lab" => Some(Self::Lab),
            "pilot" => Some(Self::Pilot),
            "fleet" => Some(Self::Fleet),
            _ => None,
        }
    }
}

impl ReleaseVersion {
    /// Parses a `MAJOR.MINOR.PATCH` string, or `None` if it is not exactly three `u16` components.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let major = parts.next()?.parse::<u16>().ok()?;
        let minor = parts.next()?.parse::<u16>().ok()?;
        let patch = parts.next()?.parse::<u16>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self::new(major, minor, patch))
    }
}

/// Parses a signing key id from exactly sixteen hex digits (the eight-byte minisign key id), or
/// `None` if the text is not sixteen hex digits.
#[must_use]
pub fn parse_signing_key_id(text: &str) -> Option<SigningKeyId> {
    if text.len() != 16 {
        return None;
    }
    let mut bytes: SigningKeyId = [0; 8];
    let mut digits = text.chars();
    for byte in &mut bytes {
        let high = digits.next()?.to_digit(16)?;
        let low = digits.next()?.to_digit(16)?;
        *byte = u8::try_from(high.saturating_mul(16).saturating_add(low)).ok()?;
    }
    Some(bytes)
}

/// The fleet-wide rollout, parsed from a [`FleetUpdateConfig`]: the update to weigh and the keys that
/// are no longer trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRollout {
    /// The published update every device measures itself against.
    pub update: PublishedUpdate,
    /// The signing keys revocation has retired; an update signed by one of these is refused.
    pub revoked_keys: Vec<SigningKeyId>,
}

/// The `fleet_update` configuration key: the rollout the cloud publishes for the whole fleet.
///
/// A typed view of the config value, so the cloud validates it before publishing and the edge parses
/// it before trusting, both through [`FleetUpdateConfig::validate`].
#[derive(Debug, Clone, Deserialize)]
pub struct FleetUpdateConfig {
    /// The target version, as `MAJOR.MINOR.PATCH`.
    pub target_version: String,
    /// The lowest eligible ring, as `lab`, `pilot`, or `fleet`.
    pub min_ring: String,
    /// The fraction of the fleet ring currently eligible, `0..=100`.
    pub rollout_percent: u8,
    /// Whether the kill switch is engaged. Absent means not halted.
    #[serde(default)]
    pub halted: bool,
    /// The signing key id the artifact is signed by, as sixteen hex digits.
    pub signing_key_id: String,
    /// The signing key ids revocation has retired, each sixteen hex digits. Absent means none.
    #[serde(default)]
    pub revoked_key_ids: Vec<String>,
}

impl FleetUpdateConfig {
    /// Validates and parses the fleet rollout.
    ///
    /// # Errors
    ///
    /// Every human-readable violation (not just the first), so an operator fixing a rejected config
    /// sees the whole list: a version, ring, percent, or key id that does not parse.
    pub fn validate(&self) -> Result<FleetRollout, Vec<String>> {
        let mut violations = Vec::new();
        let target = ReleaseVersion::parse(&self.target_version);
        if target.is_none() {
            violations.push(format!(
                "fleet_update.target_version is not MAJOR.MINOR.PATCH: {}",
                self.target_version
            ));
        }
        let min_ring = Ring::from_wire(&self.min_ring);
        if min_ring.is_none() {
            violations.push(format!(
                "fleet_update.min_ring must be lab, pilot, or fleet: {}",
                self.min_ring
            ));
        }
        if self.rollout_percent > 100 {
            violations.push(format!(
                "fleet_update.rollout_percent must be 0..=100: {}",
                self.rollout_percent
            ));
        }
        let signing_key_id = parse_signing_key_id(&self.signing_key_id);
        if signing_key_id.is_none() {
            violations.push(format!(
                "fleet_update.signing_key_id must be sixteen hex digits: {}",
                self.signing_key_id
            ));
        }
        let mut revoked_keys = Vec::with_capacity(self.revoked_key_ids.len());
        for id in &self.revoked_key_ids {
            match parse_signing_key_id(id) {
                Some(key) => revoked_keys.push(key),
                None => violations.push(format!(
                    "fleet_update.revoked_key_ids has an id that is not sixteen hex digits: {id}"
                )),
            }
        }
        // Every field parsed, so these are all `Some`; the guard keeps the assembly panic-free.
        let (Some(target), Some(min_ring), Some(signing_key_id)) =
            (target, min_ring, signing_key_id)
        else {
            return Err(violations);
        };
        if !violations.is_empty() {
            return Err(violations);
        }
        Ok(FleetRollout {
            update: PublishedUpdate {
                target,
                min_ring,
                fleet_rollout_percent: self.rollout_percent,
                signing_key_id,
                halted: self.halted,
            },
            revoked_keys,
        })
    }
}

/// A device's rollout placement, parsed from a [`DeviceOtaConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceOtaAssignment {
    /// The ring the cloud has placed this device in.
    pub ring: Ring,
    /// The device's stable `0..=99` canary bucket.
    pub canary_bucket: u8,
}

/// The `device_ota` configuration key: this device's ring assignment and canary bucket.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceOtaConfig {
    /// The ring, as `lab`, `pilot`, or `fleet`.
    pub ring: String,
    /// The stable canary bucket, `0..=99`.
    pub canary_bucket: u8,
}

impl DeviceOtaConfig {
    /// Validates and parses the device's rollout placement.
    ///
    /// # Errors
    ///
    /// Every human-readable violation: a ring that names no ring, or a bucket outside `0..=99`.
    pub fn validate(&self) -> Result<DeviceOtaAssignment, Vec<String>> {
        let mut violations = Vec::new();
        let ring = Ring::from_wire(&self.ring);
        if ring.is_none() {
            violations.push(format!(
                "device_ota.ring must be lab, pilot, or fleet: {}",
                self.ring
            ));
        }
        if self.canary_bucket > 99 {
            violations.push(format!(
                "device_ota.canary_bucket must be 0..=99: {}",
                self.canary_bucket
            ));
        }
        let Some(ring) = ring else {
            return Err(violations);
        };
        if !violations.is_empty() {
            return Err(violations);
        }
        Ok(DeviceOtaAssignment {
            ring,
            canary_bucket: self.canary_bucket,
        })
    }
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

#[cfg(test)]
mod config_tests {
    use super::{
        DeviceOtaConfig, FleetUpdateConfig, PublishedUpdate, ReleaseVersion, Ring,
        parse_signing_key_id,
    };

    #[test]
    fn rings_round_trip_through_their_wire_tokens() {
        for ring in [Ring::Lab, Ring::Pilot, Ring::Fleet] {
            assert_eq!(Ring::from_wire(ring.as_wire()), Some(ring));
        }
        assert_eq!(Ring::from_wire("canary"), None);
        assert_eq!(Ring::from_wire("Lab"), None, "the token is lower-case");
    }

    #[test]
    fn version_parsing_needs_exactly_three_numeric_components() {
        assert_eq!(
            ReleaseVersion::parse("1.2.3"),
            Some(ReleaseVersion::new(1, 2, 3))
        );
        assert_eq!(
            ReleaseVersion::parse("10.0.255"),
            Some(ReleaseVersion::new(10, 0, 255))
        );
        assert_eq!(ReleaseVersion::parse("1.2"), None);
        assert_eq!(ReleaseVersion::parse("1.2.3.4"), None);
        assert_eq!(ReleaseVersion::parse("1.2.x"), None);
        assert_eq!(ReleaseVersion::parse(""), None);
    }

    #[test]
    fn a_key_id_is_exactly_sixteen_hex_digits() {
        assert_eq!(parse_signing_key_id("a1a1a1a1a1a1a1a1"), Some([0xA1; 8]));
        assert_eq!(
            parse_signing_key_id("00000000000000ff"),
            Some([0, 0, 0, 0, 0, 0, 0, 0xFF])
        );
        assert_eq!(parse_signing_key_id("a1a1a1a1a1a1a1"), None, "too short");
        assert_eq!(parse_signing_key_id("a1a1a1a1a1a1a1a1a1"), None, "too long");
        assert_eq!(parse_signing_key_id("a1a1a1a1a1a1a1zz"), None, "not hex");
    }

    fn valid_fleet() -> FleetUpdateConfig {
        FleetUpdateConfig {
            target_version: "1.2.3".to_owned(),
            min_ring: "pilot".to_owned(),
            rollout_percent: 40,
            halted: false,
            signing_key_id: "a1a1a1a1a1a1a1a1".to_owned(),
            revoked_key_ids: vec!["b2b2b2b2b2b2b2b2".to_owned()],
        }
    }

    #[test]
    fn a_valid_fleet_update_parses_into_the_published_update_and_revocation_list() {
        let rollout = valid_fleet().validate().expect("a valid config parses");
        assert_eq!(
            rollout.update,
            PublishedUpdate {
                target: ReleaseVersion::new(1, 2, 3),
                min_ring: Ring::Pilot,
                fleet_rollout_percent: 40,
                signing_key_id: [0xA1; 8],
                halted: false,
            }
        );
        assert_eq!(rollout.revoked_keys, vec![[0xB2; 8]]);
    }

    #[test]
    fn an_incoherent_fleet_update_reports_every_violation_at_once() {
        let bad = FleetUpdateConfig {
            target_version: "not-a-version".to_owned(),
            min_ring: "canary".to_owned(),
            rollout_percent: 150,
            halted: false,
            signing_key_id: "too-short".to_owned(),
            revoked_key_ids: vec!["also-bad".to_owned()],
        };
        let violations = bad.validate().expect_err("every field is invalid");
        assert_eq!(violations.len(), 5, "one per bad field: {violations:?}");
        assert!(violations.iter().any(|v| v.contains("target_version")));
        assert!(violations.iter().any(|v| v.contains("min_ring")));
        assert!(violations.iter().any(|v| v.contains("rollout_percent")));
        assert!(violations.iter().any(|v| v.contains("signing_key_id")));
        assert!(violations.iter().any(|v| v.contains("revoked_key_ids")));
    }

    #[test]
    fn a_fleet_update_at_the_boundaries_is_accepted() {
        let mut config = valid_fleet();
        config.rollout_percent = 100;
        config.revoked_key_ids = Vec::new();
        let rollout = config.validate().expect("100% and no revocations is valid");
        assert_eq!(rollout.update.fleet_rollout_percent, 100);
        assert!(rollout.revoked_keys.is_empty());
    }

    #[test]
    fn a_device_ota_assignment_parses_and_bounds_the_bucket() {
        let assignment = DeviceOtaConfig {
            ring: "fleet".to_owned(),
            canary_bucket: 37,
        }
        .validate()
        .expect("a valid assignment parses");
        assert_eq!(assignment.ring, Ring::Fleet);
        assert_eq!(assignment.canary_bucket, 37);

        let violations = DeviceOtaConfig {
            ring: "nope".to_owned(),
            canary_bucket: 100,
        }
        .validate()
        .expect_err("bad ring and out-of-range bucket");
        assert_eq!(violations.len(), 2, "{violations:?}");
    }
}
