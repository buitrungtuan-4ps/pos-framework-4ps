// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The four-level config-tree store (ADR-0033).

use super::{Client, block_on, prepared};
use pos_proto::{StoreId, TenantId, Ulid};
use store_postgres::RowUpdate;

fn parsed(json: &str) -> serde_json::Value {
    serde_json::from_str(json).expect("valid json")
}

/// A tree state saves, loads back at the version it was written at, and replaces in place under
/// that version ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
#[test]
fn saves_loads_and_replaces_under_the_version_it_was_read_at() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let tenant = TenantId::new(Ulid::from_u128(0x00C0_FFEE));
        let store_id = StoreId::new(Ulid::from_u128(0x5709));

        assert!(
            trees
                .load_state(tenant, store_id)
                .await
                .expect("load")
                .is_none(),
            "no row before the first save"
        );

        // A representative ConfigTreeState document (the adapter treats it as opaque jsonb).
        let first = r#"{"k":20,"layers":[{"currency_code":"VND"},{},{},{}],"history":[]}"#;
        let created = match trees
            .save_state(tenant, store_id, first, None)
            .await
            .expect("the create")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the create to apply, got {other:?}")
            }
        };
        let (loaded, version) = trees
            .load_state(tenant, store_id)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(
            parsed(&loaded),
            parsed(first),
            "the stored document round-trips (compared as JSON, since jsonb reorders keys)"
        );
        assert_eq!(
            version, created,
            "the version a save returns is the one the next load reads"
        );

        // A second save at the current version replaces the row rather than duplicating it.
        let second = r#"{"k":20,"layers":[{"currency_code":"JPY"},{},{},{}],"history":[]}"#;
        let replaced = match trees
            .save_state(tenant, store_id, second, Some(&version))
            .await
            .expect("the replace")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the replace to apply, got {other:?}")
            }
        };
        assert_ne!(
            replaced, version,
            "xmin must move on every UPDATE, or the next write would be unguarded"
        );
        let (reloaded, _) = trees
            .load_state(tenant, store_id)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(
            parsed(&reloaded),
            parsed(second),
            "the conditional save replaced the state"
        );

        // Another tenant with the same store id sees nothing — the (tenant, store) key isolates.
        let other = TenantId::new(Ulid::from_u128(0xBEEF));
        assert!(
            trees
                .load_state(other, store_id)
                .await
                .expect("load")
                .is_none(),
            "the load is scoped to the tenant"
        );
    });
}

/// The four ways a config-tree save is refused, and the proof that none of them wrote anything.
///
/// The **create** case is the one this table has and the record-shaped writes do not: a config
/// tree's first publish has no prior version to name, so `expected = None` means "there is
/// nothing here yet". If another publish got there first that claim is false, and the save must
/// be refused rather than upserted away — which is exactly what an `ON CONFLICT DO UPDATE`
/// would have done silently.
#[test]
fn a_stale_garbled_or_already_taken_save_is_refused_and_writes_nothing() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let tenant = TenantId::new(Ulid::from_u128(0x00C0_FFEF));
        let store_id = StoreId::new(Ulid::from_u128(0x570C));

        let first = r#"{"k":20,"layers":[{"currency_code":"VND"},{},{},{}],"history":[]}"#;
        let second = r#"{"k":20,"layers":[{"currency_code":"JPY"},{},{},{}],"history":[]}"#;
        trees
            .save_state(tenant, store_id, first, None)
            .await
            .expect("the create");
        let (_, stale) = trees
            .load_state(tenant, store_id)
            .await
            .expect("load")
            .expect("present");
        trees
            .save_state(tenant, store_id, second, Some(&stale))
            .await
            .expect("the replace");

        // Replaying the version read before that replace is the lost update, refused.
        assert_eq!(
            trees
                .save_state(tenant, store_id, first, Some(&stale))
                .await
                .expect("the comparison must not raise"),
            RowUpdate::VersionMismatch
        );

        // A garbled tag is a mismatch too, not a database error — the comparison is on
        // `xmin::text`, because casting caller text to `xid` would raise instead of refusing.
        assert_eq!(
            trees
                .save_state(tenant, store_id, first, Some("not-a-transaction-id"))
                .await
                .expect("the comparison must not raise"),
            RowUpdate::VersionMismatch
        );

        // "Nothing here yet" about a store that has since been published to.
        assert_eq!(
            trees
                .save_state(tenant, store_id, first, None)
                .await
                .expect("the create path"),
            RowUpdate::VersionMismatch
        );

        // A version-gated save against a store with no row at all is a NotFound, not a mismatch:
        // the probe on the failure path is what keeps the two distinguishable.
        let absent = StoreId::new(Ulid::from_u128(0x570D));
        assert_eq!(
            trees
                .save_state(tenant, absent, first, Some(&stale))
                .await
                .expect("the probe"),
            RowUpdate::NotFound
        );

        let (after_refusals, _) = trees
            .load_state(tenant, store_id)
            .await
            .expect("load")
            .expect("present");
        assert_eq!(
            parsed(&after_refusals),
            parsed(second),
            "no refused save changed the stored document"
        );
    });
}

