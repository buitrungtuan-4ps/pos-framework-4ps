// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The server's unit tests: the sync key's order, the idle window's and backup interval's sources,
//! the system device id, and the exit codes.

use pos_fakes::FakeKeyVault;
use pos_fakes::executor::run_ready;
use pos_ports::key_vault::{KeyVault as _, Secret, SecretName};
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;

use super::{
    BackupPlan, ServeOutcome, ValueSource, choose_sync_key, idle_window_source, system_device_id,
    usable_env_key,
};

fn holding(secrets: &[(SecretName, &str)]) -> FakeKeyVault {
    let vault = FakeKeyVault::new();
    for (name, value) in secrets {
        run_ready(vault.store(*name, &Secret::new(value.as_bytes().to_vec())))
            .expect("the fake stores");
    }
    vault
}

/// The sign-in idle window is the store's when its `session` node sets one, the local file's
/// deprecated value when only that does — the one case the start-up warning names — and thirty
/// minutes when neither does (ADR-0160 decision 6).
#[test]
fn the_idle_window_is_the_nodes_then_the_files_then_the_default() {
    use crate::config::EdgeConfig;
    use pos_proto::session::PublishedSession;

    let store = "store_id = \"01J0000000000000000000000A\"";
    let plain = EdgeConfig::from_toml_str(store).expect("parses");
    let with_file =
        EdgeConfig::from_toml_str(&format!("{store}\nsign_in_idle_timeout_minutes = 45"))
            .expect("parses");
    let unset = PublishedSession::default();
    let published = PublishedSession {
        sign_in_idle_timeout_minutes: Some(15),
        ..PublishedSession::default()
    };
    // Outside the bounds, so the node sets nothing the edge can run.
    let unreadable = PublishedSession {
        sign_in_idle_timeout_minutes: Some(0),
        ..PublishedSession::default()
    };

    assert_eq!(
        idle_window_source(&with_file, &published),
        ValueSource::Published
    );
    assert_eq!(
        idle_window_source(&plain, &published),
        ValueSource::Published
    );
    assert_eq!(
        idle_window_source(&with_file, &unset),
        ValueSource::LocalFile
    );
    assert_eq!(
        idle_window_source(&with_file, &unreadable),
        ValueSource::LocalFile
    );
    assert_eq!(idle_window_source(&plain, &unset), ValueSource::Default);
}

/// A box archives every `backup_interval_hours` its file still sets, and every day when it sets
/// none, until the store's configuration sets an interval; `0` is the one way to switch
/// archiving off, and it starts no loop, so no published value can switch it back on
/// (ADR-0160 decision 6).
#[test]
fn the_file_sets_the_boxs_own_interval_and_zero_starts_no_loop() {
    use crate::config::EdgeConfig;
    use core::time::Duration;

    let store = "store_id = \"01J0000000000000000000000A\"";
    let plan = |extra: &str| {
        BackupPlan::from_config(
            &EdgeConfig::from_toml_str(&format!("{store}\n{extra}")).expect("parses"),
        )
    };

    let neither = plan("").expect("a box archives by default");
    assert_eq!(neither.interval, Duration::from_secs(24 * 3600));
    assert_eq!(neither.source, ValueSource::Default);

    let from_file = plan("backup_interval_hours = 6").expect("a number keeps archiving on");
    assert_eq!(from_file.interval, Duration::from_secs(6 * 3600));
    assert_eq!(from_file.source, ValueSource::LocalFile);

    assert!(
        plan("backup_interval_hours = 0").is_none(),
        "the file's zero is the one off-switch: no loop starts to read a published interval"
    );
}

/// A store key wins wherever it is set, and a box given nothing but its activation code syncs
/// with its device credential (ADR-0143).
#[test]
fn the_sync_key_falls_back_to_the_device_credential() {
    let both = holding(&[
        (SecretName::SyncKey, "pos_key_secret"),
        (SecretName::DeviceCredential, "posdev_id_secret"),
    ]);
    assert_eq!(
        run_ready(choose_sync_key(&both, Some("pos_env_secret".to_owned()))).as_deref(),
        Some("pos_key_secret")
    );
    let credential_only = holding(&[(SecretName::DeviceCredential, "posdev_id_secret")]);
    assert_eq!(
        run_ready(choose_sync_key(
            &credential_only,
            Some("pos_env_secret".to_owned())
        ))
        .as_deref(),
        Some("pos_env_secret")
    );
    assert_eq!(
        run_ready(choose_sync_key(&credential_only, None)).as_deref(),
        Some("posdev_id_secret")
    );
    assert_eq!(run_ready(choose_sync_key(&FakeKeyVault::new(), None)), None);
}

/// The hint an old console wrote after the `=` is not a key, and must not shadow the device
/// credential behind it.
#[test]
fn a_commented_environment_key_is_ignored() {
    assert_eq!(
        usable_env_key("  # issue a key in step 2, or paste one here"),
        None
    );
    assert_eq!(usable_env_key("   "), None);
    assert_eq!(
        usable_env_key(" pos_id_secret ").as_deref(),
        Some("pos_id_secret")
    );
    let credential_only = holding(&[(SecretName::DeviceCredential, "posdev_id_secret")]);
    assert_eq!(
        run_ready(choose_sync_key(
            &credential_only,
            Some("# issue a key in step 2".to_owned())
        ))
        .as_deref(),
        Some("posdev_id_secret")
    );
}

#[test]
fn the_system_device_id_is_stable_for_a_store() {
    // One box, one identity: every system event this store writes carries the same device id, so
    // the cloud can group them however many times the box restarts.
    let store_id = StoreId::new(Ulid::from_u128(7));
    assert_eq!(system_device_id(store_id), system_device_id(store_id));
}

#[test]
fn two_stores_never_share_a_system_device_id() {
    // The reason it is derived from the store rather than a fixed sentinel: two shops must not
    // collide in the cloud's own analytics.
    let one = system_device_id(StoreId::new(Ulid::from_u128(7)));
    let other = system_device_id(StoreId::new(Ulid::from_u128(8)));
    assert_ne!(one, other);
}

/// The codes a host agent reads (ADR-0113). They are a wire: changing one is changing what every
/// supervisor already deployed takes the exit to mean.
#[test]
fn each_way_of_stopping_has_its_own_exit_code() {
    assert_eq!(ServeOutcome::Stopped.exit_code(), 0);
    assert_eq!(ServeOutcome::RestartWanted.exit_code(), 10);
    assert_eq!(ServeOutcome::DrainIncomplete.exit_code(), 11);
}
