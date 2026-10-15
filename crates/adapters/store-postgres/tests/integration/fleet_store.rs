// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The fleet read model: identity + liveness + config drift + relay backlog (ADR-0068 slice 3).

use super::{block_on, prepared};
use pos_proto::{StoreId, TenantId, Ulid};

/// The placement the bump wrote comes back out of the fleet read
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// Two tables and one join, and neither the unit tests nor the in-process suite can prove it:
/// upstream the placement lives on a `FakeConfigTrees` and the fleet rows on a `FakeFleet`, two
/// unrelated `Arc<Mutex<_>>`s with no join between them. Only a real database makes the bump
/// visible to `FLEET_SELECT`. This is also the assertion that catches the failure the adapter's
/// positional decode invites: `edge_placement` is column 17, and reading it at the wrong index
/// would return a neighbouring value or panic, with nothing at compile time to notice.
/// The bump records the generation it displaced, and only a heartbeat that names *that*
/// generation with an empty outbox clears it
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// Every assertion here is unavailable anywhere else in the tree. The fake models the maps but
/// not the SQL, and the two facts the clear turns on — that `store_lease.generation` inside
/// `ON CONFLICT DO UPDATE` is the *pre-update* value, and that the clear reads the request's
/// numbers rather than the stored liveness row — are properties of the statements, not of the
/// Rust around them.
/// Opens a handover on a fresh store: registers it, issues generation 0, then bumps to 1 so
/// generation 0 is in flight. The setup both hand-made cases need, and neither is about.
async fn opened(
    store: &store_postgres::PostgresStore,
    tenant: TenantId,
    store_id: StoreId,
) -> store_postgres::PostgresConfigTrees {
    let trees = store.config_trees();
    store
        .registry()
        .insert_store(
            &store_id.to_string(),
            &tenant.to_string(),
            None,
            "Xuân Thủy",
        )
        .await
        .expect("register the store");
    super::issue(
        &trees,
        tenant,
        store_id,
        1_777_000_000_000,
        None,
        None,
        None,
    )
    .await;
    super::issue(
        &trees,
        tenant,
        store_id,
        1_777_000_001_000,
        None,
        None,
        Some(0),
    )
    .await;
    trees
}

/// Settling by hand, against the real statement
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// What only a database shows: the precondition lives in the `WHERE`, so an attestation about a
/// machine the row does not name changes nothing — and a second attestation about the same one
/// is refused rather than being a quiet no-op.
#[test]
fn a_handover_settles_only_on_the_machine_the_caller_names() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let tenant = TenantId::new(Ulid::from_u128(0x0E3A));
        let store_id = StoreId::new(Ulid::from_u128(0x0E3B));

        // Nothing issued yet: there is no handover to settle.
        assert_eq!(
            store
                .config_trees()
                .settle_handover(tenant, store_id, 0)
                .await
                .expect("settle reached the database"),
            store_postgres::StoredSettle::NoLease,
        );

        let trees = opened(&store, tenant, store_id).await;

        // Attesting about a machine that is not the one in flight reports what is.
        assert_eq!(
            trees
                .settle_handover(tenant, store_id, 7)
                .await
                .expect("settle reached the database"),
            store_postgres::StoredSettle::NotSuperseded { current: Some(0) },
        );
        assert_eq!(
            trees
                .settle_handover(tenant, store_id, 0)
                .await
                .expect("settle reached the database"),
            store_postgres::StoredSettle::Settled,
        );
        // Settling twice is refused for the same reason: the second attestation would be about
        // a machine the row no longer names.
        assert_eq!(
            trees
                .settle_handover(tenant, store_id, 0)
                .await
                .expect("settle reached the database"),
            store_postgres::StoredSettle::NotSuperseded { current: None },
        );
    });
}

