// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge OTA updater, driven against the fakes and a recording installer (P9, ADR-0055).
//!
//! Every branch of the orchestration is a test here — read-only, roll back, halt, refuse, skip,
//! verify-fails-so-nothing-is-written, self-test-fails-so-rollback, install-succeeds. The one part
//! not exercised is the real `UpdateInstaller` (writing a binary, rebooting), which is the gated
//! hardware/OS step; the fake installer records the call sequence so the *order* — verify before the
//! disk is touched, self-test before commit — is what these assert.

use std::sync::{Arc, Mutex};

use pos_core::lease::LeaseStanding;
use pos_core::ota::{DeviceState, PublishedUpdate, ReleaseVersion, Ring, SelfTest, SkipReason};
use pos_edge::{InstallError, OtaUpdater, UpdateError, UpdateInstaller, UpdateOutcome, UpdatePlan};
use pos_fakes::{FakeCloudSync, FakeSigner};
use pos_ports::signer::PublicKey;
use pos_proto::text::ReleaseTag;

/// An installer that records the sequence of real-machine steps and reports a chosen self-test
/// result. Its log lives behind a shared handle, so a test keeps a clone to inspect after the
/// updater has taken ownership of the installer.
#[derive(Clone)]
struct RecordingInstaller {
    calls: Arc<Mutex<Vec<&'static str>>>,
    self_test_passes: bool,
}

impl RecordingInstaller {
    fn new(self_test_passes: bool) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            self_test_passes,
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("lock").clone()
    }

    fn record(&self, step: &'static str) {
        self.calls.lock().expect("lock").push(step);
    }
}

impl UpdateInstaller for RecordingInstaller {
    fn stage_backup(&self) -> Result<(), InstallError> {
        self.record("stage_backup");
        Ok(())
    }

    fn apply(&self, _artifact: &[u8]) -> Result<(), InstallError> {
        self.record("apply");
        Ok(())
    }

    fn self_test(&self) -> Result<bool, InstallError> {
        self.record("self_test");
        Ok(self.self_test_passes)
    }

    fn commit(&self) -> Result<(), InstallError> {
        self.record("commit");
        Ok(())
    }

    fn rollback(&self) -> Result<(), InstallError> {
        self.record("rollback");
        Ok(())
    }
}

/// The one key baked into this "binary" for the tests.
fn trusted() -> Vec<PublicKey> {
    vec![FakeSigner::key(1)]
}

/// An updater over a faithful cloud — one whose artifact verifies against [`trusted`].
fn updater(
    installer: RecordingInstaller,
) -> OtaUpdater<FakeCloudSync, FakeSigner, RecordingInstaller> {
    updater_over(FakeCloudSync::new(), installer)
}

/// An updater over a chosen cloud, for the cases where the *cloud* is what is wrong.
///
/// The signature now arrives with the artifact ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)),
/// so "a bad signature" is no longer something a caller can hand the updater beside good bytes — it
/// is a property of the cloud being talked to. That is a better model of the threat: the attacker
/// is the host serving the artifact, not the code assembling the plan.
fn updater_over(
    cloud: FakeCloudSync,
    installer: RecordingInstaller,
) -> OtaUpdater<FakeCloudSync, FakeSigner, RecordingInstaller> {
    OtaUpdater::new(cloud, FakeSigner::new(), installer, trusted())
}

fn device(ring: Ring, last_self_test: Option<SelfTest>) -> DeviceState {
    DeviceState {
        current: ReleaseVersion::new(1, 0, 0),
        ring,
        canary_bucket: 0,
        last_self_test,
    }
}

/// A published update targeting a newer version, signed by `signing_key_id`.
fn published(halted: bool, signing_key_id: [u8; 8]) -> PublishedUpdate {
    PublishedUpdate {
        target: ReleaseVersion::new(1, 2, 3),
        min_ring: Ring::Lab,
        fleet_rollout_percent: 100,
        signing_key_id,
        halted,
    }
}

fn release() -> ReleaseTag {
    ReleaseTag::new(FakeCloudSync::KNOWN_RELEASE)
}

#[tokio::test]
async fn a_verified_eligible_update_installs_and_commits() {
    let installer = RecordingInstaller::new(true);
    let probe = installer.clone();
    let updater = updater(installer);
    let published = published(false, [1; 8]);
    let release = release();
    let plan = UpdatePlan {
        published: &published,
        release: &release,
        revoked_keys: &[],
    };

    let outcome = updater
        .run(&device(Ring::Lab, None), &plan, LeaseStanding::Active)
        .await
        .expect("run");

    assert_eq!(
        outcome,
        UpdateOutcome::Installed {
            version: ReleaseVersion::new(1, 2, 3)
        }
    );
    assert_eq!(
        probe.calls(),
        vec!["stage_backup", "apply", "self_test", "commit"]
    );
}

#[tokio::test]
async fn a_failed_self_test_rolls_back_and_never_commits() {
    let installer = RecordingInstaller::new(false);
    let probe = installer.clone();
    let updater = updater(installer);
    let published = published(false, [1; 8]);
    let release = release();
    let plan = UpdatePlan {
        published: &published,
        release: &release,
        revoked_keys: &[],
    };

    let outcome = updater
        .run(&device(Ring::Lab, None), &plan, LeaseStanding::Active)
        .await
        .expect("run");

    assert_eq!(outcome, UpdateOutcome::RolledBack);
    assert_eq!(
        probe.calls(),
        vec!["stage_backup", "apply", "self_test", "rollback"],
        "a failed self-test reverts rather than committing"
    );
}