/// A config pull records the store's liveness (contact instant, held version, pull instant), and a
/// second pull upserts that row in place rather than duplicating ([ADR-0068]).
#[test]
fn records_store_liveness_on_a_pull() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let tenant = TenantId::new(Ulid::from_u128(0x0FEE));
        let store_id = StoreId::new(Ulid::from_u128(0x57));
        let held = "0000000000CONFIGVERSION0AA";

        // First pull: holds a version, seen at t=1000ms.
        trees
            .record_seen(tenant, store_id, Some(held), 1000)
            .await
            .expect("record seen");
        let row = admin
            .query_one(
                "SELECT last_seen_at, config_version_held, last_config_pull_at \
                 FROM store_liveness WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .expect("the pull recorded a liveness row");
        assert_eq!(
            row.get::<_, i64>(0),
            1000,
            "last_seen_at is the contact instant"
        );
        assert_eq!(
            row.get::<_, Option<String>>(1).as_deref(),
            Some(held),
            "the held version is recorded verbatim"
        );
        assert_eq!(
            row.get::<_, Option<i64>>(2),
            Some(1000),
            "a config pull stamps last_config_pull_at too"
        );

        // Second pull at t=2000ms holding nothing: upserts in place — one row, advanced instant,
        // held version cleared.
        trees
            .record_seen(tenant, store_id, None, 2000)
            .await
            .expect("record seen again");
        let count: i64 = admin
            .query_one(
                "SELECT count(*) FROM store_liveness WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .expect("count")
            .get(0);
        assert_eq!(count, 1, "the second pull upserts rather than duplicating");
        let row = admin
            .query_one(
                "SELECT last_seen_at, config_version_held FROM store_liveness \
                 WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .expect("row");
        assert_eq!(row.get::<_, i64>(0), 2000, "the instant advanced");
        assert_eq!(
            row.get::<_, Option<String>>(1),
            None,
            "holding nothing clears the recorded held version"
        );
    });
}