/// A store on its first-ever lease has no previous machine, and the statement says so
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// Generation `0` is the store's first lease: the insert in `bump_store_lease` writes
/// `superseded_generation = NULL` for exactly that reason — it supersedes nobody. So the other
/// two preconditions (`superseded_generation IS NULL AND retired_at IS NULL`) both *hold* on a
/// brand-new store, and without `generation > 0` the retire succeeded: an operator could record
/// that the only machine the shop has was no longer needed.
///
/// Only a database proves this. The condition is three words inside a `WHERE`, and the upstream
/// fake mirrors it by hand — a fake agreeing with itself is not evidence about the statement
/// Postgres actually runs.
#[test]
fn a_store_on_its_first_lease_has_no_previous_machine_to_retire() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let tenant = TenantId::new(Ulid::from_u128(0x0E4E));
        let store_id = StoreId::new(Ulid::from_u128(0x0E4F));
        let trees = store.config_trees();
        store
            .registry()
            .insert_store(&store_id.to_string(), &tenant.to_string(), None, "Bà Triệu")
            .await
            .expect("register the store");

        // One bump only: generation 0, superseded_generation NULL, retired_at NULL.
        super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_000_000,
            None,
            None,
            None,
        )
        .await;

        assert_eq!(
            trees
                .retire_handover(tenant, store_id, 1_777_000_001_000, "admin-a")
                .await
                .expect("retire reached the database"),
            store_postgres::StoredRetire::NeverSuperseded,
        );

        // Nothing was written, so a second attempt is the same refusal rather than
        // `AlreadyRetired`. This is the assertion that fails if the precondition is dropped: a
        // recorded decision would change the answer.
        assert_eq!(
            trees
                .retire_handover(tenant, store_id, 1_777_000_002_000, "admin-b")
                .await
                .expect("retire reached the database"),
            store_postgres::StoredRetire::NeverSuperseded,
            "a refused retirement records no decision"
        );

        // And the store is retirable the moment it has actually handed over: bump to generation
        // 1, settle the machine that held 0, and the same call now succeeds. Without this the
        // test would pass just as well against a statement that refused every retirement.
        super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_003_000,
            None,
            None,
            Some(0),
        )
        .await;
        trees
            .settle_handover(tenant, store_id, 0)
            .await
            .expect("settle reached the database");
        assert_eq!(
            trees
                .retire_handover(tenant, store_id, 1_777_000_004_000, "admin-a")
                .await
                .expect("retire reached the database"),
            store_postgres::StoredRetire::Retired,
        );
    });
}

/// Retiring by hand, and the clear a bump performs in the same `SET` clause (migration `0054`).
///
/// The last assertion is the one only a database can make: a new bump *removes* the previous
/// retirement rather than leaving it, so a machine still in the shop never reads as decided
/// unnecessary.
#[test]
fn a_retirement_needs_a_settled_handover_is_recorded_once_and_a_bump_reopens_it() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let tenant = TenantId::new(Ulid::from_u128(0x0E4A));
        let store_id = StoreId::new(Ulid::from_u128(0x0E4B));

        assert_eq!(
            store
                .config_trees()
                .retire_handover(tenant, store_id, 1_777_000_000_000, "admin-a")
                .await
                .expect("retire reached the database"),
            store_postgres::StoredRetire::NoLease,
        );

        let trees = opened(&store, tenant, store_id).await;

        // Generation 0 may still hold a night's sales, so nobody gets to call it unnecessary.
        assert_eq!(
            trees
                .retire_handover(tenant, store_id, 1_777_000_002_000, "admin-a")
                .await
                .expect("retire reached the database"),
            store_postgres::StoredRetire::Undrained { superseded: 0 },
        );
        trees
            .settle_handover(tenant, store_id, 0)
            .await
            .expect("settle reached the database");
        assert_eq!(
            trees
                .retire_handover(tenant, store_id, 1_777_000_003_000, "admin-a")
                .await
                .expect("retire reached the database"),
            store_postgres::StoredRetire::Retired,
        );
        // A second retirement does not overwrite the first decision; it reports it.
        assert_eq!(
            trees
                .retire_handover(tenant, store_id, 1_777_000_004_000, "admin-b")
                .await
                .expect("retire reached the database"),
            store_postgres::StoredRetire::AlreadyRetired {
                at: 1_777_000_003_000,
                by: "admin-a".to_owned(),
            },
            "the row holds the person who decided, not the last person to ask"
        );

        // A new bump starts a new handover with a new outgoing machine, so the retirement stops
        // describing this row — cleared by the same statement, not by a second write.
        super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_005_000,
            None,
            None,
            Some(1),
        )
        .await;
        assert_eq!(
            trees
                .retire_handover(tenant, store_id, 1_777_000_006_000, "admin-c")
                .await
                .expect("retire reached the database"),
            store_postgres::StoredRetire::Undrained { superseded: 1 },
            "the new handover is in flight, and the old retirement is gone rather than stale"
        );
    });
}

