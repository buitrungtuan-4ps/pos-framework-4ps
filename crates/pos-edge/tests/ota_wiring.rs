// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The over-the-air path from a published config node to a swapped binary (roadmap v3 **R4** +
//! **R5-d**, [ADR-0055](../../../docs/adr/0055-edge-ota-updater.md) Amendment 1).
//!
//! `tests/ota.rs` proves the *orchestration* against a recording installer: the order of the steps,
//! and that verification gates the disk. This file proves the *wiring* — the part that did not exist
//! until R5, and the reason roadmap v3 indicts this program for merging written, tested, unreachable
//! code:
//!
//! * the rollout is read out of the **published `fleet_update` node**, through the same
//!   `session_from_config` the config-pull loop uses, rather than assembled by the test;
//! * the placement comes from the **published `device_ota` node**;
//! * the install runs against the **real [`SystemdInstaller`]** on a real directory, so the symlink
//!   swap, the database backup and the boot marker are what is asserted rather than a fake's call
//!   log;
//! * a restart is **asked for**, because an installed binary is not a running one until then.
//!
//! What is still not exercised here is the one thing a temporary directory cannot do: prove that
//! `systemd` restarts into the retargeted symlink and the store comes back trading. That is the
//! real-box step `docs/gate-register.md` tracks.
//!
//! Unix-only, because it asserts on a symlink and a permission bit. The Windows store installer is
//! roadmap **E4**; `installer.rs` keeps both operating-system primitives behind one pair of
//! functions so that slice is those two and a service manager, not a second copy of the swap logic.
#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pos_core::ota::{ReleaseVersion, SelfTest};
use pos_edge::config_client::session_from_config;
use pos_edge::{
    BootStanding, Edge, EdgeSession, InMemoryOtaState, InMemoryReceipts, OtaClient,
    OtaStateAuthority, RestartRequest, StoreIdentity, SystemdInstaller, TickOutcome,
    UpdateInstaller, UpdateOutcome, confirm_boot,
};
use pos_fakes::{FakeCloudSync, FakeSigner, FakeStore};
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tempfile::TempDir;

/// A restart requester that counts, standing in for the process exiting.
#[derive(Debug, Default)]
struct CountingRestart {
    asked: AtomicUsize,
}

impl CountingRestart {
    fn asked(&self) -> usize {
        self.asked.load(Ordering::Relaxed)
    }
}

impl RestartRequest for CountingRestart {
    fn request_restart(&self) {
        self.asked.fetch_add(1, Ordering::Relaxed);
    }
}

fn store() -> StoreId {
    StoreId::new(Ulid::from_u128(0x570E))
}

/// The store's cloud-published configuration: a rollout of the release the fake cloud serves, to the
/// whole fleet, signed by the key this "binary" trusts — and a placement for this box.
///
/// The `signing_key_id` is [`FakeSigner::key`]`(1)`'s, spelled as the node spells it: sixteen hex
/// digits. The `target_version` is **bare**, as the cloud publishes it and as the binary reports its
/// own (ADR-0088 Amendment 2) — that one string is what a release is called everywhere.
fn published_config(target: &str, rollout_percent: u8) -> serde_json::Value {
    serde_json::json!({
        "fleet_update": {
            "target_version": target,
            "min_ring": "lab",
            "rollout_percent": rollout_percent,
            "signing_key_id": "0101010101010101",
        },
        "device_ota": { "ring": "fleet", "canary_bucket": 0 },
    })
}

/// An edge whose live session is what `document` publishes — the config-pull loop's own apply step,
/// so nothing here reaches past the wire to set a field by hand.
fn edge_with(document: &serde_json::Value) -> Arc<Edge<FakeStore>> {
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store()),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    );
    edge.apply_session(session_from_config(&edge.session(), document));
    edge
}

/// A box laid out for over-the-air updates: `bin/current` on `slot-a`, holding a runnable program.
fn laid_out(root: &Path) -> SystemdInstaller {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let slot = bin.join("slot-a");
    std::fs::write(&slot, b"#!/bin/sh\nexit 0\n").expect("slot-a");
    std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o755)).expect("mode");
    std::os::unix::fs::symlink("slot-a", bin.join("current")).expect("current");
    let database = root.join("store.sqlite");
    std::fs::write(&database, b"the store as it was").expect("database");
    SystemdInstaller::new(bin, database)
}

/// The loop under test, over the faithful fake cloud.
fn client(
    document: &serde_json::Value,
    installer: SystemdInstaller,
    restart: Arc<CountingRestart>,
) -> OtaClient<
    FakeCloudSync,
    FakeSigner,
    SystemdInstaller,
    InMemoryOtaState,
    FakeStore,
    Arc<CountingRestart>,