/// An OTA report (ADR-0078) records the running version and self-test outcome onto the liveness
/// row, and — being a contact — advances `last_seen_at`, even for a store that never pulled config.
#[test]
fn records_an_ota_report() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let tenant = TenantId::new(Ulid::from_u128(0x0FEE));
        let store_id = StoreId::new(Ulid::from_u128(0x0A7A));

        // A store reports before it has ever pulled config: the row is created, last_seen_at set.
        trees
            .record_ota_report(tenant, store_id, "v1.2.3", Some(true), 5000)
            .await
            .expect("record report");
        let row = admin
            .query_one(
                "SELECT last_seen_at, installed_version, self_test_ok, reported_at \
                 FROM store_liveness WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .expect("the report created a liveness row");
        assert_eq!(row.get::<_, i64>(0), 5000, "a report is contact");
        assert_eq!(row.get::<_, Option<String>>(1).as_deref(), Some("v1.2.3"));
        assert_eq!(row.get::<_, Option<bool>>(2), Some(true));
        assert_eq!(row.get::<_, Option<i64>>(3), Some(5000));

        // A later failed-self-test report upserts in place — one row, advanced instant.
        trees
            .record_ota_report(tenant, store_id, "v1.3.0", Some(false), 9000)
            .await
            .expect("record report again");
        let row = admin
            .query_one(
                "SELECT last_seen_at, installed_version, self_test_ok FROM store_liveness \
                 WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .expect("row");
        assert_eq!(row.get::<_, i64>(0), 9000, "the instant advanced");
        assert_eq!(row.get::<_, Option<String>>(1).as_deref(), Some("v1.3.0"));
        assert_eq!(
            row.get::<_, Option<bool>>(2),
            Some(false),
            "a failed self-test is recorded, not dropped"
        );

        // A store with no self-test at all writes SQL NULL (ADR-0078 Amendment 1). The column was
        // always nullable and `FleetStore::self_test_ok` was always `Option<bool>`; before the
        // amendment nothing could put a reported row in that state, so the console's "Not
        // reported" was unreachable for a store that *had* reported. This is that state.
        trees
            .record_ota_report(tenant, store_id, "v1.4.0", None, 12_000)
            .await
            .expect("record a report with no self-test");
        let row = admin
            .query_one(
                "SELECT installed_version, self_test_ok FROM store_liveness \
                 WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .expect("row");
        assert_eq!(row.get::<_, Option<String>>(0).as_deref(), Some("v1.4.0"));
        assert_eq!(
            row.get::<_, Option<bool>>(1),
            None,
            "no self-test is NULL, not a fabricated pass or failure"
        );
    });
}

/// A heartbeat advances only `last_seen_at` (and the reported publish backlog), preserving the
/// held version and last-config-pull instant a prior pull recorded; a heartbeat before any pull
/// creates the row with those NULL ([ADR-0068] slice 2).
#[test]
fn heartbeat_advances_last_seen_only() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let tenant = TenantId::new(Ulid::from_u128(0x0FEE2));
        let store_id = StoreId::new(Ulid::from_u128(0x58));

        // A config pull first, then a later heartbeat: the heartbeat bumps last_seen and leaves the
        // held version and the config-pull instant as the pull recorded them.
        trees
            .record_seen(tenant, store_id, Some("0000000000HELDVERSION00AAA"), 1000)
            .await
            .expect("record seen");
        trees
            .record_heartbeat(tenant, store_id, 5000, None, None, None)
            .await
            .expect("record heartbeat");
        let row = admin
            .query_one(
                "SELECT last_seen_at, config_version_held, last_config_pull_at \
                 FROM store_liveness WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &store_id.to_string()],
            )
            .await
            .expect("row");
        assert_eq!(
            row.get::<_, i64>(0),
            5000,
            "the heartbeat advanced last_seen"
        );
        assert_eq!(
            row.get::<_, Option<String>>(1).as_deref(),
            Some("0000000000HELDVERSION00AAA"),
            "the held version a prior pull recorded is preserved"
        );
        assert_eq!(
            row.get::<_, Option<i64>>(2),
            Some(1000),
            "the last-config-pull instant is preserved"
        );

        // A heartbeat for a store that has never pulled creates the row with those two NULL.
        let fresh = StoreId::new(Ulid::from_u128(0x59));
        trees
            .record_heartbeat(tenant, fresh, 2000, None, None, None)
            .await
            .expect("record heartbeat for a fresh store");
        let row = admin
            .query_one(
                "SELECT last_seen_at, config_version_held, last_config_pull_at \
                 FROM store_liveness WHERE tenant_id = $1 AND store_id = $2",
                &[&tenant.to_string(), &fresh.to_string()],
            )
            .await
            .expect("row");
        assert_eq!(row.get::<_, i64>(0), 2000);
        assert_eq!(
            row.get::<_, Option<String>>(1),
            None,
            "a heartbeat-only store holds no recorded version"
        );
        assert_eq!(
            row.get::<_, Option<i64>>(2),
            None,
            "a heartbeat-only store has no config-pull instant"
        );
    });
}