#[test]
fn a_bump_records_the_superseded_generation_and_only_a_matching_drain_clears_it() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let registry = store.registry();
        let trees = store.config_trees();
        let fleet = store.fleet();
        let tenant = TenantId::new(Ulid::from_u128(0x0E2A));
        let store_id = StoreId::new(Ulid::from_u128(0x0E2B));
        let superseded = |rows: &[store_postgres::FleetStoreRow]| {
            rows.first().expect("one store").superseded_generation
        };

        registry
            .insert_store(
                &store_id.to_string(),
                &tenant.to_string(),
                None,
                "Thảo Điền",
            )
            .await
            .expect("register the store");

        // A first lease supersedes nobody: the INSERT branch writes NULL.
        let first = super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_000_000,
            None,
            None,
            None,
        )
        .await;
        assert_eq!(first.generation, 0);
        assert_eq!(
            first.superseded_generation, None,
            "a store's first-ever lease displaces no machine"
        );

        // The second bump displaces generation 0 and must say so.
        let second = super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_001_000,
            None,
            None,
            Some(0),
        )
        .await;
        assert_eq!(second.generation, 1);
        assert_eq!(
            second.superseded_generation,
            Some(0),
            "the bump records the number it displaced, read from the pre-update row"
        );
        let rows = fleet.list(&tenant.to_string()).await.expect("fleet");
        assert_eq!(superseded(&rows), Some(0), "and the fleet read carries it");

        // A heartbeat from the NEW machine, empty, does not clear: generation 1 says nothing
        // about whether generation 0's box drained.
        trees
            .record_heartbeat(tenant, store_id, 1_777_000_002_000, Some(0), Some(1), None)
            .await
            .expect("heartbeat from the new machine");
        let rows = fleet.list(&tenant.to_string()).await.expect("fleet");
        assert_eq!(
            superseded(&rows),
            Some(0),
            "an empty outbox at the wrong generation is not evidence about the right one"
        );

        // The old machine reporting generation 0 but still holding events does not clear either.
        trees
            .record_heartbeat(tenant, store_id, 1_777_000_003_000, Some(40), Some(0), None)
            .await
            .expect("heartbeat with a backlog");
        let rows = fleet.list(&tenant.to_string()).await.expect("fleet");
        assert_eq!(
            superseded(&rows),
            Some(0),
            "forty unsent sales is exactly the state the column exists to hold open"
        );

        // Both facts, from one message: generation 0, empty. Now it clears.
        trees
            .record_heartbeat(tenant, store_id, 1_777_000_004_000, Some(0), Some(0), None)
            .await
            .expect("the drained heartbeat");
        let rows = fleet.list(&tenant.to_string()).await.expect("fleet");
        assert_eq!(
            superseded(&rows),
            None,
            "the old machine reported its own generation empty, so the handover is released"
        );
    });
}