#[tokio::test]
async fn an_untrusted_key_is_refused_before_the_disk_is_touched() {
    let installer = RecordingInstaller::new(true);
    let probe = installer.clone();
    // A cloud serving an artifact signed by a key this binary does not have baked in. The published
    // id matches that key, so the decision is Install and the refusal happens at verification.
    let hostile = FakeCloudSync::serving_signature(FakeSigner::sign(
        &FakeCloudSync::artifact_bytes(),
        &FakeSigner::key(2),
    ));
    let updater = updater_over(hostile, installer);
    let published = published(false, [2; 8]);
    let release = release();
    let plan = UpdatePlan {
        published: &published,
        release: &release,
        revoked_keys: &[],
    };

    let error = updater
        .run(&device(Ring::Lab, None), &plan, LeaseStanding::Active)
        .await
        .expect_err("an untrusted key is refused");

    assert!(matches!(error, UpdateError::UntrustedKey));
    assert!(
        probe.calls().is_empty(),
        "nothing was staged or applied before verification failed"
    );
}

#[tokio::test]
async fn a_bad_signature_is_refused_before_the_disk_is_touched() {
    let installer = RecordingInstaller::new(true);
    let probe = installer.clone();
    // A cloud serving the trusted key's signature over *different bytes* than the artifact it
    // serves — a swapped blob, which is the tampering ADR-0088 says the cloud must not be able to
    // get away with.
    let hostile = FakeCloudSync::serving_signature(FakeSigner::sign(
        b"not the real artifact",
        &FakeSigner::key(1),
    ));
    let updater = updater_over(hostile, installer);
    let published = published(false, [1; 8]);
    let release = release();
    let plan = UpdatePlan {
        published: &published,
        release: &release,
        revoked_keys: &[],
    };

    let error = updater
        .run(&device(Ring::Lab, None), &plan, LeaseStanding::Active)
        .await
        .expect_err("a bad signature is refused");

    assert!(matches!(error, UpdateError::Verify(_)));
    assert!(
        probe.calls().is_empty(),
        "a spoofed artifact writes nothing"
    );
}

#[tokio::test]
async fn a_revoked_signing_key_is_refused() {
    let installer = RecordingInstaller::new(true);
    let probe = installer.clone();
    let updater = updater(installer);
    let published = published(false, [1; 8]);
    let release = release();
    let plan = UpdatePlan {
        published: &published,
        release: &release,
        revoked_keys: &[[1; 8]],
    };

    let outcome = updater
        .run(&device(Ring::Lab, None), &plan, LeaseStanding::Active)
        .await
        .expect("run");

    assert_eq!(outcome, UpdateOutcome::Refused);
    assert!(probe.calls().is_empty(), "a revoked key installs nothing");
}

#[tokio::test]
async fn the_kill_switch_halts() {
    let installer = RecordingInstaller::new(true);
    let probe = installer.clone();
    let updater = updater(installer);
    let published = published(true, [1; 8]);
    let release = release();
    let plan = UpdatePlan {
        published: &published,
        release: &release,
        revoked_keys: &[],
    };

    let outcome = updater
        .run(&device(Ring::Lab, None), &plan, LeaseStanding::Active)
        .await
        .expect("run");

    assert_eq!(outcome, UpdateOutcome::Halted);
    assert!(probe.calls().is_empty());
}

#[tokio::test]
async fn an_already_current_version_is_skipped() {
    let installer = RecordingInstaller::new(true);
    let probe = installer.clone();
    let updater = updater(installer);
    let published = published(false, [1; 8]);
    let release = release();
    let plan = UpdatePlan {
        published: &published,
        release: &release,
        revoked_keys: &[],
    };
    // The device already runs the target version.
    let current = DeviceState {
        current: ReleaseVersion::new(1, 2, 3),
        ring: Ring::Lab,
        canary_bucket: 0,
        last_self_test: None,
    };

    let outcome = updater
        .run(&current, &plan, LeaseStanding::Active)
        .await
        .expect("run");

    assert_eq!(outcome, UpdateOutcome::Skipped(SkipReason::AlreadyCurrent));
    assert!(probe.calls().is_empty());
}

#[tokio::test]
async fn a_device_that_failed_its_self_test_rolls_back() {
    let installer = RecordingInstaller::new(true);
    let probe = installer.clone();
    let updater = updater(installer);
    let published = published(false, [1; 8]);
    let release = release();
    let plan = UpdatePlan {
        published: &published,
        release: &release,
        revoked_keys: &[],
    };
    // The box failed the self-test on the version it is running: it must revert, whatever the update.
    let broken = device(
        Ring::Lab,
        Some(SelfTest {
            version: ReleaseVersion::new(1, 0, 0),
            passed: false,
        }),
    );

    let outcome = updater
        .run(&broken, &plan, LeaseStanding::Active)
        .await
        .expect("run");

    assert_eq!(outcome, UpdateOutcome::RolledBack);
    assert_eq!(probe.calls(), vec!["rollback"]);
}

#[tokio::test]
async fn a_superseded_box_is_read_only_and_updates_nothing() {
    let installer = RecordingInstaller::new(true);
    let probe = installer.clone();
    let updater = updater(installer);
    let published = published(false, [1; 8]);
    let release = release();
    let plan = UpdatePlan {
        published: &published,
        release: &release,
        revoked_keys: &[],
    };

    let outcome = updater
        .run(&device(Ring::Lab, None), &plan, LeaseStanding::Superseded)
        .await
        .expect("run");

    assert_eq!(outcome, UpdateOutcome::ReadOnly);
    assert!(probe.calls().is_empty(), "a demoted box does not update");
}