/// A reported publish backlog is recorded with the instant that reported it, and a later
/// heartbeat that reports none leaves both alone — "did not say" is not "caught up"
/// ([ADR-0068] slice 2, Track O6).
#[test]
fn a_reported_outbox_depth_is_recorded_and_never_erased_by_silence() {
    block_on(async {
        async fn read(admin: &Client, tenant: TenantId, store: StoreId) -> tokio_postgres::Row {
            admin
                .query_one(
                    "SELECT outbox_depth, outbox_reported_at, lease_generation \
                     FROM store_liveness \
                     WHERE tenant_id = $1 AND store_id = $2",
                    &[&tenant.to_string(), &store.to_string()],
                )
                .await
                .expect("row")
        }
        let (store, admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let tenant = TenantId::new(Ulid::from_u128(0x0FEE3));
        let store_id = StoreId::new(Ulid::from_u128(0x5A));

        trees
            .record_heartbeat(tenant, store_id, 1000, Some(17), Some(4), None)
            .await
            .expect("record a heartbeat that reports a backlog");
        let row = read(&admin, tenant, store_id).await;
        assert_eq!(
            row.get::<_, Option<i64>>(0),
            Some(17),
            "the depth is recorded"
        );
        assert_eq!(
            row.get::<_, Option<i64>>(1),
            Some(1000),
            "the depth is stamped with the heartbeat that carried it"
        );
        assert_eq!(
            row.get::<_, Option<i64>>(2),
            Some(4),
            "and the lease generation the box says it holds (ADR-0108)"
        );

        // An older edge, or one whose log could not be read, reports nothing. Overwriting a real
        // backlog with a fabricated zero would read as a store that had caught up.
        trees
            .record_heartbeat(tenant, store_id, 9000, None, None, None)
            .await
            .expect("record a heartbeat that reports nothing");
        let row = read(&admin, tenant, store_id).await;
        assert_eq!(
            row.get::<_, Option<i64>>(0),
            Some(17),
            "silence leaves the last reported depth standing"
        );
        assert_eq!(
            row.get::<_, Option<i64>>(1),
            Some(1000),
            "and leaves the instant that reported it, so the console can see it is stale"
        );
        assert_eq!(
            row.get::<_, Option<i64>>(2),
            Some(4),
            "the same rule guards the lease, and more sharply: generation 0 is a real first \
             lease, so a fabricated zero would report a replaced box as current"
        );

        // A store that has only ever been silent has no depth at all — NULL, not zero.
        let silent = StoreId::new(Ulid::from_u128(0x5B));
        trees
            .record_heartbeat(tenant, silent, 1000, None, None, None)
            .await
            .expect("record heartbeat for a silent store");
        let row = read(&admin, tenant, silent).await;
        assert_eq!(
            row.get::<_, Option<i64>>(0),
            None,
            "a store that never reported has no depth, which is not a depth of zero"
        );
        assert_eq!(row.get::<_, Option<i64>>(1), None);
        assert_eq!(
            row.get::<_, Option<i64>>(2),
            None,
            "nor a lease generation — 'did not say' is not 'holds generation 0'"
        );
    });
}

/// The print-agent standings follow the same `COALESCE`, and the case that separates them from
/// their neighbours is the one those columns cannot express: an **empty** report
/// ([ADR-0112](../../../../docs/adr/0112-print-agents.md)).
///
/// `NULL` is a box that did not look, and leaves the stored list standing. `'[]'` is a box that
/// looked and has no bound agent — what a manager releasing the last terminal produces — and it
/// must *replace* the list, because a console still showing an agent nobody is bound to is the
/// stale answer the column exists to prevent. Only a real database tells the two apart, because
/// The region the bump wrote comes back out of the fleet read
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Two tables and one join, which is exactly what neither the unit tests nor the in-process
/// suite can prove: they hand the view a `FleetRow` that already holds a region. This asserts
/// the `LEFT JOIN store_lease` actually carries the two columns across, and that a store with
/// no lease row at all — most of the fleet — comes back as no region rather than as an error.
#[test]
fn the_fleet_read_carries_the_region_the_bump_wrote() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let fleet = store.fleet();
        let tenant = TenantId::new(Ulid::from_u128(0x0EE0));
        let hosted = StoreId::new(Ulid::from_u128(0x0EE1));
        let unplaced = StoreId::new(Ulid::from_u128(0x0EE2));

        for (id, name) in [(hosted, "Hosted"), (unplaced, "Never bumped")] {
            store
                .registry()
                .insert_store(&id.to_string(), &tenant.to_string(), None, name)
                .await
                .expect("register the store");
        }

        super::issue_with_region(
            &trees,
            tenant,
            hosted,
            1_777_000_000_000,
            Some("EDGE_PLACEMENT_HOSTED_BY_OPERATOR"),
            store_postgres::StoredRegionWrite::Set {
                country: "SG",
                label: "ap-southeast-1",
            },
            None,
            None,
        )
        .await;

        let rows = fleet
            .list(&tenant.to_string())
            .await
            .expect("read the fleet");
        let row = |wanted: StoreId| {
            rows.iter()
                .find(|row| row.store_id == wanted.to_string())
                .expect("the store is in the fleet listing")
        };

        assert_eq!(row(hosted).region_country.as_deref(), Some("SG"));
        assert_eq!(row(hosted).region_label.as_deref(), Some("ap-southeast-1"));
        assert!(
            row(unplaced).region_country.is_none() && row(unplaced).region_label.is_none(),
            "a store with no lease row has no region, and the LEFT JOIN must say so rather \
             than dropping the row"
        );
    });
}