/// A beat that omits either fact clears nothing — ADR-0110's "an older edge that sends neither
/// simply is not yet provably settled", which falls out of SQL's three-valued logic rather than
/// needing a branch. Worth its own case because the omission is the *common* path: the edge
/// reports `outbox_depth: None` whenever its event log will not read.
#[test]
fn a_heartbeat_missing_either_fact_clears_nothing() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let registry = store.registry();
        let trees = store.config_trees();
        let fleet = store.fleet();
        let tenant = TenantId::new(Ulid::from_u128(0x0E3A));
        let store_id = StoreId::new(Ulid::from_u128(0x0E3B));

        registry
            .insert_store(
                &store_id.to_string(),
                &tenant.to_string(),
                None,
                "Bến Thành",
            )
            .await
            .expect("register the store");
        super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_000_000,
            None,
            None,
            None,
        )
        .await;
        super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_001_000,
            None,
            None,
            Some(0),
        )
        .await;

        for (depth, generation, why) in [
            (
                None,
                Some(0),
                "a beat with no depth cannot prove an empty outbox",
            ),
            (
                Some(0),
                None,
                "a beat with no generation cannot say which machine drained",
            ),
            (
                None,
                None,
                "an older edge that sends neither is not provably settled",
            ),
        ] {
            trees
                .record_heartbeat(tenant, store_id, 1_777_000_002_000, depth, generation, None)
                .await
                .expect("heartbeat");
            let rows = fleet.list(&tenant.to_string()).await.expect("fleet");
            assert_eq!(
                rows.first().expect("one store").superseded_generation,
                Some(0),
                "{why}"
            );
        }
    });
}

#[test]
fn a_bumped_placement_reaches_the_fleet_row() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let registry = store.registry();
        let trees = store.config_trees();
        let fleet = store.fleet();
        let tenant = TenantId::new(Ulid::from_u128(0x0F1E));
        let store_id = StoreId::new(Ulid::from_u128(0x0F1F));

        registry
            .insert_store(
                &store_id.to_string(),
                &tenant.to_string(),
                None,
                "Bến Thành",
            )
            .await
            .expect("register the store");

        // Before any bump there is no store_lease row, so the LEFT JOIN yields NULL — the state
        // every store in the fleet is in today.
        let rows = fleet.list(&tenant.to_string()).await.expect("list");
        let row = rows.first().expect("one store");
        assert_eq!(
            row.edge_placement, None,
            "a store with no lease row must report no placement, not a default"
        );

        super::issue(
            &trees,
            tenant,
            store_id,
            1_777_000_000_000,
            Some("EDGE_PLACEMENT_HOSTED_BY_PLATFORM"),
            None,
            None,
        )
        .await;

        let rows = fleet
            .list(&tenant.to_string())
            .await
            .expect("list after the bump");
        let row = rows.first().expect("one store");
        assert_eq!(
            row.edge_placement.as_deref(),
            Some("EDGE_PLACEMENT_HOSTED_BY_PLATFORM"),
            "the placement the bump wrote must reach the fleet read"
        );
        // The neighbouring column, to catch an off-by-one in the positional decode: the bump
        // above was this store's first, so its generation is 0 and not the placement's string.
        assert_eq!(row.lease_generation_authoritative, Some(0));
    });
}