> {
    OtaClient::new(
        FakeCloudSync::new(),
        FakeSigner::new(),
        installer,
        vec![FakeSigner::key(1)],
        InMemoryOtaState::new(),
        edge_with(document),
        store(),
        restart,
    )
}

#[tokio::test]
async fn a_promoted_release_becomes_the_binary_the_service_will_start() {
    // The load-bearing test of the whole slice: a rollout published as configuration ends with a
    // different binary behind `bin/current` and a restart requested. Every link in that chain was
    // present and unreachable before R5.
    let root = TempDir::new().expect("temp dir");
    let installer = laid_out(root.path());
    let restart = Arc::new(CountingRestart::default());
    let document = published_config(FakeCloudSync::KNOWN_RELEASE, 100);
    let ota = client(&document, installer.clone(), Arc::clone(&restart));

    let outcome = ota.tick().await;
    assert_eq!(
        outcome,
        TickOutcome::Decided(UpdateOutcome::Installed {
            version: ReleaseVersion::parse(FakeCloudSync::KNOWN_RELEASE).expect("a version"),
        }),
    );

    let bin = root.path().join("bin");
    assert_eq!(
        std::fs::read_link(bin.join("current")).expect("current"),
        Path::new("slot-b"),
        "the swap happened: the service's ExecStart target now names the other slot"
    );
    assert_eq!(
        std::fs::read(bin.join("slot-b")).expect("the new binary"),
        FakeCloudSync::artifact_bytes(),
        "and it is the artifact the cloud served, byte for byte — the bytes the signature covered"
    );
    assert!(
        root.path().join("store.sqlite.pre-update").exists(),
        "the database was copied before anything was written (roadmap P9)"
    );
    assert_eq!(
        std::fs::read_to_string(bin.join("unconfirmed")).expect("marker"),
        "0",
        "the new version is on trial until it comes up"
    );
    assert_eq!(
        restart.asked(),
        1,
        "an installed binary is not a running one until the process restarts into it"
    );
}

#[tokio::test]
async fn a_store_with_nothing_published_touches_nothing() {
    // The state of every store until an operator promotes a release, so it has to be silent: no
    // fetch, no backup, no swap, no restart.
    let root = TempDir::new().expect("temp dir");
    let installer = laid_out(root.path());
    let restart = Arc::new(CountingRestart::default());
    let ota = client(
        &serde_json::json!({ "other": true }),
        installer,
        Arc::clone(&restart),
    );

    assert_eq!(ota.tick().await, TickOutcome::NoRollout);
    assert_eq!(
        std::fs::read_link(root.path().join("bin").join("current")).expect("current"),
        Path::new("slot-a"),
    );
    assert!(!root.path().join("store.sqlite.pre-update").exists());
    assert_eq!(restart.asked(), 0);
}

#[tokio::test]
async fn a_placed_store_the_ramp_has_not_reached_waits_without_touching_the_disk() {
    // A rollout at 0 % is published and this box is in the fleet ring: eligible by ring, not yet by
    // ramp. The distinction matters because the alternative — installing anyway — is how a canary
    // stops being a canary.
    let root = TempDir::new().expect("temp dir");
    let installer = laid_out(root.path());
    let restart = Arc::new(CountingRestart::default());
    let document = published_config(FakeCloudSync::KNOWN_RELEASE, 0);
    let ota = client(&document, installer, Arc::clone(&restart));

    assert!(
        matches!(
            ota.tick().await,
            TickOutcome::Decided(UpdateOutcome::Skipped(_))
        ),
        "not this box's turn yet"
    );
    assert!(!root.path().join("store.sqlite.pre-update").exists());
    assert_eq!(restart.asked(), 0);
}

#[tokio::test]
async fn a_halted_rollout_is_not_installed_even_though_the_box_is_eligible() {
    // The console's kill switch, from the published node through to the disk.
    let root = TempDir::new().expect("temp dir");
    let installer = laid_out(root.path());
    let restart = Arc::new(CountingRestart::default());
    let mut document = published_config(FakeCloudSync::KNOWN_RELEASE, 100);
    document["fleet_update"]["halted"] = serde_json::json!(true);
    let ota = client(&document, installer, Arc::clone(&restart));

    assert_eq!(
        ota.tick().await,
        TickOutcome::Decided(UpdateOutcome::Halted),
    );
    assert!(!root.path().join("store.sqlite.pre-update").exists());
    assert_eq!(restart.asked(), 0);
}