/// only here is the `COALESCE` actually evaluated.
#[test]
fn an_empty_print_agent_report_replaces_the_list_and_an_absent_one_leaves_it() {
    block_on(async {
        async fn agents(
            admin: &Client,
            tenant: TenantId,
            store: StoreId,
        ) -> (Option<String>, Option<i64>) {
            let row = admin
                .query_one(
                    "SELECT print_agents::text, print_agents_reported_at \
                     FROM store_liveness \
                     WHERE tenant_id = $1 AND store_id = $2",
                    &[&tenant.to_string(), &store.to_string()],
                )
                .await
                .expect("row");
            (row.get(0), row.get(1))
        }
        let (store, admin) = prepared().await.expect("prepare the database");
        let trees = store.config_trees();
        let tenant = TenantId::new(Ulid::from_u128(0x0FEE4));
        let store_id = StoreId::new(Ulid::from_u128(0x5C));

        let bound = r#"[{"agent_device_id":"TILL1","paired_device_id":"PAIR-A","oldest_unacknowledged_secs":420}]"#;
        trees
            .record_heartbeat(tenant, store_id, 11_000, None, None, Some(bound.to_owned()))
            .await
            .expect("record a heartbeat carrying one bound agent");
        let (stored, reported_at) = agents(&admin, tenant, store_id).await;
        assert!(
            stored
                .as_deref()
                .is_some_and(|json| json.contains("TILL1") && json.contains("PAIR-A")),
            "the standing the store reported is the standing the column holds: {stored:?}"
        );
        assert_eq!(
            reported_at,
            Some(11_000),
            "stamped with the beat that carried it, so a stale list reads as stale"
        );

        trees
            .record_heartbeat(tenant, store_id, 12_000, None, None, None)
            .await
            .expect("record a heartbeat that says nothing about agents");
        let (stored, reported_at) = agents(&admin, tenant, store_id).await;
        assert!(
            stored.as_deref().is_some_and(|json| json.contains("TILL1")),
            "an absent report leaves the last standing alone — an older edge must not erase it"
        );
        assert_eq!(
            reported_at,
            Some(11_000),
            "and leaves its instant alone too"
        );

        trees
            .record_heartbeat(tenant, store_id, 13_000, None, None, Some("[]".to_owned()))
            .await
            .expect("record a heartbeat reporting no agents at all");
        let (stored, reported_at) = agents(&admin, tenant, store_id).await;
        assert_eq!(
            stored.as_deref(),
            Some("[]"),
            "an empty report replaces: the manager released the last terminal, and the console \
             must stop showing an agent nobody is bound to"
        );
        assert_eq!(
            reported_at,
            Some(13_000),
            "and the instant moves with it, because saying 'none' is saying something"
        );
    });
}