/// The fleet read joins registry identity, liveness, config drift, and relay backlog into one row
/// per store, scoped to its tenant. A configured+seen store shows all four; a bare registered
/// store shows identity only; a different tenant sees nothing ([ADR-0068] slice 3).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: it seeds all four joined tables (registry, config tree, \
              liveness, order queue) and asserts every field of the joined row, plus fetch_one \
              and tenant-scope — splitting it would duplicate the multi-table setup"
)]
fn joins_identity_liveness_drift_and_backlog() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let registry = store.registry();
        let trees = store.config_trees();
        let fleet = store.fleet();

        let tenant = TenantId::new(Ulid::from_u128(0x000F_1EE7));
        let seen = StoreId::new(Ulid::from_u128(0x570A)); // configured + seen + a backlog
        let bare = StoreId::new(Ulid::from_u128(0x570B)); // registered, never seen, unconfigured

        registry
            .insert_store(&bare.to_string(), &tenant.to_string(), None, "Bare")
            .await
            .expect("insert the bare store");
        registry
            .insert_store(&seen.to_string(), &tenant.to_string(), None, "Seen")
            .await
            .expect("insert the seen store");

        // The seen store holds v1 while the published history's last id is v2 — a drift the read
        // surfaces by comparison, not storage.
        let held = "0000000000CONFIGVERSIONV1AA";
        let published = "0000000000CONFIGVERSIONV2AA";
        let state = format!(
            r#"{{"k":20,"layers":[{{}},{{}},{{}},{{}}],"history":[{{"id":"{held}","effective":{{}}}},{{"id":"{published}","effective":{{}}}}]}}"#
        );
        trees
            .save_state(tenant, seen, &state, None)
            .await
            .expect("save the config tree");
        trees
            .record_seen(tenant, seen, Some(held), 1000)
            .await
            .expect("record the pull");

        // Two orders queued for the seen store: one still pending (arrived at epoch 1_234_567s),
        // one already reported. Only the pending one is backlog.
        admin
            .execute(
                "INSERT INTO order_queue \
                 (tenant_id, store_id, sales_channel, external_reference, queued_id, payload, status, created_at) \
                 VALUES ($1, $2, 'grab', 'ref-pending', 'q-pending', '{}'::jsonb, 'pending', to_timestamp(1234567.0)), \
                        ($1, $2, 'grab', 'ref-reported', 'q-reported', '{}'::jsonb, 'reported', now())",
                &[&tenant.to_string(), &seen.to_string()],
            )
            .await
            .expect("seed the order queue");

        let rows = fleet
            .list(&tenant.to_string())
            .await
            .expect("list the fleet");
        assert_eq!(rows.len(), 2, "both of the tenant's stores are listed");

        // Look rows up by id — created_at can tie for two inserts in the same millisecond, so the
        // list order is not asserted here.
        let seen_row = rows
            .iter()
            .find(|row| row.store_id == seen.to_string())
            .expect("the seen store is present");
        assert_eq!(seen_row.name, "Seen");
        assert_eq!(seen_row.status, "active");
        assert_eq!(
            seen_row.last_seen_at_ms,
            Some(1000),
            "last-seen from the pull"
        );
        assert_eq!(seen_row.last_config_pull_at_ms, Some(1000));
        assert_eq!(
            seen_row.config_version_held.as_deref(),
            Some(held),
            "the held version the edge reported"
        );
        assert_eq!(
            seen_row.config_version_published.as_deref(),
            Some(published),
            "the published version is the last history id"
        );
        assert_eq!(
            seen_row.relay_backlog, 1,
            "only the pending order counts toward the backlog"
        );
        assert_eq!(
            seen_row.oldest_pending_at_ms,
            Some(1_234_567_000),
            "the oldest pending order's arrival, in Unix ms"
        );

        let bare_row = rows
            .iter()
            .find(|row| row.store_id == bare.to_string())
            .expect("the bare store is present");
        assert_eq!(bare_row.name, "Bare");
        assert_eq!(
            bare_row.last_seen_at_ms, None,
            "a store that never checked in has no last-seen"
        );
        assert_eq!(bare_row.config_version_held, None);
        assert_eq!(
            bare_row.config_version_published, None,
            "an unconfigured store has no published version"
        );
        assert_eq!(bare_row.relay_backlog, 0);
        assert_eq!(bare_row.oldest_pending_at_ms, None);

        // fetch_one returns exactly one store; an unknown store is None.
        let one = fleet
            .fetch_one(&tenant.to_string(), &seen.to_string())
            .await
            .expect("fetch one")
            .expect("the seen store is present");
        assert_eq!(one.name, "Seen");
        assert_eq!(one.relay_backlog, 1);
        let unknown = StoreId::new(Ulid::from_u128(0xDEAD));
        assert!(
            fleet
                .fetch_one(&tenant.to_string(), &unknown.to_string())
                .await
                .expect("fetch one")
                .is_none(),
            "an unknown store reads as None"
        );

        // A different tenant with the same store ids sees nothing — the read is tenant-scoped.
        let other = TenantId::new(Ulid::from_u128(0xB0B));
        assert!(
            fleet
                .list(&other.to_string())
                .await
                .expect("list the other tenant")
                .is_empty(),
            "the fleet read is scoped to the tenant"
        );
    });
}