#[tokio::test]
async fn a_revoked_signing_key_is_refused_before_the_artifact_is_fetched() {
    // Revocation travels through configuration precisely so a compromised key can stop being
    // accepted without shipping a release that key would sign.
    let root = TempDir::new().expect("temp dir");
    let installer = laid_out(root.path());
    let restart = Arc::new(CountingRestart::default());
    let mut document = published_config(FakeCloudSync::KNOWN_RELEASE, 100);
    document["fleet_update"]["revoked_key_ids"] = serde_json::json!(["0101010101010101"]);
    let ota = client(&document, installer, Arc::clone(&restart));

    assert_eq!(
        ota.tick().await,
        TickOutcome::Decided(UpdateOutcome::Refused),
    );
    assert!(!root.path().join("store.sqlite.pre-update").exists());
    assert_eq!(restart.asked(), 0);
}

#[tokio::test]
async fn a_version_that_failed_its_own_boot_reverts_on_the_next_tick() {
    // The rule ADR-0048 puts above every other, end to end and through the durable authority: this
    // box is running a version whose self-test failed, so it goes back — whatever the rollout says.
    let root = TempDir::new().expect("temp dir");
    let installer = laid_out(root.path());
    // A previous install: `previous` names slot-a and `current` names slot-b.
    installer.stage_backup().expect("backup");
    installer.apply(b"#!/bin/sh\nexit 0\n").expect("apply");
    installer.commit().expect("commit");
    std::fs::write(root.path().join("store.sqlite"), b"migrated").expect("migrate");

    let restart = Arc::new(CountingRestart::default());
    let document = published_config(FakeCloudSync::KNOWN_RELEASE, 100);
    let ota = client(&document, installer, Arc::clone(&restart));
    // The verdict is recorded against the version this process *is*, which is what makes the rule
    // fire — and why the record has to survive the restart an install performs.
    let running = pos_edge::released().expect("this binary's version parses");
    ota.authority()
        .record_self_test(
            store(),
            SelfTest {
                version: running,
                passed: false,
            },
        )
        .await
        .expect("record");

    assert_eq!(
        ota.tick().await,
        TickOutcome::Decided(UpdateOutcome::RolledBack),
    );
    assert_eq!(
        std::fs::read_link(root.path().join("bin").join("current")).expect("current"),
        Path::new("slot-a"),
        "back on the binary that worked"
    );
    assert_eq!(
        std::fs::read(root.path().join("store.sqlite")).expect("database"),
        b"the store as it was",
        "and on the schema that binary understands"
    );
    assert_eq!(restart.asked(), 1);
}

#[tokio::test]
async fn confirming_a_boot_records_the_pass_the_rollback_rule_reads() {
    // The half a pre-commit self-test cannot do: settle the verdict for the version now *running*,
    // so the next tick does not weigh a box against a stale one.
    let root = TempDir::new().expect("temp dir");
    let installer = laid_out(root.path());
    let authority = InMemoryOtaState::new();
    let cloud = FakeCloudSync::new();

    installer.apply(b"#!/bin/sh\nexit 0\n").expect("apply");
    installer.commit().expect("commit");
    let standing = installer.begin_boot().expect("boot");
    assert_eq!(standing, BootStanding::Unconfirmed { attempt: 1 });

    confirm_boot(&cloud, &authority, &installer, store(), standing).await;

    assert!(
        !root.path().join("bin").join("unconfirmed").exists(),
        "the marker is cleared, so ordinary restarts never accumulate towards a revert"
    );
    let recorded = authority
        .last_self_test(store())
        .await
        .expect("read")
        .expect("a verdict is on record");
    assert_eq!(recorded.version, pos_edge::released().expect("parses"));
    assert!(recorded.passed);
}

#[tokio::test]
async fn an_ordinary_restart_does_not_overwrite_a_recorded_failure() {
    // A store that failed a release keeps that fact until the next install replaces it. Recording a
    // pass earned by no install would erase the one thing the console knows about that store.
    let authority = InMemoryOtaState::new();
    let failed = SelfTest {
        version: ReleaseVersion::new(9, 9, 9),
        passed: false,
    };
    authority
        .record_self_test(store(), failed)
        .await
        .expect("record");

    let root = TempDir::new().expect("temp dir");
    let installer = laid_out(root.path());
    confirm_boot(
        &FakeCloudSync::new(),
        &authority,
        &installer,
        store(),
        BootStanding::Settled,
    )
    .await;

    assert_eq!(
        authority.last_self_test(store()).await.expect("read"),
        Some(failed),
        "a settled boot reports, and records nothing"
    );
}
