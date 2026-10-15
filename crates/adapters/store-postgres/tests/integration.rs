// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `store-postgres` against a live PostgreSQL.
//!
//! Two things are proven here, and neither can be proven any other way:
//!
//!  1. **The shared `EventStore` contract** ([`pos_contract_tests::event_store_suite`]) — the same
//!     twelve cases that run against `store-sqlite` and the in-memory fake, so the three are known
//!     to agree rather than assumed to. Ordered read-back, idempotency by ULID, and survival of a
//!     crash mid-transaction, plus the outbox obligations.
//!  2. **Cloud-specific behaviour the port contract does not reach**: row-level tenant isolation
//!     (a non-owner role sees only its tenant's rows, and nothing at all with no tenant set) and
//!     monthly partition routing (ADR-0022). RLS is the single worst multi-tenant failure to get
//!     wrong, so it is tested against the real planner rather than reasoned about.
//!
//! # Why one test binary, run single-threaded
//!
//! Every case shares one database, so `fresh()` truncates between cases and the whole binary must
//! run with `--test-threads=1`. Cargo runs separate integration-test *binaries* concurrently, so
//! splitting these across binaries would reintroduce exactly the races the truncation is there to
//! avoid — hence one binary. Its areas are modules in `tests/integration/`, which this file names
//! by `#[path]`, so they compile into it rather than into binaries of their own. See the crate's
//! `integration` feature note in `Cargo.toml`.
//!
//! Run it:
//!
//! ```text
//! DATABASE_URL='host=localhost port=55432 user=pos dbname=poscloud' \
//!   cargo test -p store-postgres --features integration -- --test-threads=1
//! ```

// Gated behind the `integration` feature so an ordinary `cargo test` neither compiles nor runs it:
// it needs a live database. Both the pull-request `postgres` job and the merge-to-`main` integration
// job turn the feature on against the same pinned `postgres:16` digest — deliberately the same one,
// since two gates disagreeing on the version is worse than one gate.
#![cfg(feature = "integration")]
// The whole file is test scaffolding. `allow-expect-in-tests` in clippy.toml scopes to `#[test]`
// and `#[cfg(test)]`, which does not reach an integration test's module-level helpers, so the
// harness setup (a runtime, a connection) is allowed to panic here explicitly.
#![allow(
    clippy::expect_used,
    clippy::needless_pass_by_value,
    reason = "test scaffolding: a missing DATABASE_URL or an unreachable database is an \
              unrecoverable test-setup fault, not a contract failure; and the error-to-HarnessError \
              converters take their error by value so they can be used point-free with `map_err`"
)]

use std::future::Future;

use pos_contract_tests::fixtures;
use pos_contract_tests::harness::{EventStoreHarness, HarnessError, Setup};
use pos_proto::{BusinessDate, StoreId, TenantId, Ulid};
use store_postgres::PostgresStore;
use tokio_postgres::{Client, NoTls};

/// The libpq connection string the tests connect with.
///
/// # Panics
///
/// If `DATABASE_URL` is unset. The `integration` feature is only ever enabled where a database is
/// present, so an unset variable is a misconfigured run, not a case to skip silently.
fn database_url() -> String {
    std::env::var("DATABASE_URL").expect(
        "DATABASE_URL must be set for the `integration` feature — e.g. \
         host=localhost port=55432 user=pos dbname=poscloud",
    )
}

/// Drives a future on a fresh multi-thread runtime with IO enabled.
///
/// Multi-thread with `enable_all` rather than the fake's one-poll executor, because `tokio-postgres`
/// needs the IO driver for its socket and `deadpool` spawns each connection's driver task — a
/// current-thread runtime with no IO would hang on the first query.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a multi-thread tokio runtime")
        .block_on(future)
}

/// Opens a raw admin connection (the superuser the tests connect as) and drives its protocol task.
///
/// Separate from the store's pool because the harness does privileged setup the port has no
/// business exposing — terminating stray backends, truncating, and (for the RLS cases) assuming
/// the `app_tenant` role.
async fn admin() -> Setup<Client> {
    let (client, connection) = tokio_postgres::connect(&database_url(), NoTls)
        .await
        .map_err(db_err)?;
    // The connection task must be polled for the client to make progress; it ends when the client
    // is dropped at the close of the call that opened it.
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

fn port_err(error: pos_ports::PortError) -> HarnessError {
    HarnessError::new(error.to_string())
}

fn db_err(error: tokio_postgres::Error) -> HarnessError {
    HarnessError::new(error.to_string())
}

/// The `EventStore` harness over a live database.
///
/// Holds nothing but the knowledge of how to reach the database: a store is built fresh per case,
/// and a clean slate is a truncation rather than a new file, since there is only one database.
struct StoreHarness;

impl StoreHarness {
    fn new() -> Self {
        Self
    }
}

impl EventStoreHarness for StoreHarness {
    type Store = PostgresStore;

    async fn fresh(&self) -> Setup<PostgresStore> {
        // Idempotent schema first (safe on every boot), so a cold database is handled and the
        // truncation below has tables to truncate.
        let store = PostgresStore::connect(&database_url()).map_err(port_err)?;
        store.migrate().await.map_err(port_err)?;

        let admin = admin().await?;
        // A clean slate. `pg_terminate_backend` first, because the crash-mid-transaction case
        // deliberately leaves a backend idle-in-transaction; its row-exclusive lock would make the
        // TRUNCATE (which needs an access-exclusive lock) wait on a transaction that never ends.
        // TRUNCATE on the partitioned parent cascades to every partition.
        admin
            .batch_execute(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                 WHERE datname = current_database() AND pid <> pg_backend_pid() \
                   AND state IN ('idle in transaction', 'idle in transaction (aborted)'); \
                 TRUNCATE activation_codes, admin_invites, admin_recovery_codes, admin_sessions, admin_users, alerts, \
                 api_keys, audit_log, brands, campaigns, chain_anchor_conflicts, chain_anchors, \
                 catalog_display_categories, \
                 catalog_display_subcategories, catalog_item_categories, catalog_item_subcategories, \
                 catalog_items, catalog_layout_buttons, catalog_menu_sections, catalog_menus, \
                 catalog_modifier_groups, catalog_placements, catalog_tax_classes, catalog_tax_rate_versions, \
                 catalog_tax_rates, config_trees, data_migrations, device_claims, device_credentials, device_proposals, devices, \
                 employee_scope_assignments, employee_store_assignments, employees, event_outbox, events, fee_rules, floor_areas, floor_tables, \
                 integration_connections, inventory_items, kitchen_stations, media_assets, order_queue, ota_releases, \
                 people_republishes, reconcile_runs, \
                 reason_codes, releases, role_templates, rollups, scheduled_publishes, setting_values, station_routing_rules, \
                 store_group_members, store_groups, store_lease, \
             store_liveness, stores, \
                 subjects, super_admin, task_health, tenants, translations, vouchers, webhook_endpoints RESTART IDENTITY;",
            )
            .await
            .map_err(db_err)?;
        Ok(store)
    }

    async fn lose_power(&self, store: PostgresStore) -> Setup<PostgresStore> {
        // PostgreSQL's durability is server-side: every committed transaction is on disk, and the
        // one the "crash" left open is invisible to all other connections and is rolled back when
        // its pooled connection is next recycled (the pool recycles with `ROLLBACK` — see
        // `PostgresStore::connect`). There is nothing buffered on our side to lose — unlike the
        // file-backed adapter — so reopening after power loss is handing the same store back.
        Ok(store)
    }

    fn chains(&self) -> bool {
        // **No, and deliberately.** The cloud's log is a durable copy of chains the *stores*
        // stamped — each envelope arrives carrying the link its store wrote, and the cloud verifies
        // those links rather than minting its own (ADR-0132). A second chain here would be the
        // cloud vouching for itself, which is worth nothing: the whole value of the anchor is that
        // it is held somewhere the party being checked cannot reach.
        //
        // So `chain_head` answers `None`, and the suite holds this adapter to *keeping* that answer
        // rather than inventing a head.
        false
    }

    fn store_id(&self) -> StoreId {
        StoreId::new(Ulid::from_u128(0x0ADA))
    }
}

mod event_store {
    use super::{StoreHarness, block_on};
    pos_contract_tests::event_store_suite!(StoreHarness::new(), block_on);
}

// ---------------------------------------------------------------------------
// Cloud-specific behaviour beyond the shared port contract.
// ---------------------------------------------------------------------------

/// A tenant that owns some rows, and one that owns others.
const TENANT_A: &str = "tenant-a";
const TENANT_B: &str = "tenant-b";

/// Applies the schema and returns a clean database plus an admin connection.
async fn prepared() -> Setup<(PostgresStore, Client)> {
    let store = PostgresStore::connect(&database_url()).map_err(port_err)?;
    store.migrate().await.map_err(port_err)?;
    let admin = admin().await?;
    admin
        .batch_execute(
            "TRUNCATE activation_codes, admin_invites, admin_recovery_codes, admin_sessions, admin_users, alerts, \
             api_keys, audit_log, brands, campaigns, chain_anchor_conflicts, chain_anchors, \
             catalog_display_categories, \
             catalog_display_subcategories, catalog_item_categories, catalog_item_subcategories, \
             catalog_items, catalog_layout_buttons, catalog_menu_sections, catalog_menus, \
             catalog_modifier_groups, catalog_placements, catalog_tax_classes, catalog_tax_rate_versions, \
             catalog_tax_rates, config_trees, data_migrations, device_claims, device_credentials, device_proposals, devices, \
             employee_scope_assignments, employee_store_assignments, employees, event_outbox, events, fee_rules, floor_areas, floor_tables, \
             integration_connections, inventory_items, kitchen_stations, media_assets, order_queue, ota_releases, \
             people_republishes, reconcile_runs, \
             reason_codes, releases, role_templates, rollups, scheduled_publishes, setting_values, station_routing_rules, \
                 store_group_members, store_groups, store_lease, \
             store_liveness, stores, \
             subjects, super_admin, task_health, tenants, translations, vouchers, webhook_endpoints RESTART IDENTITY",
        )
        .await
        .map_err(db_err)?;
    Ok((store, admin))
}

/// Inserts a bare row as the table owner (RLS bypassed), for the isolation cases.
async fn seed_row(admin: &Client, seed: i32, tenant: &str, store: &str) -> Setup<()> {
    admin
        .execute(
            "INSERT INTO events (business_date, event_id, tenant_id, store_id, envelope) \
             VALUES (DATE '2026-01-01', $1, $2, $3, '{}'::json)",
            &[&format!("EVT{seed:022}"), &tenant, &store],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Inserts a bare row on a named trading day, for the chain-window cases.
async fn seed_row_on(
    admin: &Client,
    seed: i32,
    tenant: &str,
    store: &str,
    business_date: &str,
) -> Setup<()> {
    admin
        .execute(
            "INSERT INTO events (business_date, event_id, tenant_id, store_id, envelope) \
             VALUES (to_date($4, 'YYYY-MM-DD'), $1, $2, $3, '{}'::json)",
            &[&format!("EVT{seed:022}"), &tenant, &store, &business_date],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

/// A session with no tenant set sees nothing — default-deny, the property that turns a forgotten
/// `SET app.tenant_id` into an empty result rather than a cross-tenant leak.
#[test]
fn rls_denies_a_read_with_no_tenant_set() {
    block_on(async {
        let (_store, admin) = prepared().await.expect("prepare the database");
        seed_row(&admin, 1, TENANT_A, "store-1")
            .await
            .expect("seed tenant A");

        // Become the un-privileged query role. As a non-owner, non-superuser role, RLS now applies.
        admin
            .batch_execute("SET ROLE app_tenant")
            .await
            .expect("assume the app_tenant role");
        let count: i64 = admin
            .query_one("SELECT count(*) FROM events", &[])
            .await
            .expect("count is readable")
            .get(0);
        admin
            .batch_execute("RESET ROLE")
            .await
            .expect("reset the role");

        assert_eq!(
            count, 0,
            "with app.tenant_id unset the policy predicate is NULL and every row is filtered — a \
             query that forgets its tenant must see nothing, not everything"
        );
    });
}

/// With a tenant set, a session sees that tenant's rows and only those.
#[test]
fn rls_shows_a_tenant_only_its_own_rows() {
    block_on(async {
        let (_store, admin) = prepared().await.expect("prepare the database");
        seed_row(&admin, 1, TENANT_A, "store-1")
            .await
            .expect("seed A #1");
        seed_row(&admin, 2, TENANT_A, "store-1")
            .await
            .expect("seed A #2");
        seed_row(&admin, 3, TENANT_B, "store-2")
            .await
            .expect("seed B #1");

        admin
            .batch_execute("SET ROLE app_tenant")
            .await
            .expect("assume the app_tenant role");

        // set_config rather than string interpolation, so the tenant value is a bound parameter.
        admin
            .execute(
                "SELECT set_config('app.tenant_id', $1, false)",
                &[&TENANT_A],
            )
            .await
            .expect("scope the session to tenant A");
        let a_count: i64 = admin
            .query_one("SELECT count(*) FROM events", &[])
            .await
            .expect("count for A")
            .get(0);
        let a_all_match: bool = admin
            .query_one("SELECT bool_and(tenant_id = $1) FROM events", &[&TENANT_A])
            .await
            .expect("tenant column for A")
            .get(0);

        admin
            .execute(
                "SELECT set_config('app.tenant_id', $1, false)",
                &[&TENANT_B],
            )
            .await
            .expect("scope the session to tenant B");
        let b_count: i64 = admin
            .query_one("SELECT count(*) FROM events", &[])
            .await
            .expect("count for B")
            .get(0);

        admin
            .batch_execute("RESET ROLE")
            .await
            .expect("reset the role");

        assert_eq!(a_count, 2, "tenant A sees its two rows");
        assert!(a_all_match, "and sees nothing that is not tenant A's");
        assert_eq!(
            b_count, 1,
            "tenant B sees its one row, across the same table"
        );
    });
}

/// An ensured month gets its own partition and rows route to it; an un-ensured month lands in the
/// default partition rather than being lost (ADR-0022).
#[test]
fn partitions_route_by_business_month() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let store_id = StoreHarness.store_id();

        // Ensure March 2026; leave July 2026 without an explicit partition.
        store
            .ensure_partition("2026-03-15")
            .await
            .expect("create the March partition");
        // Idempotent: a second call for the same month is a no-op, not an error.
        store
            .ensure_partition("2026-03-20")
            .await
            .expect("re-ensuring the same month is a no-op");

        let march = append_dated(&store, store_id, 1, 2026, 3, 15).await;
        let july = append_dated(&store, store_id, 2, 2026, 7, 1).await;

        let march_partition = partition_of(&admin, &march).await;
        let july_partition = partition_of(&admin, &july).await;

        assert_eq!(
            march_partition, "events_p_2026_03",
            "an event dated in an ensured month lives in that month's partition"
        );
        assert_eq!(
            july_partition, "events_default",
            "an event dated in a month with no partition is caught by the default, not dropped"
        );
    });
}

/// Appends one activation event carrying an explicit business date, and returns its `event_id`
/// as the text stored in the table.
async fn append_dated(
    store: &PostgresStore,
    store_id: StoreId,
    seed: u32,
    year: i16,
    month: u8,
    day: u8,
) -> String {
    use pos_ports::{EventStore, Transactional, TxContext};

    let mut event = fixtures::activation(store_id, seed);
    event.business_date = BusinessDate::from_ymd(year, month, day).expect("a valid business date");
    let event_id = event.event_id.to_string();

    let mut tx = store.begin().await.expect("begin");
    store
        .append(&mut tx, core::slice::from_ref(&event))
        .await
        .expect("append the dated event");
    tx.commit().await.expect("commit");
    event_id
}

/// The name of the physical partition a row lives in, read via its `tableoid`.
async fn partition_of(admin: &Client, event_id: &str) -> String {
    admin
        .query_one(
            "SELECT tableoid::regclass::text FROM events WHERE event_id = $1",
            &[&event_id],
        )
        .await
        .expect("the row is somewhere")
        .get(0)
}

// ---------------------------------------------------------------------------
// The materialised-rollup table (ADR-0036): the read path the public /v1 dashboard uses.
// ---------------------------------------------------------------------------

/// A rollup is keyed by `(tenant, store)`, so a read names its own tenant and can only return that
/// tenant's row. This is the isolation the `/v1` dashboard rests on: the tenant comes from the
/// caller's authenticated grant, never the request, and a guessed foreign `store_id` finds nothing.
#[test]
fn rollups_are_isolated_by_the_tenant_store_key() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let rollups = store.rollups();
        let tenant_a = TenantId::new(Ulid::from_u128(0x0A));
        let tenant_b = TenantId::new(Ulid::from_u128(0x0B));
        let store_id = StoreId::new(Ulid::from_u128(0x5));

        // Tenant A materialises a rollup for the store.
        rollups
            .save_state(
                tenant_a,
                store_id,
                r#"{"cursor":null,"days":{"2026-01-01":{"business_date":"2026-01-01","total_events":7,"by_type":{}}}}"#,
            )
            .await
            .expect("tenant A saves its rollup");

        let a = rollups
            .load_state(tenant_a, store_id)
            .await
            .expect("load for A")
            .expect("tenant A sees the rollup it saved");
        let value: serde_json::Value = serde_json::from_str(&a).expect("valid jsonb");
        let total = value
            .pointer("/days/2026-01-01/total_events")
            .and_then(serde_json::Value::as_i64);
        assert_eq!(total, Some(7), "A reads back exactly what it stored");

        // Tenant B, naming the very same store id, finds no row — the key includes the tenant.
        let b = rollups
            .load_state(tenant_b, store_id)
            .await
            .expect("load for B");
        assert!(
            b.is_none(),
            "a foreign tenant's read of the same store id returns nothing, not A's data"
        );
    });
}

/// Belt-and-suspenders behind the key: the rollup table also carries RLS, so a query role assuming
/// `app_tenant` sees only its own tenant's rollup rows even across the same store id.
#[test]
fn rls_isolates_rollup_rows_by_tenant() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let rollups = store.rollups();
        let tenant_a = TenantId::new(Ulid::from_u128(0x0A));
        let tenant_b = TenantId::new(Ulid::from_u128(0x0B));
        let store_id = StoreId::new(Ulid::from_u128(0x5));

        rollups
            .save_state(tenant_a, store_id, r#"{"cursor":null,"days":{}}"#)
            .await
            .expect("A saves");
        rollups
            .save_state(tenant_b, store_id, r#"{"cursor":null,"days":{}}"#)
            .await
            .expect("B saves");

        admin
            .batch_execute("SET ROLE app_tenant")
            .await
            .expect("assume the app_tenant role");
        admin
            .execute(
                "SELECT set_config('app.tenant_id', $1, false)",
                &[&tenant_a.to_string()],
            )
            .await
            .expect("scope the session to tenant A");
        let a_count: i64 = admin
            .query_one("SELECT count(*) FROM rollups", &[])
            .await
            .expect("count A's rollup rows")
            .get(0);
        admin
            .batch_execute("RESET ROLE")
            .await
            .expect("reset the role");

        assert_eq!(
            a_count, 1,
            "app_tenant scoped to A sees only A's rollup row, though both tenants have one for the \
             same store"
        );
    });
}

// ---------------------------------------------------------------------------
// The super-admin credential and session store (ADR-0034).
// ---------------------------------------------------------------------------

#[path = "integration/admin_store.rs"]
mod admin_store;

// ---------------------------------------------------------------------------
// The scoped per-tenant API-key store (ADR-0037).
// ---------------------------------------------------------------------------

#[path = "integration/api_keys_store.rs"]
mod api_keys_store;

// ---------------------------------------------------------------------------
// The activation-code and device-credential store (ADR-0050).
// ---------------------------------------------------------------------------

#[path = "integration/activation_codes.rs"]
mod activation_codes;

// ---------------------------------------------------------------------------
// The receipt floor a lease bump publishes (ADR-0149).
// ---------------------------------------------------------------------------

#[path = "integration/receipt_floor.rs"]
mod receipt_floor;

// ---------------------------------------------------------------------------
// The device-claim store (ADR-0148).
// ---------------------------------------------------------------------------

#[path = "integration/claims.rs"]
mod claims;

// ---------------------------------------------------------------------------
// The four-level config-tree store (ADR-0033).
// ---------------------------------------------------------------------------

#[path = "integration/config_tree_store.rs"]
mod config_tree_store;

// ---------------------------------------------------------------------------
// The fleet read model: identity + liveness + config drift + relay backlog (ADR-0068 slice 3).
// ---------------------------------------------------------------------------

/// A bump that must be issued, with its `If-Match` expectation and any acknowledgement spelled
/// out at the call site.
///
/// Every bump now carries a precondition, so a test that does not name one is not testing the
/// route callers use. `expected` is the generation the row must be at — `None` asserts the store
/// has never been issued a lease — and `acknowledge` names an undrained generation the bump is
/// knowingly abandoning.
async fn issue(
    trees: &store_postgres::PostgresConfigTrees,
    tenant: TenantId,
    store_id: StoreId,
    at_ms: i64,
    placement: Option<&str>,
    acknowledge: Option<i64>,
    expected: Option<i64>,
) -> store_postgres::StoredBump {
    // `Keep` is what a caller that says nothing about the region means, and it is what every
    // pre-ADR-0114 bump in this file means. A case that is *about* the region calls
    // `issue_with_region` and names one.
    issue_with_region(
        trees,
        tenant,
        store_id,
        at_ms,
        placement,
        store_postgres::StoredRegionWrite::Keep,
        acknowledge,
        expected,
    )
    .await
}

/// The same, naming what the bump does to the store's region
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
async fn issue_with_region(
    trees: &store_postgres::PostgresConfigTrees,
    tenant: TenantId,
    store_id: StoreId,
    at_ms: i64,
    placement: Option<&str>,
    region: store_postgres::StoredRegionWrite<'_>,
    acknowledge: Option<i64>,
    expected: Option<i64>,
) -> store_postgres::StoredBump {
    match trees
        .bump_store_lease(
            tenant,
            store_id,
            at_ms,
            placement,
            region,
            acknowledge,
            expected,
        )
        .await
        .expect("the bump reached the database")
    {
        store_postgres::BumpOutcome::Issued(stored) => Some(stored),
        // Both refusals are a test-authoring mistake here, not a behaviour under test: the caller
        // named the wrong precondition, or failed to acknowledge a handover it had itself caused.
        // The refusals get their own cases, where they are the assertion rather than the accident.
        store_postgres::BumpOutcome::VersionMismatch { .. }
        | store_postgres::BumpOutcome::Undrained { .. } => None,
    }
    .expect("the bump was issued; check the If-Match generation and any acknowledgement")
}

#[path = "integration/fleet_store.rs"]
mod fleet_store;

// ---------------------------------------------------------------------------
// Background-task health: last-tick per loop, upserted (ADR-0068 slice 4).
// ---------------------------------------------------------------------------

#[path = "integration/task_health.rs"]
mod task_health;

// ---------------------------------------------------------------------------
// Operational alerts: open→resolved lifecycle, partial-unique dedup (ADR-0073, Track O2).
// ---------------------------------------------------------------------------

#[path = "integration/alerts.rs"]
mod alerts;

// ---------------------------------------------------------------------------
// The per-(tax class × channel) tax rate table: wholesale replace, tenant-scoped (ADR-0074, M4).
// ---------------------------------------------------------------------------

#[path = "integration/tax_rates.rs"]
mod tax_rates;

// ---------------------------------------------------------------------------
// The translation grid: one jsonb row per tenant, versioned by its own xmin (ADR-0095).
// ---------------------------------------------------------------------------

#[path = "integration/translations.rs"]
mod translations;

// ---------------------------------------------------------------------------
// Campaign authoring: jsonb insert/list/get/update-at/delete, id ordering, tenant isolation
// (ADR-0077, ADR-0095).
// ---------------------------------------------------------------------------

#[path = "integration/campaigns.rs"]
mod campaigns;

// ---------------------------------------------------------------------------
// Voucher instances: batch insert, code uniqueness per tenant, list-by-campaign, isolation (ADR-0077).
// ---------------------------------------------------------------------------

#[path = "integration/vouchers.rs"]
mod vouchers;

// ---------------------------------------------------------------------------
// Scheduled publishes: schedule, due-by-time, list-for-store, cancel, mark-applied (ADR-0077).
// ---------------------------------------------------------------------------

#[path = "integration/scheduled_publishes.rs"]
mod scheduled_publishes;

// ---------------------------------------------------------------------------
// Media renditions: bytea round-trip, single-rendition read, tenant isolation, delete (ADR-0075).
// ---------------------------------------------------------------------------

#[path = "integration/media.rs"]
mod media;

// ---------------------------------------------------------------------------
// The console audit trail: append-only, tenant-scoped, INSERT/SELECT-only (ADR-0069).
// ---------------------------------------------------------------------------

#[path = "integration/audit_log.rs"]
mod audit_log;

// ---------------------------------------------------------------------------
// The employee store: tenant-scoped, RLS-isolated, PIN held only as its hash (ADR-0070).
// ---------------------------------------------------------------------------

#[path = "integration/employees_store.rs"]
mod employees_store;

// ---------------------------------------------------------------------------
// Role templates + per-store assignments: people & access (ADR-0070, M1 slice 2).
// ---------------------------------------------------------------------------

#[path = "integration/role_templates_and_assignments.rs"]
mod role_templates_and_assignments;

// ---------------------------------------------------------------------------
// The stores whose people the cloud publishes again by itself (migration 0083, ADR-0158).
// ---------------------------------------------------------------------------

#[path = "integration/people_republishes_queue.rs"]
mod people_republishes_queue;

// ---------------------------------------------------------------------------
// The subject store: retention / PII masking (ADR-0035).
// ---------------------------------------------------------------------------

mod subjects_store {
    use super::{block_on, prepared};

    /// A due row is fetched, masking scrubs it and stamps `masked_at`, and a masked row is neither
    /// re-fetched nor re-masked (the sweep's idempotence at the database).
    #[test]
    fn fetch_due_then_mask_is_idempotent() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let subjects = store.subjects();

            // Seed one unmasked subject collected at t=1000ms with obviously-fake placeholder PII.
            let id: &str = "SUBJECT0000000000000000AA";
            let tenant: &str = "TENANT000000000000000000AA";
            let collected: i64 = 1000;
            let fields: &str = r#"{"name":"name-placeholder","phone":"phone-placeholder"}"#;
            admin
                .execute(
                    // `$4::text::jsonb`, not `$4::jsonb`: the inner `::text` pins the bound parameter's
                    // inferred type to `text` (which `&str` serialises to) before the database casts it
                    // to `jsonb`. `$4::jsonb` alone makes Postgres infer the parameter itself as `jsonb`,
                    // which tokio-postgres rejects for a `&str` (WrongType). Same pattern the production
                    // writers use — subjects.rs, config_trees.rs, translations.rs, rollups.rs.
                    "INSERT INTO subjects (subject_id, tenant_id, collected_at, fields) \
                     VALUES ($1, $2, $3, $4::text::jsonb)",
                    &[&id, &tenant, &collected, &fields],
                )
                .await
                .expect("seed a subject");

            // Not yet due before its collection instant; due at or after it.
            assert!(
                subjects
                    .fetch_due(999, 100)
                    .await
                    .expect("fetch")
                    .is_empty(),
                "a record is not due before its collection instant"
            );
            let due = subjects.fetch_due(2000, 100).await.expect("fetch");
            assert_eq!(due.len(), 1);
            assert_eq!(due.first().expect("one row").subject_id, id);

            // Mask it: redact the values in place and stamp masked_at.
            let redacted: &str = r#"{"name":"[REDACTED]","phone":"[REDACTED]"}"#;
            assert!(
                subjects.mask(id, redacted, 5000).await.expect("mask"),
                "an unmasked row is masked"
            );

            // A masked row is neither returned by a sweep nor masked a second time.
            assert!(
                subjects
                    .fetch_due(9999, 100)
                    .await
                    .expect("fetch")
                    .is_empty(),
                "masked rows are excluded, which is what makes the sweep idempotent"
            );
            assert!(
                !subjects.mask(id, redacted, 6000).await.expect("re-mask"),
                "an already-masked row is not re-masked"
            );
        });
    }

    /// `fetch_one` is the subject-request tooling's read (ADR-0076): it returns one row scoped to its
    /// tenant, reports the real `masked_at`, and returns `None` for a wrong tenant or unknown id.
    #[test]
    fn fetch_one_is_tenant_scoped_and_reports_masked_at() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let subjects = store.subjects();

            let id: &str = "SUBJECT0000000000000000BB";
            let tenant: &str = "TENANT000000000000000000AA";
            let other_tenant: &str = "TENANT000000000000000000BB";
            let fields: &str = r#"{"name":"name-placeholder"}"#;
            admin
                .execute(
                    "INSERT INTO subjects (subject_id, tenant_id, collected_at, fields) \
                     VALUES ($1, $2, $3, $4::text::jsonb)",
                    &[&id, &tenant, &1000_i64, &fields],
                )
                .await
                .expect("seed a subject");

            // The owning tenant sees it, unmasked (masked_at is None).
            let row = subjects
                .fetch_one(id, tenant)
                .await
                .expect("fetch")
                .expect("the subject exists for its tenant");
            assert_eq!(row.subject_id, id);
            assert_eq!(row.masked_at_ms, None);

            // A different tenant cannot reach it, and an unknown id is None.
            assert!(
                subjects
                    .fetch_one(id, other_tenant)
                    .await
                    .expect("fetch")
                    .is_none(),
                "a subject is not visible to another tenant"
            );
            assert!(
                subjects
                    .fetch_one("SUBJECT0000000000000000ZZ", tenant)
                    .await
                    .expect("fetch")
                    .is_none()
            );

            // After masking, fetch_one reports the masked_at stamp.
            let redacted: &str = r#"{"name":"[REDACTED]"}"#;
            assert!(subjects.mask(id, redacted, 7000).await.expect("mask"));
            let masked = subjects
                .fetch_one(id, tenant)
                .await
                .expect("fetch")
                .expect("still present after masking");
            assert_eq!(masked.masked_at_ms, Some(7000));
        });
    }
}

// ---------------------------------------------------------------------------
// The webhook-endpoint store (ADR-0032): registration facts, the admin CRUD's
// tenant-scoped listing, and the delivery task's fleet-wide enabled load.
// ---------------------------------------------------------------------------

mod webhooks_store {
    use super::{block_on, prepared};
    use store_postgres::PostgresWebhooks;

    // Obviously-fake, ULID-shaped identifiers for the fixture endpoints — never real key material.
    const TENANT_A: &str = "TENANT000000000000000000AA";
    const TENANT_B: &str = "TENANT000000000000000000BB";
    const STORE_1: &str = "STORE0000000000000000000A1";
    const STORE_2: &str = "STORE0000000000000000000B1";
    const HOOK_A1: &str = "WEBHOOK00000000000000000A1";
    const HOOK_A2: &str = "WEBHOOK00000000000000000A2";
    const HOOK_B1: &str = "WEBHOOK00000000000000000B1";

    /// Registers two endpoints for tenant A and one for tenant B — the fixture both cases start from.
    async fn seed(hooks: &PostgresWebhooks) {
        hooks
            .create(
                HOOK_A1,
                TENANT_A,
                STORE_1,
                "https://a.example/hook1",
                "secret-a1",
            )
            .await
            .expect("register A's first endpoint");
        hooks
            .create(
                HOOK_A2,
                TENANT_A,
                STORE_1,
                "https://a.example/hook2",
                "secret-a2",
            )
            .await
            .expect("register A's second endpoint");
        hooks
            .create(
                HOOK_B1,
                TENANT_B,
                STORE_2,
                "https://b.example/hook",
                "secret-b1",
            )
            .await
            .expect("register B's endpoint");
    }

    /// The tenant-scoped listing hides the secret and isolates by tenant; the fleet-wide enabled
    /// load carries the secret; and a successful delivery advances the cursor.
    #[test]
    fn list_is_tenant_scoped_and_enabled_load_carries_the_secret() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let hooks = store.webhooks();
            seed(&hooks).await;

            // A sees its two, B sees its one, and the summary row has no secret field at all.
            let listed_a = hooks.list(TENANT_A).await.expect("list A");
            assert_eq!(listed_a.len(), 2, "tenant A sees its two endpoints");
            let mut a_ids: Vec<&str> = listed_a.iter().map(|row| row.id.as_str()).collect();
            a_ids.sort_unstable();
            assert_eq!(a_ids, vec![HOOK_A1, HOOK_A2]);
            assert!(
                listed_a
                    .iter()
                    .all(|row| row.cursor.is_none() && !row.disabled),
                "freshly-registered endpoints have no cursor and are enabled"
            );

            let listed_b = hooks.list(TENANT_B).await.expect("list B");
            assert_eq!(listed_b.len(), 1, "tenant B sees only its own endpoint");
            assert_eq!(
                listed_b.first().expect("one row").store_id,
                STORE_2,
                "and it is the store B registered against"
            );

            // The fleet-wide enabled load is what the delivery task uses: all three, each in full,
            // including the signing secret it must sign with. It is NOT tenant-scoped.
            let enabled = hooks.fetch_enabled().await.expect("fetch enabled");
            assert_eq!(enabled.len(), 3, "all three endpoints are enabled at first");
            let a1 = enabled
                .iter()
                .find(|row| row.id == HOOK_A1)
                .expect("A's first endpoint is in the enabled load");
            assert_eq!(a1.tenant_id, TENANT_A);
            assert_eq!(a1.secret, "secret-a1", "the secret is loaded for signing");
            assert!(a1.cursor.is_none(), "no cursor until the first delivery");

            // A successful delivery advances the cursor; the next enabled load reflects it.
            hooks
                .advance_cursor(HOOK_A1, "EVENT0000000000000000000X1")
                .await
                .expect("advance A1's cursor");
            let enabled = hooks.fetch_enabled().await.expect("re-fetch enabled");
            let a1 = enabled
                .iter()
                .find(|row| row.id == HOOK_A1)
                .expect("still enabled");
            assert_eq!(
                a1.cursor.as_deref(),
                Some("EVENT0000000000000000000X1"),
                "the cursor advanced to the last delivered event id"
            );
        });
    }

    /// Disabling drops an endpoint from the enabled load but keeps it listed as disabled; delete is
    /// tenant-scoped and a second delete of the same id removes nothing.
    #[test]
    fn disable_suppresses_and_delete_is_tenant_scoped() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let hooks = store.webhooks();
            seed(&hooks).await;

            // Disabling an endpoint (the breaker's 24-hour auto-disable) drops it from the enabled
            // load but keeps it in the tenant's listing, marked disabled.
            hooks
                .mark_disabled(HOOK_A2, true)
                .await
                .expect("disable A2");
            let enabled = hooks.fetch_enabled().await.expect("enabled after disable");
            assert_eq!(
                enabled.len(),
                2,
                "the disabled endpoint is not delivered to"
            );
            assert!(
                enabled.iter().all(|row| row.id != HOOK_A2),
                "A2 specifically is gone from the enabled load"
            );
            let listed_a = hooks.list(TENANT_A).await.expect("re-list A");
            let a2 = listed_a
                .iter()
                .find(|row| row.id == HOOK_A2)
                .expect("A2 still appears in the listing");
            assert!(a2.disabled, "and it reads disabled");

            // Delete is tenant-scoped: B cannot delete A's endpoint, A can, and a second delete of
            // the same id removes nothing.
            assert!(
                !hooks
                    .remove(TENANT_B, HOOK_A1)
                    .await
                    .expect("B's delete of A's id"),
                "a tenant cannot delete another tenant's endpoint"
            );
            assert!(
                hooks
                    .remove(TENANT_A, HOOK_A1)
                    .await
                    .expect("A deletes its own endpoint"),
                "the owning tenant removes its endpoint"
            );
            assert!(
                !hooks
                    .remove(TENANT_A, HOOK_A1)
                    .await
                    .expect("second delete"),
                "deleting an already-removed endpoint removes nothing"
            );
            assert_eq!(
                hooks
                    .list(TENANT_A)
                    .await
                    .expect("list A after delete")
                    .len(),
                1,
                "one of A's two endpoints remains"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// The reconciliation membership query (ADR-0040): which candidate ids the
// cloud already holds, scoped by tenant and store.
// ---------------------------------------------------------------------------

mod reconcile_query {
    use super::{TENANT_A, block_on, prepared, seed_row};

    /// The event id `seed_row` writes for `seed`.
    fn evt(seed: i32) -> String {
        format!("EVT{seed:022}")
    }

    #[test]
    fn present_event_ids_answers_membership_scoped_by_tenant_and_store() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            // Two events for (TENANT_A, store-1); one for a different store; one for a different
            // tenant — none of which must count toward store-1's membership.
            seed_row(&admin, 1, TENANT_A, "store-1")
                .await
                .expect("seed 1");
            seed_row(&admin, 2, TENANT_A, "store-1")
                .await
                .expect("seed 2");
            seed_row(&admin, 3, TENANT_A, "store-2")
                .await
                .expect("seed 3");
            seed_row(&admin, 4, "tenant-b", "store-1")
                .await
                .expect("seed 4");

            let reconcile = store.reconcile();
            // Candidates: 1 and 2 are present for store-1; 3 belongs to store-2; 9 was never written.
            let candidates = vec![evt(1), evt(2), evt(3), evt(9)];
            let present = reconcile
                .present_event_ids(TENANT_A, "store-1", &candidates)
                .await
                .expect("membership query");
            let mut present = present;
            present.sort();
            assert_eq!(
                present,
                vec![evt(1), evt(2)],
                "only ids actually in (TENANT_A, store-1) count — not store-2's, not tenant-b's, \
                 and not one that was never written"
            );

            // An empty candidate set is an empty answer and touches nothing.
            let none = reconcile
                .present_event_ids(TENANT_A, "store-1", &[])
                .await
                .expect("empty query");
            assert!(none.is_empty(), "no candidates, no membership");
        });
    }

    #[test]
    fn records_and_lists_reconciliation_runs_scoped_by_tenant_and_store() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let reconcile = store.reconcile();

            // Two runs for (TENANT_A, store-1) at different instants, one for store-2, and one for a
            // different tenant — the last two must not leak into TENANT_A's history.
            reconcile
                .record_reconcile_run("run-1", TENANT_A, "store-1", 10, 3, 1_000)
                .await
                .expect("record run 1");
            reconcile
                .record_reconcile_run("run-2", TENANT_A, "store-1", 8, 0, 2_000)
                .await
                .expect("record run 2");
            reconcile
                .record_reconcile_run("run-3", TENANT_A, "store-2", 5, 5, 1_500)
                .await
                .expect("record run 3");
            reconcile
                .record_reconcile_run("run-4", "tenant-b", "store-1", 9, 9, 3_000)
                .await
                .expect("record run 4");

            // Tenant-wide, newest first: store-1's two runs and store-2's one, ordered by `ran_at`.
            let all = reconcile
                .list_reconcile_runs(TENANT_A, None, 10)
                .await
                .expect("list tenant runs");
            let ids: Vec<&str> = all.iter().map(|row| row.run_id.as_str()).collect();
            assert_eq!(
                ids,
                vec!["run-2", "run-3", "run-1"],
                "TENANT_A's runs only, newest first; tenant-b's run-4 is not present"
            );
            let newest = all.first().expect("a newest run");
            assert_eq!(newest.store_id, "store-1");
            assert_eq!(newest.candidates_offered, 8);
            assert_eq!(newest.missing_found, 0);
            assert_eq!(newest.ran_at, 2_000);

            // Narrowed to one store.
            let store_one = reconcile
                .list_reconcile_runs(TENANT_A, Some("store-1"), 10)
                .await
                .expect("list store-1 runs");
            let ids: Vec<&str> = store_one.iter().map(|row| row.run_id.as_str()).collect();
            assert_eq!(ids, vec!["run-2", "run-1"], "only store-1's runs");

            // The limit caps the page.
            let capped = reconcile
                .list_reconcile_runs(TENANT_A, None, 1)
                .await
                .expect("list capped");
            assert_eq!(capped.len(), 1, "the limit caps the page");
            assert_eq!(
                capped.first().expect("a capped run").run_id,
                "run-2",
                "and keeps the newest"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// The device-proposal onboarding queue (ADR-0041): propose, list by status,
// and the one-way pending → approved/rejected resolve.
// ---------------------------------------------------------------------------

mod device_proposals {
    use pos_proto::devices::{DeviceConnection, PaperWidth};
    use pos_proto::printing::{ReceiptLanguage, ReceiptSecondLanguage};

    use super::{TENANT_A, block_on, port_err, prepared};

    #[test]
    fn propose_list_and_resolve_round_trip() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let devices = store.device_proposals();

            // Two proposals for store-1, one for store-2 — all pending.
            devices
                .create(
                    "DEV1",
                    TENANT_A,
                    "store-1",
                    "printer",
                    "Kitchen 1",
                    "10.0.0.1:9100",
                )
                .await
                .expect("propose 1");
            devices
                .create("DEV2", TENANT_A, "store-1", "kds", "Expo", "10.0.0.2")
                .await
                .expect("propose 2");
            devices
                .create(
                    "DEV3",
                    TENANT_A,
                    "store-2",
                    "printer",
                    "Bar",
                    "10.0.0.3:9100",
                )
                .await
                .expect("propose 3");

            // The store-scoped pending list sees only store-1's two; the tenant-wide queue sees all.
            let store_1_pending = devices
                .fetch(TENANT_A, Some("store-1"), "pending")
                .await
                .expect("store-1 pending");
            assert_eq!(store_1_pending.len(), 2, "only store-1's proposals");
            let all_pending = devices
                .fetch(TENANT_A, None, "pending")
                .await
                .expect("tenant pending");
            assert_eq!(all_pending.len(), 3, "the whole tenant's queue");

            // Approve one; it leaves the pending list and joins the approved one, carrying the two
            // facts approval adds (ADR-0100).
            assert!(
                devices
                    .mark(
                        TENANT_A,
                        "DEV1",
                        "approved",
                        Some(DeviceConnection::Usb),
                        None
                    )
                    .await
                    .expect("approve"),
                "a pending proposal is resolved"
            );
            let approved = devices
                .fetch(TENANT_A, Some("store-1"), "approved")
                .await
                .expect("store-1 approved");
            assert_eq!(approved.len(), 1);
            let row = approved.first().expect("one row");
            assert_eq!(row.id, "DEV1");
            // Stored, and stored in the *short* spelling — asserted here rather than left to prose.
            // This is the check that was missing: the seam wrote the config node's prefixed
            // `DEVICE_CONNECTION_USB` into this column instead, `compile_devices` prefixed that a
            // second time, and the store received a token no build can read.
            assert_eq!(
                row.connection.as_deref(),
                Some(DeviceConnection::Usb.short_name()),
                "approval's connection is stored, in the spelling every reader parses"
            );
            assert_eq!(
                row.station_id, None,
                "a counter printer serves the bill, not a station"
            );
            assert_eq!(
                devices
                    .fetch(TENANT_A, Some("store-1"), "pending")
                    .await
                    .expect("store-1 pending after approve")
                    .len(),
                1,
                "one of store-1's two is still pending"
            );

            // Resolving again is a no-op: the row is no longer pending.
            assert!(
                !devices
                    .mark(TENANT_A, "DEV1", "rejected", None, None)
                    .await
                    .expect("re-resolve"),
                "an already-resolved proposal does not transition again"
            );
            // And another tenant cannot resolve this tenant's proposal.
            assert!(
                !devices
                    .mark(
                        "tenant-b",
                        "DEV2",
                        "approved",
                        Some(DeviceConnection::Network),
                        None
                    )
                    .await
                    .expect("cross-tenant"),
                "the tenant scope stops one tenant resolving another's proposal"
            );
        });
    }

    /// A terminal is written approved, and an approved device points at it conditionally
    /// (ADR-0112).
    ///
    /// Three claims the fake cannot settle, because they are the database's: a terminal row is
    /// created already `approved` (there is no proposal for anyone to resolve), the agent pointer
    /// round-trips through the column the publish reads, and the conditional write is decided by
    /// `xmin` — a real version the adapter does not mint itself.
    #[test]
    fn a_terminal_is_created_approved_and_an_agent_pick_is_conditional() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let devices = store.device_proposals();

            devices
                .create_terminal("TILL1", TENANT_A, "store-1", "Till 1")
                .await
                .expect("create the terminal");
            devices
                .create(
                    "PRN1",
                    TENANT_A,
                    "store-1",
                    "printer",
                    "Counter",
                    "/dev/usb/lp0",
                )
                .await
                .expect("propose the printer");
            devices
                .mark(
                    TENANT_A,
                    "PRN1",
                    "approved",
                    Some(DeviceConnection::Usb),
                    None,
                )
                .await
                .expect("approve the printer");

            let approved = devices
                .fetch(TENANT_A, Some("store-1"), "approved")
                .await
                .expect("read the approved devices");
            assert_eq!(
                approved.len(),
                2,
                "the terminal is approved on creation, with no resolve step of its own"
            );
            let terminal = approved
                .iter()
                .find(|row| row.id == "TILL1")
                .expect("the terminal");
            assert_eq!(terminal.kind, "terminal");
            assert_eq!(terminal.address, "", "nothing dials a terminal");
            assert_eq!(
                terminal.connection, None,
                "a terminal is not attached to the edge at all"
            );
            let printer = approved
                .iter()
                .find(|row| row.id == "PRN1")
                .expect("the printer");
            assert_eq!(
                printer.agent_device_id, None,
                "absent means the edge opens the address itself"
            );

            // A pick against the version the row is at succeeds and moves it on.
            let moved = devices
                .set_agent(TENANT_A, "PRN1", Some("TILL1"), &printer.version)
                .await
                .expect("the conditional pick")
                .expect("a matching version writes");
            assert_ne!(
                moved, printer.version,
                "the write moves the row to a new version"
            );

            // The same token replayed is stale, and the row is unchanged.
            assert!(
                devices
                    .set_agent(TENANT_A, "PRN1", None, &printer.version)
                    .await
                    .expect("the stale pick")
                    .is_none(),
                "a caller holding the old version does not silently overwrite the new one"
            );
            assert_eq!(
                devices
                    .version_of(TENANT_A, "PRN1")
                    .await
                    .expect("the current version"),
                Some(moved.clone()),
                "and the row still holds what the first pick left"
            );
            let after = devices
                .fetch(TENANT_A, Some("store-1"), "approved")
                .await
                .expect("re-read");
            assert_eq!(
                after
                    .iter()
                    .find(|row| row.id == "PRN1")
                    .and_then(|row| row.agent_device_id.clone()),
                Some("TILL1".to_owned()),
                "the agent pointer round-trips through the column the publish reads"
            );
        });
    }

    /// Only an **approved** device in **this** tenant can be pointed at an agent (ADR-0112).
    ///
    /// Two scopes on one statement, split out from the round trip above so each failure names
    /// itself: a pending proposal has no place to print from yet, and one tenant must not reach
    /// another's devices — the isolation every statement in this table carries.
    #[test]
    fn an_agent_pick_reaches_only_this_tenant_s_approved_devices() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let devices = store.device_proposals();

            devices
                .create_terminal("TILL1", TENANT_A, "store-1", "Till 1")
                .await
                .expect("create the terminal");
            devices
                .create(
                    "PRN1",
                    TENANT_A,
                    "store-1",
                    "printer",
                    "Counter",
                    "/dev/usb/lp0",
                )
                .await
                .expect("propose the printer");
            assert!(
                devices
                    .version_of(TENANT_A, "PRN1")
                    .await
                    .expect("a pending row's version")
                    .is_none(),
                "a pending proposal is not addressable yet: it has no version to write against"
            );

            devices
                .mark(
                    TENANT_A,
                    "PRN1",
                    "approved",
                    Some(DeviceConnection::Usb),
                    None,
                )
                .await
                .expect("approve the printer");
            let version = devices
                .version_of(TENANT_A, "PRN1")
                .await
                .expect("the approved row's version")
                .expect("an approved row has one");
            assert!(
                devices
                    .set_agent("tenant-b", "PRN1", Some("TILL1"), &version)
                    .await
                    .expect("cross-tenant pick")
                    .is_none(),
                "the tenant scope stops one tenant picking another's print agent"
            );
        });
    }

    /// A printer's cash drawer is marked, cleared and read back through the column the publish reads
    /// (ADR-0165 decision 4), under the same conditional write as the agent pick.
    ///
    /// The default is the claim a fleet upgrade rests on: a row nobody marked reads `false`, so no
    /// store's drawer starts opening because the column appeared.
    #[test]
    fn a_drawer_mark_round_trips_and_a_row_nobody_marked_has_none() {
        async fn read(
            devices: &store_postgres::PostgresDeviceProposals,
        ) -> store_postgres::DeviceProposalRow {
            devices
                .fetch(TENANT_A, Some("store-1"), "approved")
                .await
                .expect("read the approved devices")
                .into_iter()
                .find(|row| row.id == "PRN1")
                .expect("the printer")
        }

        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let devices = store.device_proposals();
            devices
                .create(
                    "PRN1",
                    TENANT_A,
                    "store-1",
                    "printer",
                    "Counter",
                    "/dev/usb/lp0",
                )
                .await
                .expect("propose the printer");
            devices
                .mark(
                    TENANT_A,
                    "PRN1",
                    "approved",
                    Some(DeviceConnection::Usb),
                    None,
                )
                .await
                .expect("approve the printer");
            let printer = read(&devices).await;
            assert!(
                !printer.drawer_attached,
                "a printer nobody marked has no drawer"
            );

            let moved = devices
                .set_drawer(TENANT_A, "PRN1", true, &printer.version)
                .await
                .expect("the conditional mark")
                .expect("a matching version writes");
            assert!(read(&devices).await.drawer_attached, "the mark round-trips");
            assert!(
                devices
                    .set_drawer(TENANT_A, "PRN1", false, &printer.version)
                    .await
                    .expect("the stale clear")
                    .is_none(),
                "a caller holding the old version does not silently undo the mark"
            );
            assert!(
                devices
                    .set_drawer("tenant-b", "PRN1", false, &moved)
                    .await
                    .expect("cross-tenant clear")
                    .is_none(),
                "the tenant scope stops one tenant clearing another's drawer"
            );
            devices
                .set_drawer(TENANT_A, "PRN1", false, &moved)
                .await
                .expect("the clear")
                .expect("the current version writes");
            assert!(
                !read(&devices).await.drawer_attached,
                "and the clear does too"
            );
        });
    }

    /// A printer's paper and cutter are written and read back through the columns the publish reads
    /// (ADR-0160 decision 2), under the drawer mark's conditional write.
    ///
    /// The default is the claim a fleet upgrade rests on: a row nobody set reads neither, which is
    /// published as nothing, so no store's receipts change because the columns appeared.
    #[test]
    fn a_printers_paper_round_trips_as_its_token_and_a_row_nobody_set_has_none() {
        async fn read(
            devices: &store_postgres::PostgresDeviceProposals,
        ) -> store_postgres::DeviceProposalRow {
            devices
                .fetch(TENANT_A, Some("store-1"), "approved")
                .await
                .expect("read the approved devices")
                .into_iter()
                .find(|row| row.id == "PRN1")
                .expect("the printer")
        }

        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let devices = store.device_proposals();
            devices
                .create(
                    "PRN1",
                    TENANT_A,
                    "store-1",
                    "printer",
                    "Counter",
                    "192.0.2.10:9100",
                )
                .await
                .expect("propose the printer");
            devices
                .mark(
                    TENANT_A,
                    "PRN1",
                    "approved",
                    Some(DeviceConnection::Network),
                    None,
                )
                .await
                .expect("approve the printer");
            let printer = read(&devices).await;
            assert_eq!(
                (printer.paper_width.as_deref(), printer.cuts_paper),
                (None, None),
                "a printer nobody set says nothing about its paper"
            );

            let moved = devices
                .set_paper(
                    TENANT_A,
                    "PRN1",
                    PaperWidth::Millimetres58,
                    false,
                    &printer.version,
                )
                .await
                .expect("the conditional write")
                .expect("a matching version writes");
            let written = read(&devices).await;
            assert_eq!(
                (written.paper_width.as_deref(), written.cuts_paper),
                (Some("PAPER_WIDTH_MILLIMETRES_58"), Some(false)),
                "the paper is stored as the token the node carries"
            );
            assert!(
                devices
                    .set_paper(
                        TENANT_A,
                        "PRN1",
                        PaperWidth::Millimetres80,
                        true,
                        &printer.version
                    )
                    .await
                    .expect("the stale write")
                    .is_none(),
                "a caller holding the old version does not silently undo the paper"
            );
            assert!(
                devices
                    .set_paper("tenant-b", "PRN1", PaperWidth::Millimetres80, true, &moved)
                    .await
                    .expect("cross-tenant write")
                    .is_none(),
                "the tenant scope stops one tenant setting another's paper"
            );
        });
    }

    /// A till's receipt printer and languages are written and read back through the columns the
    /// publish reads (ADR-0160 decision 4), under the paper's conditional write, and cleared with
    /// nulls.
    ///
    /// The default is the claim a fleet upgrade rests on: a terminal nobody set reads none of the
    /// three, which is published as nothing, so no till's receipts change because the columns
    /// appeared.
    #[test]
    fn a_tills_receipts_round_trip_as_tokens_and_a_terminal_nobody_set_has_none() {
        async fn read(
            devices: &store_postgres::PostgresDeviceProposals,
        ) -> store_postgres::DeviceProposalRow {
            devices
                .fetch(TENANT_A, Some("store-1"), "approved")
                .await
                .expect("read the approved devices")
                .into_iter()
                .find(|row| row.id == "TILL1")
                .expect("the terminal")
        }
        fn receipts(row: &store_postgres::DeviceProposalRow) -> [Option<&str>; 3] {
            [
                row.receipt_printer_id.as_deref(),
                row.receipt_language.as_deref(),
                row.receipt_second_language.as_deref(),
            ]
        }

        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let devices = store.device_proposals();
            devices
                .create_terminal("TILL1", TENANT_A, "store-1", "Bar till")
                .await
                .expect("create the terminal");
            let till = read(&devices).await;
            assert_eq!(
                receipts(&till),
                [None, None, None],
                "a terminal nobody set says nothing about its receipts"
            );

            let moved = devices
                .set_receipt(
                    TENANT_A,
                    "TILL1",
                    Some("PRN2"),
                    Some(ReceiptLanguage::English),
                    Some(ReceiptSecondLanguage::None),
                    &till.version,
                )
                .await
                .expect("the conditional write")
                .expect("a matching version writes");
            assert_eq!(
                receipts(&read(&devices).await),
                [
                    Some("PRN2"),
                    Some("RECEIPT_LANGUAGE_EN"),
                    Some("RECEIPT_SECOND_LANGUAGE_NONE")
                ],
                "each language is stored as the token the node carries"
            );
            assert!(
                devices
                    .set_receipt(TENANT_A, "TILL1", None, None, None, &till.version)
                    .await
                    .expect("the stale write")
                    .is_none(),
                "a caller holding the old version does not silently undo the receipts"
            );
            assert!(
                devices
                    .set_receipt("tenant-b", "TILL1", None, None, None, &moved)
                    .await
                    .expect("cross-tenant write")
                    .is_none(),
                "the tenant scope stops one tenant setting another's receipts"
            );
            devices
                .set_receipt(TENANT_A, "TILL1", None, None, None, &moved)
                .await
                .expect("the clearing write")
                .expect("a matching version clears");
            assert_eq!(
                receipts(&read(&devices).await),
                [None, None, None],
                "a till cleared is the store's again"
            );
        });
    }

    /// A till's own float is written and read back through the column migration 0085 adds, under
    /// the receipts' conditional write, kept across the next boot, and cleared with a null
    /// (ADR-0167 decision 4).
    ///
    /// The default is the claim a fleet upgrade rests on: a terminal nobody set reads no float,
    /// which is published as nothing, so no till's drawer opens differently because the column
    /// appeared.
    #[test]
    fn a_tills_float_round_trips_through_the_next_boot_and_a_terminal_nobody_set_has_none() {
        async fn float(devices: &store_postgres::PostgresDeviceProposals) -> (Option<i64>, String) {
            let till = devices
                .fetch(TENANT_A, Some("store-1"), "approved")
                .await
                .expect("read the approved devices")
                .into_iter()
                .find(|row| row.id == "TILL1")
                .expect("the terminal");
            (till.opening_float_minor, till.version)
        }

        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let devices = store.device_proposals();
            devices
                .create_terminal("TILL1", TENANT_A, "store-1", "Bar till")
                .await
                .expect("create the terminal");
            let (unset, version) = float(&devices).await;
            assert_eq!(unset, None, "a terminal nobody set has no float of its own");

            let moved = devices
                .set_float(TENANT_A, "TILL1", Some(250_000), &version)
                .await
                .expect("the conditional write")
                .expect("a matching version writes");
            assert!(
                devices
                    .set_float(TENANT_A, "TILL1", None, &version)
                    .await
                    .expect("the stale write")
                    .is_none(),
                "a caller holding the old version does not silently undo the float"
            );
            assert!(
                devices
                    .set_float("tenant-b", "TILL1", None, &moved)
                    .await
                    .expect("cross-tenant write")
                    .is_none(),
                "the tenant scope stops one tenant setting another's float"
            );

            store.migrate().await.expect("the next boot");
            let (kept, version) = float(&devices).await;
            assert_eq!(
                kept,
                Some(250_000),
                "the next boot applies 0085 again and keeps what was written"
            );
            devices
                .set_float(TENANT_A, "TILL1", Some(0), &version)
                .await
                .expect("the zero write")
                .expect("a matching version writes");
            let (zero, version) = float(&devices).await;
            assert_eq!(
                zero,
                Some(0),
                "a drawer started empty is a float, not the store's"
            );
            devices
                .set_float(TENANT_A, "TILL1", None, &version)
                .await
                .expect("the clearing write")
                .expect("a matching version clears");
            assert_eq!(
                float(&devices).await.0,
                None,
                "a till cleared is the store's again"
            );
        });
    }

    /// Migration 0055 repairs the rows a two-spelling seam already wrote.
    ///
    /// Typing the argument stops the mistake being made again; it does nothing for the approvals
    /// already in a production database. Those rows carry the config node's `DEVICE_CONNECTION_USB`
    /// where the column's contract is `usb`, and until they are repaired every one of them still
    /// publishes a token no build can read — a USB printer arriving at its store as a network
    /// device, cash drawer disabled. So the repair runs at boot with the rest of the schema, and
    /// this proves it on a row written the old way.
    #[test]
    fn the_migration_repairs_a_connection_written_in_the_node_s_spelling() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");

            // A row exactly as the old seam left it, alongside one already spelled correctly and one
            // carrying a value that is neither — the last is the case the repair must not touch.
            admin
                .batch_execute(
                    "INSERT INTO device_proposals                        (id, tenant_id, store_id, kind, name, address, status, connection) VALUES                        ('OLD1', 'tenant-a', 'store-1', 'printer', 'Counter', '/dev/usb/lp0',                         'approved', 'DEVICE_CONNECTION_USB'),                        ('OK1', 'tenant-a', 'store-1', 'printer', 'Bar', '10.0.0.9:9100',                         'approved', 'network'),                        ('ODD1', 'tenant-a', 'store-1', 'printer', 'Label', '10.0.0.8:9100',                         'approved', 'infrared')",
                )
                .await
                .expect("seed rows written under the old behaviour");

            // Re-running the schema is what a boot does; the repair is idempotent by its own WHERE.
            store.migrate().await.map_err(port_err)?;
            store.migrate().await.map_err(port_err)?;

            let devices = store.device_proposals();
            let rows = devices
                .fetch(TENANT_A, Some("store-1"), "approved")
                .await
                .expect("read the repaired rows");
            let spelling = |id: &str| {
                rows.iter()
                    .find(|row| row.id == id)
                    .and_then(|row| row.connection.clone())
            };
            assert_eq!(
                spelling("OLD1").as_deref(),
                Some("usb"),
                "the node's spelling is repaired to the column's"
            );
            assert_eq!(
                spelling("OK1").as_deref(),
                Some("network"),
                "a row already correct is left alone"
            );
            assert_eq!(
                spelling("ODD1").as_deref(),
                Some("infrared"),
                "a value that is neither spelling is left exactly as it is, not guessed at"
            );
            Ok::<(), pos_contract_tests::harness::HarnessError>(())
        })
        .expect("the repair runs");
    }
}

// ---------------------------------------------------------------------------
// Inventory authoring (ADR-0079): ingredients, recipes, and suppliers as
// per-(tenant, kind, id) jsonb rows, round-tripped and tenant-scoped.
// ---------------------------------------------------------------------------

mod inventory_authoring {
    use super::{TENANT_A, TENANT_B, block_on, prepared};

    use pos_proto::enums::UnitOfMeasure;
    use pos_proto::ids::{IngredientId, MenuItemId, SupplierId};
    use pos_proto::inventory::{
        PublishedIngredient, PublishedRecipe, PublishedRecipeLine, PublishedSupplier,
    };
    use pos_proto::quantity::Quantity;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;
    use store_postgres::{PostgresInventory, RowUpdate};

    fn ingredient(n: u128, name: &str) -> PublishedIngredient {
        PublishedIngredient {
            id: IngredientId::new(Ulid::from_u128(n)),
            name: DisplayName::new(name),
            unit: Open::from_known(UnitOfMeasure::Gram),
        }
    }

    /// Insert a wire record (already a JSON value) under `(tenant, kind, id)`, asserting the key was
    /// free and handing back the version the row starts at — collapses the repeated
    /// serialize-then-insert at every call site. Takes a `serde_json::Value` so the helper needs no
    /// `serde::Serialize` bound (the test crate depends only on `serde_json`).
    async fn put(
        inventory: &PostgresInventory,
        tenant: &str,
        kind: &str,
        id: &str,
        record: serde_json::Value,
    ) -> String {
        inventory
            .insert(tenant, kind, id, &record.to_string())
            .await
            .expect("insert")
            .expect("the key was free")
    }

    async fn count(inventory: &PostgresInventory, tenant: &str, kind: &str) -> usize {
        inventory.fetch(tenant, kind).await.expect("fetch").len()
    }

    /// The single-row read is scoped by tenant *and* kind, and a miss is `None`, not an error.
    ///
    /// The tenant scope is the one that matters: `(tenant_id, kind, entity_id)` is the primary key,
    /// so a neighbour's record with the same id is a different row — but only because the query
    /// filters on all three. A `fetch_one` that dropped `tenant_id` would still pass the
    /// found-and-decoded assertions and would leak across tenants, so this asserts the neighbour is
    /// *not* found under this tenant, which is the half that catches it.
    ///
    /// The `kind` scope has the same shape: the three record types share one table, and a recipe id
    /// happening to equal an ingredient id must not cross.
    #[test]
    fn the_single_row_read_is_scoped_by_tenant_and_kind_and_a_miss_is_not_an_error() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let inventory = store.inventory();
            let shared = Ulid::from_u128(10).to_string();

            // The same entity id under two tenants, and under two kinds within one tenant.
            put(
                &inventory,
                TENANT_A,
                "ingredient",
                &shared,
                serde_json::to_value(ingredient(10, "Mine")).expect("json"),
            )
            .await;
            put(
                &inventory,
                TENANT_B,
                "ingredient",
                &shared,
                serde_json::to_value(ingredient(10, "Theirs")).expect("json"),
            )
            .await;
            put(
                &inventory,
                TENANT_A,
                "supplier",
                &shared,
                serde_json::to_value(PublishedSupplier {
                    id: SupplierId::new(Ulid::from_u128(10)),
                    name: DisplayName::new("Same id, other kind"),
                })
                .expect("json"),
            )
            .await;

            let mine = inventory
                .fetch_one(TENANT_A, "ingredient", &shared)
                .await
                .expect("read")
                .expect("this tenant has it");
            let decoded: PublishedIngredient =
                serde_json::from_str(&mine.doc_json).expect("decode");
            assert_eq!(
                decoded.name.as_str(),
                "Mine",
                "the tenant filter picks this tenant's row, not the neighbour's",
            );
            assert_eq!(mine.entity_id, shared);
            assert!(
                !mine.version.is_empty(),
                "the single-row read carries the version, so a caller can hand it back to update_at",
            );

            let theirs = inventory
                .fetch_one(TENANT_B, "ingredient", &shared)
                .await
                .expect("read")
                .expect("the neighbour has its own");
            let decoded: PublishedIngredient =
                serde_json::from_str(&theirs.doc_json).expect("decode");
            assert_eq!(decoded.name.as_str(), "Theirs");

            let other_kind = inventory
                .fetch_one(TENANT_A, "supplier", &shared)
                .await
                .expect("read")
                .expect("the same id under another kind is another row");
            let decoded: PublishedSupplier =
                serde_json::from_str(&other_kind.doc_json).expect("decode");
            assert_eq!(decoded.name.as_str(), "Same id, other kind");

            // Two ways to miss: an id nobody holds, and a kind this tenant has nothing of. Both are
            // `None` — the routes turn that into a 404, and an error would become a 503.
            assert!(
                inventory
                    .fetch_one(TENANT_A, "ingredient", &Ulid::from_u128(404).to_string())
                    .await
                    .expect("a miss reads cleanly")
                    .is_none()
            );
            assert!(
                inventory
                    .fetch_one(TENANT_A, "recipe", &shared)
                    .await
                    .expect("a miss reads cleanly")
                    .is_none()
            );
        });
    }

    /// The single-row read and the list agree about a row: same id, same document, same version.
    ///
    /// This is what let the nine `/admin/inventory` handlers stop scanning the list. If the two
    /// reads disagreed on any of the three, swapping one for the other would have changed what the
    /// routes answer — and the column order is the way they would: both reads name
    /// `INVENTORY_COLUMNS` and share one row reader, and this is the assertion that says so.
    #[test]
    fn the_single_row_read_returns_exactly_what_the_list_holds_for_that_row() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let inventory = store.inventory();
            for n in 1..=3_u128 {
                put(
                    &inventory,
                    TENANT_A,
                    "ingredient",
                    &Ulid::from_u128(n).to_string(),
                    serde_json::to_value(ingredient(n, &format!("Ingredient {n}"))).expect("json"),
                )
                .await;
            }

            let listed = inventory
                .fetch(TENANT_A, "ingredient")
                .await
                .expect("the list");
            assert_eq!(listed.len(), 3);
            for row in listed {
                let one = inventory
                    .fetch_one(TENANT_A, "ingredient", &row.entity_id)
                    .await
                    .expect("the single-row read")
                    .expect("the list said it is there");
                assert_eq!(
                    one, row,
                    "the two reads must agree on the whole row, columns included",
                );
            }
        });
    }

    #[test]
    fn ingredients_recipes_suppliers_round_trip_scoped_by_tenant_and_kind() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let inventory = store.inventory();
            let ing_id = Ulid::from_u128(10).to_string();

            // A neighbour tenant's ingredient must not leak into TENANT_A's reads.
            put(
                &inventory,
                TENANT_B,
                "ingredient",
                &Ulid::from_u128(99).to_string(),
                serde_json::to_value(ingredient(99, "Neighbour")).expect("json"),
            )
            .await;

            // Create one ingredient, then rename it through the update at the version the insert
            // minted. The insert/update split itself is asserted below, in its own test.
            let at_create = put(
                &inventory,
                TENANT_A,
                "ingredient",
                &ing_id,
                serde_json::to_value(ingredient(10, "Dough")).expect("json"),
            )
            .await;
            let renamed = serde_json::to_value(ingredient(10, "Dough (renamed)"))
                .expect("json")
                .to_string();
            assert!(matches!(
                inventory
                    .update_at(TENANT_A, "ingredient", &ing_id, &renamed, &at_create)
                    .await
                    .expect("the rename"),
                RowUpdate::Updated(_)
            ));

            // A recipe and a supplier share the table under other kinds.
            let recipe = PublishedRecipe {
                item: MenuItemId::new(Ulid::from_u128(20)),
                lines: vec![PublishedRecipeLine {
                    ingredient: IngredientId::new(Ulid::from_u128(10)),
                    per_unit: Quantity::from_milli(100_000),
                }],
                auto_86_threshold: 3,
            };
            put(
                &inventory,
                TENANT_A,
                "recipe",
                &recipe.item.to_string(),
                serde_json::to_value(&recipe).expect("json"),
            )
            .await;
            let supplier = PublishedSupplier {
                id: SupplierId::new(Ulid::from_u128(30)),
                name: DisplayName::new("Anchor Dairy"),
            };
            put(
                &inventory,
                TENANT_A,
                "supplier",
                &supplier.id.to_string(),
                serde_json::to_value(&supplier).expect("json"),
            )
            .await;

            // The kind filter keeps the three apart, and the rename replaced in place.
            let ingredients = inventory.fetch(TENANT_A, "ingredient").await.expect("ings");
            assert_eq!(ingredients.len(), 1, "an update adds no row");
            let row = ingredients.first().expect("one");
            let decoded: PublishedIngredient = serde_json::from_str(&row.doc_json).expect("decode");
            assert_eq!(decoded.name, DisplayName::new("Dough (renamed)"));
            assert_ne!(
                row.version, at_create,
                "and the read carries the version the update left the row at, not the insert's"
            );
            assert_eq!(count(&inventory, TENANT_A, "recipe").await, 1);
            assert_eq!(count(&inventory, TENANT_A, "supplier").await, 1);

            // Tenant isolation: TENANT_B sees only its own ingredient, none of TENANT_A's rows.
            assert_eq!(count(&inventory, TENANT_B, "ingredient").await, 1);
            assert_eq!(count(&inventory, TENANT_B, "recipe").await, 0);

            // Delete is scoped by (kind, id) and idempotent; other kinds are untouched.
            inventory
                .delete(TENANT_A, "ingredient", &ing_id)
                .await
                .expect("delete");
            inventory
                .delete(TENANT_A, "ingredient", &ing_id)
                .await
                .expect("no-op");
            assert_eq!(count(&inventory, TENANT_A, "ingredient").await, 0);
            assert_eq!(count(&inventory, TENANT_A, "recipe").await, 1);
            assert_eq!(count(&inventory, TENANT_A, "supplier").await, 1);
        });
    }

    /// A record's key is `(tenant, kind, entity_id)` and it comes from the caller, so a second
    /// insert at a taken key writes nothing and returns no version — where the `upsert` this
    /// replaced replaced the document (ADR-0095). A stale update is refused, an absent one is
    /// `NotFound`, and the two answers are distinct because the caller's next move differs.
    #[test]
    fn an_insert_writes_nothing_when_the_key_is_taken_and_an_update_needs_the_version_read() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let inventory = store.inventory();
            let ing_id = Ulid::from_u128(10).to_string();
            let renamed = serde_json::to_value(ingredient(10, "Dough (renamed)"))
                .expect("json")
                .to_string();

            let at_create = put(
                &inventory,
                TENANT_A,
                "ingredient",
                &ing_id,
                serde_json::to_value(ingredient(10, "Dough")).expect("json"),
            )
            .await;
            assert!(
                inventory
                    .insert(TENANT_A, "ingredient", &ing_id, &renamed)
                    .await
                    .expect("the conflict must not raise")
                    .is_none(),
                "the (kind, id) is taken"
            );
            let row = inventory
                .fetch(TENANT_A, "ingredient")
                .await
                .expect("fetch")
                .pop()
                .expect("one");
            let decoded: PublishedIngredient = serde_json::from_str(&row.doc_json).expect("decode");
            assert_eq!(
                decoded.name,
                DisplayName::new("Dough"),
                "and does not rename the record it refused to overwrite"
            );
            assert_eq!(row.version, at_create);

            assert!(matches!(
                inventory
                    .update_at(TENANT_A, "ingredient", &ing_id, &renamed, &at_create)
                    .await
                    .expect("the rename"),
                RowUpdate::Updated(_)
            ));
            assert_eq!(
                inventory
                    .update_at(TENANT_A, "ingredient", &ing_id, &renamed, &at_create)
                    .await
                    .expect("the comparison must not raise"),
                RowUpdate::VersionMismatch,
                "replaying a spent version is the lost update"
            );
            assert_eq!(
                inventory
                    .update_at(TENANT_A, "ingredient", "none", &renamed, &at_create)
                    .await
                    .expect("the comparison must not raise"),
                RowUpdate::NotFound
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Conditional writes over the `xmin` system column (ADR-0094).
// ---------------------------------------------------------------------------

mod conditional_writes {
    use super::{block_on, prepared};
    use store_postgres::RowUpdate;

    /// The compare-and-swap itself, against the real planner.
    ///
    /// Three things can only be shown here. That `xmin` **moves on every `UPDATE`** — the whole
    /// scheme rests on it, and no fake can prove Postgres does it. That a stale version is refused
    /// as a `VersionMismatch` rather than silently applying. And that a **garbled** tag is a
    /// mismatch too, not a database error: the comparison is on `xmin::text`, because casting
    /// caller-supplied text to `xid` raises `invalid input syntax for type xid` and would turn a
    /// client's stale tag into a `500` instead of the `412` it has earned.
    #[test]
    fn a_stale_or_garbled_version_is_refused_and_a_current_one_moves_it() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let registry = store.registry();
            let tenant = "0000000000TENANTXMINAAAAAA";

            let first = registry
                .insert_tenant(tenant, "Placeholder")
                .await
                .expect("insert the tenant");

            // A garbled tag: not a transaction id at all. It must simply not match.
            assert_eq!(
                registry
                    .set_tenant(tenant, "Nope", "active", "not-a-transaction-id")
                    .await
                    .expect("the comparison must not raise"),
                RowUpdate::VersionMismatch
            );

            // The current version applies, and hands back a different one.
            let second = match registry
                .set_tenant(tenant, "Pizza 4P's", "active", &first)
                .await
                .expect("the update")
            {
                RowUpdate::Updated(version) => version,
                other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                    panic!("expected the update to apply, got {other:?}")
                }
            };
            assert_ne!(
                second, first,
                "xmin must move on every UPDATE, or the next write would be unguarded"
            );

            // Replaying the first version is the lost update, refused.
            assert_eq!(
                registry
                    .set_tenant(tenant, "Stale Overwrite", "archived", &first)
                    .await
                    .expect("the update"),
                RowUpdate::VersionMismatch
            );

            // And the refused write changed nothing.
            let rows = registry.fetch_tenants().await.expect("list the tenants");
            // By id, not by position: the shared truncation between cases does not clear `tenants`.
            let row = rows
                .iter()
                .find(|row| row.tenant_id == tenant)
                .expect("the tenant is there");
            assert_eq!(row.name, "Pizza 4P's");
            assert_eq!(row.status, "active");
            assert_eq!(row.version, second);
        });
    }

    /// The floor family's own SQL, which the registry case cannot cover.
    ///
    /// Two things are specific to these tables. Each `SELECT` had `xmin::text` appended to a
    /// hand-written column list whose mapper reads by **position**, so a column added in the wrong
    /// place would hand back a name where a version belongs — only a real read proves the order.
    /// And `set_station` writes a self-referencing `backup_station_id`, the most intricate of the
    /// three conditional statements.
    #[test]
    fn a_floor_row_carries_the_version_it_was_read_at_and_a_stale_write_is_refused() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let floor = store.floor();
            let tenant = "0000000000TENANTFLOORXMINA";
            let store_id = "00000000000000STOREFLOORXA";
            let area = "0000000000000000AREAXMINAA";
            let station = "000000000000000STATIONXMIN";

            let area_version = floor
                .insert_area(area, tenant, store_id, "Terrace")
                .await
                .expect("insert the area");
            let station_version = floor
                .insert_station(station, tenant, store_id, "Oven", None, true, None, None)
                .await
                .expect("insert the station");

            // The read hands back the same token the insert minted, from the right column.
            let areas = floor
                .fetch_areas(tenant, store_id)
                .await
                .expect("list the areas");
            let row = areas.first().expect("the area is there");
            assert_eq!(
                row.name, "Terrace",
                "the column order survived the addition"
            );
            assert_eq!(row.version, area_version);

            // A stale tag is refused; the current one applies and moves the row.
            assert_eq!(
                floor
                    .set_station(
                        tenant, station, "Nope", None, false, None, None, "active", "1"
                    )
                    .await
                    .expect("the comparison must not raise"),
                RowUpdate::VersionMismatch
            );
            let moved = match floor
                .set_station(
                    tenant,
                    station,
                    "Pizza oven",
                    Some(station),
                    false,
                    None,
                    None,
                    "active",
                    &station_version,
                )
                .await
                .expect("the update")
            {
                RowUpdate::Updated(version) => version,
                other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                    panic!("expected the update to apply, got {other:?}")
                }
            };
            assert_ne!(moved, station_version, "xmin moves on every UPDATE");

            let stations = floor
                .fetch_stations(tenant, store_id)
                .await
                .expect("list the stations");
            let row = stations.first().expect("the station is there");
            assert_eq!(row.name, "Pizza oven");
            assert_eq!(row.backup_station_id.as_deref(), Some(station));
            assert_eq!(row.version, moved);

            // The other half of the same probe, and the half that had no assertion: when the UPDATE
            // matches nothing, the probe decides whether the caller sent a stale version (412) or
            // named a station that does not exist (404). Only the stale branch was covered, so the
            // probe could — and did — query a table that was not there while still looking correct.
            assert_eq!(
                floor
                    .set_station(
                        tenant,
                        "00000000000000NOSUCHSTATIO",
                        "Ghost",
                        None,
                        false,
                        None,
                        None,
                        "active",
                        &moved,
                    )
                    .await
                    .expect("the probe must not raise"),
                RowUpdate::NotFound
            );
        });
    }

    /// A station's late threshold (migration 0080, ADR-0160 decision 2) is written on create and on
    /// update and read back from the column the mapper expects; a station nobody has set reads
    /// `None`, which the cloud publishes as nothing, and an update that clears it writes null again.
    #[test]
    fn a_stations_late_threshold_round_trips_and_a_row_nobody_set_has_none() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let floor = store.floor();
            let tenant = "0000000000TENANTSTATIONLATE";
            let store_id = "000000000000STORESTATIONLA";
            let oven = "0000000000000STATIONOVENLA";
            let bar = "00000000000000STATIONBARLA";

            let oven_version = floor
                .insert_station(oven, tenant, store_id, "Oven", None, true, Some(240), None)
                .await
                .expect("insert the oven");
            floor
                .insert_station(bar, tenant, store_id, "Bar", None, false, None, None)
                .await
                .expect("insert the bar");
            let read = |rows: &[store_postgres::StationRow], id: &str| {
                rows.iter()
                    .find(|row| row.id == id)
                    .map(|row| (row.name.clone(), row.late_after_seconds))
                    .expect("the station is listed")
            };
            let rows = floor
                .fetch_stations(tenant, store_id)
                .await
                .expect("list the stations");
            assert_eq!(read(&rows, oven), ("Oven".to_owned(), Some(240)));
            assert_eq!(read(&rows, bar), ("Bar".to_owned(), None));

            let moved = match floor
                .set_station(
                    tenant,
                    oven,
                    "Oven",
                    None,
                    true,
                    Some(900),
                    None,
                    "active",
                    &oven_version,
                )
                .await
                .expect("the update")
            {
                RowUpdate::Updated(version) => version,
                other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                    panic!("expected the update to apply, got {other:?}")
                }
            };
            let row = floor
                .fetch_station(tenant, oven)
                .await
                .expect("read the oven")
                .expect("the oven is there");
            assert_eq!(row.late_after_seconds, Some(900));
            assert_eq!(
                row.version, moved,
                "the version still comes from its own column"
            );

            floor
                .set_station(
                    tenant, oven, "Oven", None, true, None, None, "active", &moved,
                )
                .await
                .expect("the update");
            let row = floor
                .fetch_station(tenant, oven)
                .await
                .expect("read the oven")
                .expect("the oven is there");
            assert_eq!(
                row.late_after_seconds, None,
                "cleared, it is nobody's again"
            );
        });
    }

    /// A station's ticket language (migration 0081, ADR-0160 decision 2) is written on create and on
    /// update as the token the cloud validated, and read back from the column the mapper expects; a
    /// station nobody has set reads `None`, which the cloud publishes as nothing, and an update that
    /// clears it writes null again.
    #[test]
    fn a_stations_ticket_language_round_trips_and_a_row_nobody_set_has_none() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let floor = store.floor();
            let tenant = "000000000TENANTSTATIONLANG";
            let store_id = "000000000000STORESTATIONLG";
            let oven = "0000000000000STATIONOVENLG";
            let bar = "00000000000000STATIONBARLG";
            let english = Some("RECEIPT_LANGUAGE_EN");

            let oven_version = floor
                .insert_station(oven, tenant, store_id, "Oven", None, true, None, english)
                .await
                .expect("insert the oven");
            floor
                .insert_station(bar, tenant, store_id, "Bar", None, false, Some(300), None)
                .await
                .expect("insert the bar");
            let rows = floor
                .fetch_stations(tenant, store_id)
                .await
                .expect("list the stations");
            let read = |id: &str| {
                rows.iter()
                    .find(|row| row.id == id)
                    .map(|row| (row.ticket_language.clone(), row.late_after_seconds))
                    .expect("the station is listed")
            };
            assert_eq!(read(oven), (english.map(str::to_owned), None));
            assert_eq!(read(bar), (None, Some(300)), "each column in its own place");

            let vietnamese = Some("RECEIPT_LANGUAGE_VI");
            let moved = match floor
                .set_station(
                    tenant,
                    oven,
                    "Oven",
                    None,
                    true,
                    None,
                    vietnamese,
                    "active",
                    &oven_version,
                )
                .await
                .expect("the update")
            {
                RowUpdate::Updated(version) => version,
                other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                    panic!("expected the update to apply, got {other:?}")
                }
            };
            let row = floor
                .fetch_station(tenant, oven)
                .await
                .expect("read the oven")
                .expect("the oven is there");
            assert_eq!(row.ticket_language.as_deref(), vietnamese);
            assert_eq!(
                row.version, moved,
                "the version still comes from its own column"
            );

            floor
                .set_station(
                    tenant, oven, "Oven", None, true, None, None, "active", &moved,
                )
                .await
                .expect("the update");
            let row = floor
                .fetch_station(tenant, oven)
                .await
                .expect("read the oven")
                .expect("the oven is there");
            assert_eq!(row.ticket_language, None, "cleared, it is nobody's again");
        });
    }

    /// The people family's own SQL. `role_templates` stores its permissions as `jsonb`, so its
    /// column list is the one most likely to have taken `xmin::text` in the wrong position.
    #[test]
    fn a_people_row_carries_the_version_it_was_read_at_and_a_stale_write_is_refused() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let people = store.people();
            let tenant = "000000000TENANTPEOPLEXMINA";
            let role = "00000000000000000ROLEXMINA";

            let first = people
                .insert_role_template(
                    role,
                    tenant,
                    "Cashier",
                    "[\"sales.item.open\"]",
                    "[\"billing.bill.void\"]",
                    None,
                )
                .await
                .expect("insert the role template");

            let rows = people
                .fetch_role_templates(tenant)
                .await
                .expect("list the role templates");
            let row = rows.first().expect("the role is there");
            assert_eq!(
                row.name, "Cashier",
                "the column order survived the addition"
            );
            assert_eq!(row.permissions_json, "[\"sales.item.open\"]");
            assert_eq!(
                row.permissions_with_approval_json,
                "[\"billing.bill.void\"]"
            );
            assert_eq!(row.version, first);

            assert_eq!(
                people
                    .set_role_template(
                        tenant,
                        role,
                        "Nope",
                        "[]",
                        "[]",
                        "active",
                        None,
                        "not-a-transaction-id",
                    )
                    .await
                    .expect("the comparison must not raise"),
                RowUpdate::VersionMismatch
            );
            let second = match people
                .set_role_template(
                    tenant, role, "Cashier", "[]", "[]", "archived", None, &first,
                )
                .await
                .expect("the update")
            {
                RowUpdate::Updated(version) => version,
                other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                    panic!("expected the update to apply, got {other:?}")
                }
            };
            assert_ne!(second, first, "xmin moves on every UPDATE");

            let rows = people
                .fetch_role_templates(tenant)
                .await
                .expect("list the role templates");
            let row = rows.first().expect("the role is there");
            assert_eq!(row.status, "archived");
            assert_eq!(row.version, second);
        });
    }

    /// An absent row is `NotFound`, not `VersionMismatch` — the probe on the failure path is what
    /// separates a `404` from a `412`, and zero rows alone cannot.
    #[test]
    fn an_absent_row_is_not_found_rather_than_a_version_mismatch() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let registry = store.registry();
            assert_eq!(
                registry
                    .set_tenant("0000000000TENANTGONEAAAAAA", "Nope", "active", "1")
                    .await
                    .expect("the update"),
                RowUpdate::NotFound
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Catalog placements and layout buttons: the two caller-keyed rows, on the insert/update-at split
// (ADR-0095). Both were single upserts, and both have their whole write path here for the first
// time — the class of defect #152 fixed (a presence probe naming the wrong table) is invisible to
// every test that does not run the SQL.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The item master, paged: window, total, order and the index that carries it (ADR-0098).
// ---------------------------------------------------------------------------

mod catalog_item_pages {
    use core::fmt::Write as _;

    use store_postgres::{ItemOrder, PostgresCatalog};

    use super::{TENANT_A, TENANT_B, block_on, prepared};

    /// Inserts `count` items for one tenant. The id carries the tenant because `menu_item_id` is the
    /// primary key across every tenant: two calls with the same index range would collide.
    async fn stock(catalog: &PostgresCatalog, tenant: &str, count: u32) {
        for index in 0..count {
            catalog
                .insert_item(
                    &format!("{tenant}-item-{index:04}"),
                    tenant,
                    &format!("Item {index:04}"),
                    "{}",
                    "tax-standard",
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .expect("insert an item");
        }
    }

    /// An item's per-locale names survive a create and an update against real PostgreSQL.
    ///
    /// This is a regression guard for a bug these paging tests found, not a paging test. Both writes
    /// bind `name_translations` as text into a `jsonb` column, and they cast it `$N::text::jsonb`
    /// rather than `$N::jsonb`. The difference is not cosmetic: PostgreSQL infers a bare `$N::jsonb`
    /// parameter *as* `jsonb`, and `tokio-postgres` then refuses to send a Rust `&str` for it — so
    /// both statements failed at the driver, and every item create and rename answered `503`.
    ///
    /// It shipped because nothing in this suite had ever called `insert_item`: the console tests run
    /// against the fake, which has no parameter types to get wrong, and this file only exercised
    /// placements. Every other `jsonb` parameter in the tree already carried the double cast; these
    /// two, added with the per-locale names column (migration 0029), did not.
    #[test]
    fn an_items_per_locale_names_round_trip_through_a_create_and_an_update() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();

            let version = catalog
                .insert_item(
                    "item-jsonb",
                    TENANT_A,
                    "Margherita",
                    r#"{"vi":"Bánh Margherita"}"#,
                    "tax-standard",
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .expect("a create must reach the database, not fail at the driver");

            let stored = catalog.fetch_items(TENANT_A).await.expect("read back");
            let row = stored.first().expect("the item");
            assert_eq!(row.name, "Margherita");
            assert!(
                row.name_translations.contains("Bánh Margherita"),
                "the per-locale name round-trips through the jsonb column; got {:?}",
                row.name_translations,
            );

            let outcome = catalog
                .set_item(
                    TENANT_A,
                    "item-jsonb",
                    "Margherita Classic",
                    r#"{"vi":"Bánh Margherita cổ điển"}"#,
                    "tax-standard",
                    None,
                    None,
                    None,
                    None,
                    "active",
                    &version,
                )
                .await
                .expect("an update must reach the database too");
            assert!(
                matches!(outcome, store_postgres::RowUpdate::Updated(_)),
                "the update applies at the version the create returned",
            );

            let stored = catalog
                .fetch_items(TENANT_A)
                .await
                .expect("read back again");
            let row = stored.first().expect("the item");
            assert_eq!(row.name, "Margherita Classic");
            assert!(row.name_translations.contains("cổ điển"));
        });
    }

    /// A page carries its own window and the size of the whole item master, and consecutive pages
    /// partition it without overlap or gaps (ADR-0098 slice B3-2).
    #[test]
    fn a_page_of_the_item_master_carries_the_window_and_the_total() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();
            stock(&catalog, TENANT_A, 25).await;
            // A neighbour's items must reach neither the page nor the count.
            stock(&catalog, TENANT_B, 7).await;

            let (first, total) = catalog
                .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 10, 0)
                .await
                .expect("first page");
            assert_eq!(first.len(), 10, "the window is the limit");
            assert_eq!(total, 25, "the total is the master, not the window");

            let mut seen: Vec<String> = first.into_iter().map(|row| row.menu_item_id).collect();
            for offset in [10, 20] {
                let (page, page_total) = catalog
                    .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 10, offset)
                    .await
                    .expect("later page");
                assert_eq!(page_total, 25, "the total does not change as pages advance");
                seen.extend(page.into_iter().map(|row| row.menu_item_id));
            }
            assert_eq!(seen.len(), 25, "three pages of ten cover twenty-five items");
            let mut unique = seen.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), 25, "no item appears on two pages");
            assert!(
                unique.iter().all(|id| id.starts_with(TENANT_A)),
                "no neighbour's item reached the pages",
            );
        });
    }

    /// The paged read returns the same rows in the same order as the whole-set read, and a page past
    /// the end is empty rather than an error.
    #[test]
    fn the_paged_item_master_agrees_with_the_whole_set_read() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();
            stock(&catalog, TENANT_A, 6).await;
            stock(&catalog, TENANT_B, 4).await;

            let whole: Vec<String> = catalog
                .fetch_items(TENANT_A)
                .await
                .expect("whole set")
                .into_iter()
                .map(|row| row.menu_item_id)
                .collect();
            assert_eq!(whole.len(), 6, "only this tenant's items");

            let (paged, total) = catalog
                .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 6, 0)
                .await
                .expect("paged");
            assert_eq!(total, 6, "the count is tenant-scoped too");
            let paged: Vec<String> = paged.into_iter().map(|row| row.menu_item_id).collect();
            assert_eq!(
                paged, whole,
                "a full-width page is the whole-set read, in the same order"
            );

            // A page past the end: empty, but `total` still reports the whole master. The window
            // count rides on the rows and there are none, so the adapter falls back to a second
            // count rather than claiming the tenant has no items.
            let (beyond, beyond_total) = catalog
                .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 10, 100)
                .await
                .expect("a page past the end still reads");
            assert!(beyond.is_empty());
            assert_eq!(
                beyond_total, 6,
                "an empty window still reports the size of the set it is past the end of"
            );
        });
    }

    /// A batch written in one transaction shares one `created_at`, which is why the read needs the
    /// `menu_item_id` tiebreaker — the premise of ADR-0098 decision 9, measured rather than trusted.
    ///
    /// A CSV import of a menu is exactly this shape. The tie assertion is deterministic; the
    /// partition check below it is the property that follows, and the `EXPLAIN` guard is what
    /// actually catches the tiebreaker going missing.
    #[test]
    fn an_imported_batch_shares_one_timestamp_and_still_pages() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();
            let mut inserts = String::new();
            for index in 0..6 {
                write!(
                    inserts,
                    "INSERT INTO catalog_items \
                     (menu_item_id, tenant_id, name, tax_class_id) \
                     VALUES ('import-{index:04}', '{TENANT_A}', 'Imported {index}', 'tax-standard');"
                )
                .expect("writing to a String cannot fail");
            }
            admin
                .batch_execute(&format!("BEGIN; {inserts} COMMIT;"))
                .await
                .expect("write the import in one transaction");

            let distinct: i64 = admin
                .query_one(
                    "SELECT count(DISTINCT created_at) FROM catalog_items WHERE tenant_id = $1",
                    &[&TENANT_A],
                )
                .await
                .expect("count the distinct timestamps")
                .get(0);
            assert_eq!(
                distinct, 1,
                "six items imported in one transaction carry one created_at, not six close ones — \
                 which is why the read's ORDER BY needs menu_item_id as a tiebreaker"
            );

            let mut seen = Vec::new();
            for offset in [0, 2, 4] {
                let (page, total) = catalog
                    .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 2, offset)
                    .await
                    .expect("page over the imported batch");
                assert_eq!(total, 6);
                seen.extend(page.into_iter().map(|row| row.menu_item_id));
            }
            let mut unique = seen.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                unique.len(),
                6,
                "three pages of two cover the batch exactly once each; got {seen:?}"
            );
        });
    }

    /// `?q=` narrows the rows and the total together, and reaches the per-locale names.
    ///
    /// Only real PostgreSQL can answer this: the predicate is `position(lower(..) in lower(..))` over
    /// `name` *and* over every value in a `jsonb` document via `jsonb_each_text`, with the count
    /// riding on the same statement. A `Vec::filter` fake agreeing with itself proves none of it.
    #[test]
    fn a_search_narrows_the_page_and_its_total_including_per_locale_names() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();
            for (id, name, vi) in [
                ("i-1", "Margherita", "Banh Margherita"),
                ("i-2", "Marinara", "Banh Marinara"),
                ("i-3", "Tiramisu", "Banh ngot Tiramisu"),
            ] {
                catalog
                    .insert_item(
                        id,
                        TENANT_A,
                        name,
                        &format!("{{\"vi\":\"{vi}\"}}"),
                        "tax-standard",
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
                    .expect("insert an item");
            }

            // A primary-name substring, case-insensitively.
            let (rows, total) = catalog
                .fetch_items_page(TENANT_A, Some("MARI"), ItemOrder::Newest, false, 10, 0)
                .await
                .expect("search");
            assert_eq!(total, 1, "the total counts the match, not the master");
            assert_eq!(rows.first().expect("a row").name, "Marinara");

            // A per-locale substring appearing in no primary name at all.
            let (rows, total) = catalog
                .fetch_items_page(TENANT_A, Some("ngot"), ItemOrder::Newest, false, 10, 0)
                .await
                .expect("search");
            assert_eq!(total, 1, "the per-locale name is searched too");
            assert_eq!(rows.first().expect("a row").name, "Tiramisu");

            // No match is an empty page and a zero total, not an error — and this zero is the true
            // one. The empty-window fallback runs here too, carrying the same search predicate, so
            // it counts the match (none) rather than the master (six).
            let (rows, total) = catalog
                .fetch_items_page(TENANT_A, Some("carbonara"), ItemOrder::Newest, false, 10, 0)
                .await
                .expect("search");
            assert!(rows.is_empty());
            assert_eq!(
                total, 0,
                "the fallback count is filtered, not the whole master"
            );

            // `%` is a character in the needle, not a wildcard: the read uses `position`, not
            // `ILIKE`, so an operator searching for a literal percent gets a literal search.
            let (rows, _total) = catalog
                .fetch_items_page(TENANT_A, Some("%"), ItemOrder::Newest, false, 10, 0)
                .await
                .expect("search");
            assert!(
                rows.is_empty(),
                "a percent sign matches nothing here; under ILIKE it would match everything",
            );
        });
    }

    /// Each order the route offers is total in SQL, so pages partition the set under every one.
    ///
    /// Every item here shares a name, so `?sort=name` rests entirely on the tiebreaker — the case
    /// that would silently repeat or skip a row if the `ORDER BY` stopped at `name`.
    #[test]
    fn every_offered_order_partitions_the_master_in_both_directions() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();
            for index in 0..6 {
                catalog
                    .insert_item(
                        &format!("same-{index:04}"),
                        TENANT_A,
                        "Margherita",
                        "{}",
                        "tax-standard",
                        None,
                        None,
                        None,
                        None,
                    )
                    .await
                    .expect("insert an item");
            }

            for order in [ItemOrder::Newest, ItemOrder::Name, ItemOrder::Status] {
                for descending in [false, true] {
                    let mut seen = Vec::new();
                    for offset in [0, 2, 4] {
                        let (page, total) = catalog
                            .fetch_items_page(TENANT_A, None, order, descending, 2, offset)
                            .await
                            .expect("page");
                        assert_eq!(total, 6);
                        seen.extend(page.into_iter().map(|row| row.menu_item_id));
                    }
                    let mut unique = seen.clone();
                    unique.sort_unstable();
                    unique.dedup();
                    assert_eq!(
                        unique.len(),
                        6,
                        "{order:?} descending={descending} covers each item once; got {seen:?}",
                    );
                }
            }
        });
    }

    /// Migration 0044's index carries the *name* order, so `?sort=name` stops a scan rather than
    /// sorting the whole master — the economy 0043 bought for the default order, kept for the second.
    ///
    /// This is the guard that makes offering a second order safe: without it, a query parameter
    /// reintroduces exactly the cost 0043 exists to remove.
    #[test]
    fn the_name_sorted_page_is_served_by_its_own_index() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();
            stock(&catalog, TENANT_A, 40).await;
            admin
                .batch_execute("ANALYZE catalog_items")
                .await
                .expect("analyze");

            let plan = {
                admin
                    .batch_execute("SET enable_seqscan = off")
                    .await
                    .expect("prefer an index if one fits");
                let rows = admin
                    .query(
                        "EXPLAIN SELECT menu_item_id FROM catalog_items WHERE tenant_id = $1 \
                         ORDER BY name ASC, menu_item_id ASC LIMIT $2 OFFSET $3",
                        &[&TENANT_A, &10_i64, &0_i64],
                    )
                    .await
                    .expect("explain");
                admin
                    .batch_execute("RESET enable_seqscan")
                    .await
                    .expect("restore");
                rows.iter()
                    .map(|row| row.get::<_, String>(0))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            assert!(
                plan.contains("catalog_items_by_tenant_name"),
                "the name sort should be served by migration 0044's index; plan was:\n{plan}"
            );
            assert!(
                !plan.contains("Sort Key"),
                "an index that carries the order needs no sort step; plan was:\n{plan}"
            );
        });
    }

    /// Migration 0043's index carries the read's whole order, so `LIMIT` stops the scan instead of
    /// truncating a sort of every item a chain sells.
    #[test]
    fn the_paged_item_query_is_served_by_the_index_and_not_by_a_sort() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();
            stock(&catalog, TENANT_A, 40).await;
            admin
                .batch_execute("ANALYZE catalog_items")
                .await
                .expect("analyze");

            let plan = {
                admin
                    .batch_execute("SET enable_seqscan = off")
                    .await
                    .expect("prefer an index if one fits");
                let rows = admin
                    .query(
                        "EXPLAIN SELECT menu_item_id FROM catalog_items WHERE tenant_id = $1 \
                         ORDER BY created_at DESC, menu_item_id DESC LIMIT $2 OFFSET $3",
                        &[&TENANT_A, &10_i64, &0_i64],
                    )
                    .await
                    .expect("explain");
                admin
                    .batch_execute("RESET enable_seqscan")
                    .await
                    .expect("restore");
                rows.iter()
                    .map(|row| row.get::<_, String>(0))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            assert!(
                plan.contains("catalog_items_by_tenant_newest"),
                "the page should be served by the sort-carrying index; plan was:\n{plan}"
            );
            assert!(
                !plan.contains("Sort Key"),
                "an index that carries the order needs no sort step; plan was:\n{plan}"
            );
        });
    }
}

mod catalog_keyed_rows {
    use store_postgres::{PostgresCatalog, RowUpdate};

    use super::{TENANT_A, TENANT_B, block_on, prepared};

    const MENU: &str = "menu-1";
    const ITEM: &str = "item-1";
    const CHANNEL: &str = "SALES_CHANNEL_DINE_IN";
    const CATEGORY: &str = "cat-1";

    fn prices(amount: i64) -> String {
        format!(
            "[{{\"sales_channel\":\"SALES_CHANNEL_DINE_IN\",\
               \"unit_price\":{{\"currency_code\":\"VND\",\"amount_minor\":{amount}}}}}]"
        )
    }

    /// The version an applied write left the row at, or `None` when it was refused. `Option` rather
    /// than a panic because this is a plain helper, not a `#[test]` fn — the caller unwraps it, where
    /// `clippy.toml`'s test allowances apply.
    fn applied(outcome: RowUpdate) -> Option<String> {
        match outcome {
            RowUpdate::Updated(version) => Some(version),
            RowUpdate::VersionMismatch | RowUpdate::NotFound => None,
        }
    }

    /// Puts an item on a menu at `amount`, answering the version the row starts at (or `None` when
    /// the `(menu, item)` pair is already taken).
    async fn place(
        catalog: &PostgresCatalog,
        tenant: &str,
        menu: &str,
        item: &str,
        amount: i64,
    ) -> Option<String> {
        catalog
            .insert_placement(tenant, menu, item, None, &prices(amount), true)
            .await
            .expect("the insert must not raise")
    }

    /// Reprices that placement, only at `expected`.
    async fn reprice(
        catalog: &PostgresCatalog,
        tenant: &str,
        item: &str,
        amount: i64,
        expected: &str,
    ) -> RowUpdate {
        catalog
            .update_placement_at(tenant, MENU, item, None, &prices(amount), true, expected)
            .await
            .expect("the comparison must not raise")
    }

    /// Places a button in the `(channel, item)` slot, answering the version it starts at (or `None`
    /// when the slot is taken).
    async fn press(
        catalog: &PostgresCatalog,
        tenant: &str,
        item: &str,
        label: &str,
    ) -> Option<String> {
        catalog
            .insert_layout_button(tenant, CHANNEL, CATEGORY, None, item, label, None, None, 0)
            .await
            .expect("the insert must not raise")
    }

    /// Relabels and re-sorts that button, only at `expected`.
    async fn relabel(
        catalog: &PostgresCatalog,
        tenant: &str,
        item: &str,
        label: &str,
        sort: i32,
        expected: &str,
    ) -> RowUpdate {
        catalog
            .update_layout_button_at(
                tenant, CHANNEL, CATEGORY, None, item, label, None, None, sort, expected,
            )
            .await
            .expect("the comparison must not raise")
    }

    /// A placement's identity is the caller's `(menu, item)` pair, so a second insert at the same
    /// pair writes nothing rather than repricing what is already on the menu. The per-channel prices
    /// are the price-change journal (ADR-0069), so that overwrite was recorded as a set with no
    /// `before` to compare against — which is why ADR-0095 split this seam.
    #[test]
    fn a_placement_insert_writes_nothing_when_the_pair_is_already_on_the_menu() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();

            // A neighbour's placement on its own menu must survive our writes.
            place(&catalog, TENANT_B, "menu-z", ITEM, 99_000)
                .await
                .expect("free");
            let at_create = place(&catalog, TENANT_A, MENU, ITEM, 150_000)
                .await
                .expect("the pair was free");

            assert!(
                place(&catalog, TENANT_A, MENU, ITEM, 160_000)
                    .await
                    .is_none(),
                "the pair is taken"
            );
            let rows = catalog
                .fetch_placements(TENANT_A, MENU)
                .await
                .expect("fetch");
            assert_eq!(rows.len(), 1, "a refused insert adds no row");
            let row = rows.first().expect("one");
            assert!(
                row.prices_json.contains("150000"),
                "and does not reprice the placement it refused to overwrite"
            );
            assert_eq!(
                row.version, at_create,
                "the read carries the version the insert minted, which is the only way a caller \
                 that did not perform the insert can obtain it"
            );

            // Removing frees the pair again, and the neighbour is untouched throughout.
            assert!(
                catalog
                    .delete_placement(TENANT_A, MENU, ITEM)
                    .await
                    .expect("remove")
            );
            assert!(
                place(&catalog, TENANT_A, MENU, ITEM, 150_000)
                    .await
                    .is_some(),
                "the pair is free after the remove"
            );
            assert_eq!(
                catalog
                    .fetch_placements(TENANT_B, "menu-z")
                    .await
                    .expect("fetch neighbour")
                    .len(),
                1
            );
        });
    }

    /// A reprice applies at the version the read carried, refuses a spent one, and answers `NotFound`
    /// for a pair that is not on the menu — the probe on that failure path is what separates a `412`
    /// from a `404`, and it has to name the same table the update did (the defect #152 fixed).
    #[test]
    fn a_placement_reprice_applies_only_at_the_version_the_read_carried() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();
            let at_create = place(&catalog, TENANT_A, MENU, ITEM, 150_000)
                .await
                .expect("the pair was free");

            let at_update = applied(reprice(&catalog, TENANT_A, ITEM, 160_000, &at_create).await)
                .expect("the reprice applies at the version the read carried");
            assert_ne!(at_update, at_create, "the version moves on every write");
            let rows = catalog
                .fetch_placements(TENANT_A, MENU)
                .await
                .expect("fetch");
            assert_eq!(rows.len(), 1, "an update does not add a row");
            assert!(rows.first().expect("one").prices_json.contains("160000"));

            assert_eq!(
                reprice(&catalog, TENANT_A, ITEM, 170_000, &at_create).await,
                RowUpdate::VersionMismatch,
                "replaying a spent version is the lost update"
            );
            assert_eq!(
                reprice(&catalog, TENANT_A, "item-none", 170_000, &at_update).await,
                RowUpdate::NotFound,
                "and an absent pair is a different answer, because the caller's next move differs"
            );
        });
    }

    /// A layout button's identity is the caller's `(channel, item)` slot, and it behaves the same way.
    #[test]
    fn a_layout_button_insert_writes_nothing_when_the_slot_is_taken() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();

            press(&catalog, TENANT_B, ITEM, "Neighbour")
                .await
                .expect("free");
            let at_create = press(&catalog, TENANT_A, ITEM, "Margherita")
                .await
                .expect("the slot was free");

            assert!(
                press(&catalog, TENANT_A, ITEM, "Margherita (classic)")
                    .await
                    .is_none(),
                "the slot is taken"
            );
            let rows = catalog.fetch_layout_buttons(TENANT_A).await.expect("fetch");
            assert_eq!(rows.len(), 1, "a refused insert adds no row");
            let row = rows.first().expect("one");
            assert_eq!(
                row.label, "Margherita",
                "and does not relabel the button it refused to overwrite"
            );
            assert_eq!(row.version, at_create);

            assert!(
                catalog
                    .delete_layout_button(TENANT_A, CHANNEL, ITEM)
                    .await
                    .expect("remove")
            );
            assert_eq!(
                catalog
                    .fetch_layout_buttons(TENANT_B)
                    .await
                    .expect("fetch neighbour")
                    .len(),
                1,
                "the neighbour's button is untouched throughout"
            );
        });
    }

    /// And a relabel is conditional the same way, with the same two distinct refusals.
    #[test]
    fn a_layout_button_relabel_applies_only_at_the_version_the_read_carried() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let catalog = store.catalog();
            let at_create = press(&catalog, TENANT_A, ITEM, "Margherita")
                .await
                .expect("the slot was free");

            let at_update = applied(
                relabel(
                    &catalog,
                    TENANT_A,
                    ITEM,
                    "Margherita (classic)",
                    3,
                    &at_create,
                )
                .await,
            )
            .expect("the relabel applies at the version the read carried");
            assert_ne!(at_update, at_create);
            let rows = catalog.fetch_layout_buttons(TENANT_A).await.expect("fetch");
            assert_eq!(rows.len(), 1);
            let row = rows.first().expect("one");
            assert_eq!(row.label, "Margherita (classic)");
            assert_eq!(row.sort, 3);

            assert_eq!(
                relabel(
                    &catalog,
                    TENANT_A,
                    ITEM,
                    "Margherita (again)",
                    0,
                    &at_create
                )
                .await,
                RowUpdate::VersionMismatch
            );
            assert_eq!(
                relabel(&catalog, TENANT_A, "item-none", "Ghost", 0, &at_update).await,
                RowUpdate::NotFound
            );
        });
    }
}

/// The lease bump against the real column, real default and real `CHECK`
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md), migration 0052).
///
/// The unit tests upstream run against a fake that *mirrors* the `COALESCE` semantics, which proves
/// the callers agree with the fake and nothing about whether the fake agrees with PostgreSQL. The
/// two branches of the upsert behave differently and only one of them is exercised by a first bump,
/// so the keep-what-you-had rule — the one whose failure would quietly readvertise a hosted store as
/// offline-capable — lives entirely in SQL this file is the only thing that runs.
mod edge_placement {
    use pos_proto::{StoreId, TenantId, Ulid};

    use super::{block_on, prepared};

    /// A fixed pair, so the assertions read as one store's history rather than as arithmetic.
    fn ids() -> (TenantId, StoreId) {
        (
            TenantId::new(Ulid::from_u128(0x0EDA)),
            StoreId::new(Ulid::from_u128(0x0EDB)),
        )
    }

    #[test]
    fn a_bump_sets_the_placement_and_a_later_one_without_it_keeps_it() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let trees = store.config_trees();
            let (tenant, store_id) = ids();

            // First bump, no placement named: generation 0, and the column falls to its schema
            // default rather than to the wire's UNSPECIFIED. This is the INSERT branch's COALESCE.
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
            assert_eq!(first.edge_placement, "EDGE_PLACEMENT_IN_STORE");

            // Named: the store moves, and the generation advances in the same statement.
            let moved = super::issue(
                &trees,
                tenant,
                store_id,
                1_777_000_001_000,
                Some("EDGE_PLACEMENT_HOSTED_BY_PLATFORM"),
                None,
                Some(0),
            )
            .await;
            assert_eq!(moved.generation, 1);
            assert_eq!(moved.edge_placement, "EDGE_PLACEMENT_HOSTED_BY_PLATFORM");

            // Not named: ADR-0003's swap of the hosted machine. The ON CONFLICT branch's COALESCE
            // keeps the stored value. Drop that COALESCE and this returns IN_STORE — a store
            // silently re-promised offline trading it cannot do.
            //
            // It also acknowledges generation 0, which the move above left undrained. Without that
            // this bump is refused — which is the point of the refusal, and is asserted on its own
            // below.
            let swapped = super::issue(
                &trees,
                tenant,
                store_id,
                1_777_000_002_000,
                None,
                Some(0),
                Some(1),
            )
            .await;
            assert_eq!(swapped.generation, 2);
            assert_eq!(
                swapped.edge_placement, "EDGE_PLACEMENT_HOSTED_BY_PLATFORM",
                "a bump naming no placement must keep the one the store had"
            );
        });
    }

    /// The region rides the same statement as the placement it qualifies
    /// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Three behaviours, and none of them is provable by the unit tests or the in-process suite:
    /// the `CASE WHEN` that lets a swap keep a region, the insert branch that writes one on a
    /// store's first-ever lease, and the pair being cleared together when a store comes home.
    #[test]
    fn a_bump_writes_the_region_and_a_swap_keeps_it_and_coming_home_clears_it() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let trees = store.config_trees();
            let (tenant, store_id) = (
                TenantId::new(Ulid::from_u128(0x0EDC)),
                StoreId::new(Ulid::from_u128(0x0EDD)),
            );

            // A store's first-ever lease, hosted from the start: the INSERT branch carries the
            // pair. Written straight rather than through the `CASE`, because a row that does not
            // exist has no region to keep.
            let first = super::issue_with_region(
                &trees,
                tenant,
                store_id,
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
            assert_eq!(first.generation, 0);
            assert_eq!(first.region_country.as_deref(), Some("SG"));
            assert_eq!(first.region_label.as_deref(), Some("ap-southeast-1"));

            // A swap in place: the machine is replaced and the place is not. `CASE WHEN $7` is
            // false, so both columns are left alone. Drop that `CASE` for a plain assignment and
            // this returns NULL — a hosted store with no recorded region, which is the exact state
            // ADR-0114 exists to prevent.
            let swapped = super::issue(
                &trees,
                tenant,
                store_id,
                1_777_000_001_000,
                None,
                None,
                Some(0),
            )
            .await;
            assert_eq!(swapped.generation, 1);
            assert_eq!(
                swapped.region_country.as_deref(),
                Some("SG"),
                "a swap in place keeps the region, exactly as it keeps the placement"
            );
            assert_eq!(swapped.region_label.as_deref(), Some("ap-southeast-1"));

            // Moved again, to a different place. The `CASE` writes both.
            let moved = super::issue_with_region(
                &trees,
                tenant,
                store_id,
                1_777_000_002_000,
                Some("EDGE_PLACEMENT_HOSTED_BY_PLATFORM"),
                store_postgres::StoredRegionWrite::Set {
                    country: "JP",
                    label: "堺のラック",
                },
                Some(0),
                Some(1),
            )
            .await;
            assert_eq!(moved.region_country.as_deref(), Some("JP"));
            assert_eq!(
                moved.region_label.as_deref(),
                Some("堺のラック"),
                "the label is `text`, so a non-ASCII place name round-trips unchanged"
            );

            // Home to the shop. Both columns go to NULL in the same `SET` clause that writes the
            // placement, so there is no instant at which the row says in-store *and* names a rack
            // in Sakai.
            let home = super::issue_with_region(
                &trees,
                tenant,
                store_id,
                1_777_000_003_000,
                Some("EDGE_PLACEMENT_IN_STORE"),
                store_postgres::StoredRegionWrite::Clear,
                Some(1),
                Some(2),
            )
            .await;
            assert_eq!(home.edge_placement, "EDGE_PLACEMENT_IN_STORE");
            assert!(
                home.region_country.is_none() && home.region_label.is_none(),
                "an in-store machine is in the shop and has no region"
            );
        });
    }

    #[test]
    fn the_database_refuses_a_placement_the_framework_never_defined() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let trees = store.config_trees();
            let (tenant, store_id) = ids();

            // The route already refuses this, so the CHECK is the second line: it catches a token
            // that reached the column some other way — a hand-run UPDATE during an incident, a
            // fork's own writer. Without it the console would render UNSPECIFIED beside
            // "Offline-capable" for a store that is in fact hosted.
            let refused = trees
                .bump_store_lease(
                    tenant,
                    store_id,
                    1_777_000_000_000,
                    Some("EDGE_PLACEMENT_ON_THE_MOON"),
                    store_postgres::StoredRegionWrite::Keep,
                    None,
                    None,
                )
                .await;
            assert!(
                refused.is_err(),
                "the CHECK constraint must reject a token outside the three modes"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Reason codes (ADR-0115), for the one column only this file can check.
// ---------------------------------------------------------------------------

/// `updated_at` crossing the seam as epoch milliseconds, against the real column.
///
/// The cloud's own tests run over an in-memory fake, which can only prove the routes carry whatever
/// the seam hands them. Whether `(EXTRACT(EPOCH FROM updated_at) * 1000)::bigint` yields a sane
/// millisecond count, and whether an edit moves it, is a question about PostgreSQL — a cast that
/// silently produced seconds would pass every test upstream and date every row in the console to
/// 1970.
mod reason_code_timestamps {
    use super::{TENANT_A, block_on, prepared};

    use store_postgres::RowUpdate;

    /// The reason code this test authors. A ULID, because that is what the id column holds.
    const ID: &str = "01J0000000000000000000000A";

    /// 2020-01-01, in Unix milliseconds — the floor a projected timestamp has to clear.
    ///
    /// Milliseconds and seconds differ by a factor of a thousand, and the only check that separates
    /// them is one against a real clock: this date sits comfortably below any run of this suite and
    /// far above what a seconds-valued cast would produce for any date this decade.
    const Y2020_MS: i64 = 1_577_836_800_000;

    /// An authored record, shaped like the wire one but assembled here so the test crate needs no
    /// dependency on the cloud's types — the adapter treats `doc` as opaque JSON either way.
    fn doc(code: &str) -> String {
        serde_json::json!({
            "id": ID,
            "code": code,
            "display_name": "Waste or spoilage",
            "applies_to": ["REASON_ACTION_VOID_LINE"],
            "active": true,
        })
        .to_string()
    }

    #[test]
    fn a_read_carries_the_write_instant_in_milliseconds_and_an_edit_moves_it() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let reason_codes = store.reason_codes();

            let version = reason_codes
                .insert(TENANT_A, ID, &doc("WASTE"))
                .await
                .expect("insert")
                .expect("the id was free");

            let listed = reason_codes.fetch(TENANT_A).await.expect("list");
            let written = listed.first().expect("the row just inserted").updated_at_ms;

            assert!(
                written > Y2020_MS,
                "the projection is milliseconds since the epoch, not seconds: {written}"
            );

            // An edit moves it. `updated_at = now()` lives in the UPDATE statement and nowhere
            // else — drop it and every row reads as the day it was created, which is exactly the
            // staleness question the console asks this column to answer.
            let updated = reason_codes
                .update_at(TENANT_A, ID, &doc("WASTE_2"), &version)
                .await
                .expect("update");
            assert!(
                matches!(updated, RowUpdate::Updated(_)),
                "the version handed back by the insert still stands"
            );

            let edited = reason_codes
                .fetch_one(TENANT_A, ID)
                .await
                .expect("read")
                .expect("still there")
                .updated_at_ms;
            assert!(
                edited >= written,
                "the edit restamped the row: {written} then {edited}"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// A release's pairs: the release id, the report, and the failure a row carries (ADR-0125).
// ---------------------------------------------------------------------------

mod release_pairs {
    use store_postgres::NewScheduledPublishRow;

    use super::{TENANT_A, block_on, prepared};

    const STORE: &str = "store-1";
    const RELEASE: &str = "01RELEASEAAAAAAAAAAAAAAAAA";

    fn pair<'a>(
        id: &'a str,
        node_key: &'a str,
        release_id: Option<&'a str>,
    ) -> NewScheduledPublishRow<'a> {
        NewScheduledPublishRow {
            id,
            tenant_id: TENANT_A,
            store_id: STORE,
            node_key,
            node_value_json: "{}",
            effective_at_ms: 1_000,
            created_by: "admin-1",
            release_id,
            // The standalone-schedule shape: an instant, resolved against no store's clock.
            resolved_timezone: None,
        }
    }

    /// The report reads every pair of one release and nothing else, and a row that fails carries
    /// its reason until it succeeds.
    ///
    /// Both halves are what a release's `partial` is *derived* from
    /// ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §4): a failure
    /// only the log can see is a release that reports itself as still trying, and a reason that
    /// outlived its failure is a release that looks broken after it recovered.
    #[test]
    fn a_release_reports_its_own_pairs_and_a_failure_clears_when_it_lands() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let scheduled = store.scheduled_publishes();

            scheduled
                .schedule(&pair("rp-tax", "tax", Some(RELEASE)))
                .await
                .expect("tax");
            scheduled
                .schedule(&pair("rp-menu", "menu", Some(RELEASE)))
                .await
                .expect("menu");
            // A standalone schedule in the same store and tenant: ADR-0077's shape, which must not
            // appear in any release's report.
            scheduled
                .schedule(&pair("rp-solo", "campaigns", None))
                .await
                .expect("solo");

            let report = scheduled
                .list_for_release(TENANT_A, RELEASE)
                .await
                .expect("report");
            assert_eq!(report.len(), 2, "the standalone row is not in the release");
            assert!(
                report
                    .iter()
                    .all(|row| row.release_id.as_deref() == Some(RELEASE)),
                "every reported pair belongs to the release"
            );
            // Ordered by store then node, so the grid reads the same on every load.
            let nodes: Vec<&str> = report.iter().map(|row| row.node_key.as_str()).collect();
            assert_eq!(nodes, vec!["menu", "tax"]);

            // A failure lands on the row and the row stays pending, to be retried.
            scheduled
                .mark_failed("rp-menu", "the store's configuration changed")
                .await
                .expect("mark failed");
            let after_failure = scheduled
                .list_for_release(TENANT_A, RELEASE)
                .await
                .expect("report after failure");
            let menu = after_failure
                .iter()
                .find(|row| row.id == "rp-menu")
                .expect("the menu pair");
            assert_eq!(menu.status, "PENDING", "a failure is not terminal");
            assert!(
                menu.failure
                    .as_deref()
                    .is_some_and(|said| said.contains("configuration changed")),
                "the reason is on the row: {:?}",
                menu.failure
            );

            // The retry succeeds, and the row stops explaining itself.
            scheduled
                .mark_applied("rp-menu", "ver-9")
                .await
                .expect("apply");
            let settled = scheduled
                .list_for_release(TENANT_A, RELEASE)
                .await
                .expect("report after apply");
            let menu = settled
                .iter()
                .find(|row| row.id == "rp-menu")
                .expect("the menu pair");
            assert_eq!(menu.status, "APPLIED");
            assert_eq!(
                menu.failure, None,
                "a row that recovered must not keep reporting the failure it survived"
            );
            assert_eq!(menu.applied_version_id.as_deref(), Some("ver-9"));
        });
    }
}

// ---------------------------------------------------------------------------
// Releases: create, list, read one, the status roll-up, and the in-flight scan (ADR-0125).
// ---------------------------------------------------------------------------

mod releases {
    use store_postgres::NewReleaseRow;

    use super::{TENANT_A, TENANT_B, block_on, prepared};

    const WALL: &str = "01RELWALLAAAAAAAAAAAAAAAAA";
    const INSTANT: &str = "01RELINSTANTAAAAAAAAAAAAAA";

    fn wall_clock<'a>(id: &'a str, tenant_id: &'a str, name: &'a str) -> NewReleaseRow<'a> {
        NewReleaseRow {
            id,
            tenant_id,
            name,
            status: "DRAFT",
            target_group_id: Some("01GROUPAAAAAAAAAAAAAAAAAAA"),
            wall_clock_date: Some("2027-02-08"),
            wall_clock_time: Some("04:00"),
            instant_at_ms: None,
            created_by: "admin-1",
        }
    }

    fn instant<'a>(id: &'a str, tenant_id: &'a str, name: &'a str) -> NewReleaseRow<'a> {
        NewReleaseRow {
            id,
            tenant_id,
            name,
            status: "DRAFT",
            target_group_id: None,
            wall_clock_date: None,
            wall_clock_time: None,
            instant_at_ms: Some(1_800_000_000_000),
            created_by: "admin-1",
        }
    }

    /// The two kinds of moment survive a round trip and stay distinguishable, the roll-up moves, and
    /// neither the tenant-scoped reads nor the fleet-wide in-flight scan mixes tenants up.
    ///
    /// The two kinds matter because only one of them is safe at a store whose timezone nobody
    /// recorded ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §2): a
    /// wall-clock release is refused there by name, an instant release is not. If the columns came
    /// back indistinguishable the refusal could not be made.
    #[test]
    fn the_two_kinds_of_moment_survive_and_the_rollup_moves() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let releases = store.config_releases();

            releases
                .create(&wall_clock(WALL, TENANT_A, "Tết 2027"))
                .await
                .expect("wall clock");
            releases
                .create(&instant(INSTANT, TENANT_A, "Price fix"))
                .await
                .expect("instant");
            // Another tenant's release, which must not appear in tenant A's list.
            releases
                .create(&instant(
                    "01RELOTHERAAAAAAAAAAAAAAAA",
                    TENANT_B,
                    "Someone else's",
                ))
                .await
                .expect("other tenant");

            let listed = releases.list_for_tenant(TENANT_A).await.expect("list");
            assert_eq!(listed.len(), 2, "the other tenant's release is not listed");

            let wall = releases
                .fetch_one(TENANT_A, WALL)
                .await
                .expect("read")
                .expect("the wall-clock release");
            assert_eq!(
                wall.name, "Tết 2027",
                "the operator's name comes back as typed"
            );
            assert_eq!(wall.wall_clock_date.as_deref(), Some("2027-02-08"));
            assert_eq!(wall.wall_clock_time.as_deref(), Some("04:00"));
            assert_eq!(
                wall.instant_at_ms, None,
                "a wall-clock release has no single instant: its instants are per store"
            );
            assert_eq!(
                wall.target_group_id.as_deref(),
                Some("01GROUPAAAAAAAAAAAAAAAAAAA"),
                "the cohort it was aimed at is recorded for the report"
            );

            let moment = releases
                .fetch_one(TENANT_A, INSTANT)
                .await
                .expect("read")
                .expect("the instant release");
            assert_eq!(moment.instant_at_ms, Some(1_800_000_000_000));
            assert_eq!(
                moment.wall_clock_date, None,
                "an instant release carries no local date to resolve"
            );

            // Nothing is in flight while both are drafts — a draft fires nothing.
            assert!(
                releases.in_flight().await.expect("in flight").is_empty(),
                "a draft is not in flight"
            );

            assert!(
                releases
                    .set_status(TENANT_A, WALL, "SCHEDULED")
                    .await
                    .expect("schedule it")
            );
            let in_flight = releases.in_flight().await.expect("in flight");
            assert_eq!(in_flight.len(), 1);
            assert_eq!(in_flight.first().expect("row").id, WALL);

            // A terminal status leaves the in-flight scan again.
            assert!(
                releases
                    .set_status(TENANT_A, WALL, "PARTIAL")
                    .await
                    .expect("settle it")
            );
            assert!(
                releases.in_flight().await.expect("in flight").is_empty(),
                "a settled release is no longer in flight"
            );

            // A release of another tenant cannot be moved through this tenant's handle.
            assert!(
                !releases
                    .set_status(TENANT_A, "01RELOTHERAAAAAAAAAAAAAAAA", "SCHEDULED")
                    .await
                    .expect("cross-tenant write"),
                "the write names the tenant, so it changes nothing"
            );
            assert!(
                releases
                    .fetch_one(TENANT_A, "01RELOTHERAAAAAAAAAAAAAAAA")
                    .await
                    .expect("cross-tenant read")
                    .is_none(),
                "and the read cannot see it either"
            );
        });
    }
}

mod chain_anchors {
    //! The cloud's half of the chained event log, against a real planner
    //! ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
    //!
    //! These cases exist because the refusal is enforced by the **schema**, not by the application
    //! layer above it: `chain_anchors` is keyed `(tenant_id, store_id, chain_seq)` and written with
    //! `ON CONFLICT DO NOTHING`, so a store gets one head per chain length and nothing can replace
    //! it. Reasoning about a primary key is not the same as watching PostgreSQL enforce one.

    use super::{TENANT_A, TENANT_B, block_on, prepared};
    use store_postgres::{AnchorRow, ConflictRow};

    fn anchor(chain_seq: i64, head: &str) -> AnchorRow {
        AnchorRow {
            chain_seq,
            chain_head: head.to_owned(),
            unchained: 0,
            observed_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn a_stores_head_is_the_highest_anchor_it_published() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let anchors = store.chain_anchors();

            for row in [anchor(10, "aa"), anchor(25, "bb"), anchor(61, "cc")] {
                anchors
                    .record_anchor(TENANT_A, "store-1", &row)
                    .await
                    .expect("record an anchor");
            }
            // Another store's chain, and another tenant's, must not be mistaken for this one's.
            anchors
                .record_anchor(TENANT_A, "store-2", &anchor(999, "dd"))
                .await
                .expect("record store-2");
            anchors
                .record_anchor(TENANT_B, "store-1", &anchor(999, "ee"))
                .await
                .expect("record tenant-b");

            let (head, at_length) = anchors
                .held_for(TENANT_A, "store-1", 25)
                .await
                .expect("read what is held");
            assert_eq!(
                head.map(|row| row.chain_seq),
                Some(61),
                "the head is the longest chain this store published — not another store's, and \
                 not another tenant's"
            );
            assert_eq!(
                at_length.map(|row| row.chain_head),
                Some("bb".to_owned()),
                "and the row at the offered length is the one recorded there"
            );
        });
    }

    #[test]
    fn the_head_is_also_the_row_at_its_own_length() {
        block_on(async {
            // A store re-sending its latest anchor: one row is both answers, and a read that
            // returned it as only one of them would make a re-delivery look like a backfill.
            let (store, _admin) = prepared().await.expect("prepare the database");
            let anchors = store.chain_anchors();
            anchors
                .record_anchor(TENANT_A, "store-1", &anchor(42, "ab"))
                .await
                .expect("record an anchor");

            let (head, at_length) = anchors
                .held_for(TENANT_A, "store-1", 42)
                .await
                .expect("read what is held");
            assert_eq!(head.as_ref().map(|row| row.chain_seq), Some(42));
            assert_eq!(at_length.as_ref().map(|row| row.chain_seq), Some(42));
        });
    }

    #[test]
    fn a_store_with_no_anchors_holds_nothing() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let (head, at_length) = store
                .chain_anchors()
                .held_for(TENANT_A, "store-1", 1)
                .await
                .expect("read what is held");
            assert!(head.is_none() && at_length.is_none());
        });
    }

    #[test]
    fn a_re_delivered_anchor_is_a_no_op() {
        block_on(async {
            // At-least-once ingest guarantees this happens. It must not fail and must not duplicate.
            let (store, _admin) = prepared().await.expect("prepare the database");
            let anchors = store.chain_anchors();
            for _ in 0..3 {
                anchors
                    .record_anchor(TENANT_A, "store-1", &anchor(42, "ab"))
                    .await
                    .expect("a re-delivery must not fail");
            }
            let (head, _) = anchors
                .held_for(TENANT_A, "store-1", 42)
                .await
                .expect("read what is held");
            assert_eq!(head.map(|row| row.chain_head), Some("ab".to_owned()));
        });
    }

    #[test]
    fn a_contradicting_head_cannot_displace_the_one_already_recorded() {
        block_on(async {
            // The recomputation case, and the reason the primary key is the mechanism rather than a
            // tidiness measure: the store rewrote its history, re-derived every link, and now hashes
            // its log of length 42 to something else. The write succeeds — `ON CONFLICT DO NOTHING`
            // — and changes nothing, so the record that makes the contradiction visible survives
            // even if the layer above forgot to check.
            let (store, _admin) = prepared().await.expect("prepare the database");
            let anchors = store.chain_anchors();
            anchors
                .record_anchor(TENANT_A, "store-1", &anchor(42, "ab"))
                .await
                .expect("record the original");

            anchors
                .record_anchor(TENANT_A, "store-1", &anchor(42, "ff"))
                .await
                .expect("the write does not error");

            let (head, at_length) = anchors
                .held_for(TENANT_A, "store-1", 42)
                .await
                .expect("read what is held");
            assert_eq!(
                head.map(|row| row.chain_head),
                Some("ab".to_owned()),
                "the cloud's record is not overwritten by the one that contradicts it"
            );
            assert_eq!(at_length.map(|row| row.chain_head), Some("ab".to_owned()));
        });
    }

    #[test]
    fn a_truncated_chain_cannot_reclaim_the_lengths_it_already_published() {
        block_on(async {
            // The truncation case, as far as the anchors alone reach. A store that published heads
            // at 10, 25 and 61, then cut its log back and re-traded, has to re-publish lengths the
            // cloud already holds — and every one of them is refused. What this does *not* catch is
            // a store that truncates and then only ever anchors *above* 61: that looks like honest
            // growth from here, and it is the events, not the anchors, that give it away. Recorded
            // as a limit rather than glossed.
            let (store, _admin) = prepared().await.expect("prepare the database");
            let anchors = store.chain_anchors();
            for row in [anchor(10, "aa"), anchor(25, "bb"), anchor(61, "cc")] {
                anchors
                    .record_anchor(TENANT_A, "store-1", &row)
                    .await
                    .expect("record the real history");
            }

            // Cut back to 10 and trade on: the rewritten chain reaches 25 and 61 again with
            // different contents.
            for row in [anchor(25, "f1"), anchor(61, "f2")] {
                anchors
                    .record_anchor(TENANT_A, "store-1", &row)
                    .await
                    .expect("the write does not error");
            }

            for (seq, expected) in [(10, "aa"), (25, "bb"), (61, "cc")] {
                let (_, at_length) = anchors
                    .held_for(TENANT_A, "store-1", seq)
                    .await
                    .expect("read what is held");
                assert_eq!(
                    at_length.map(|row| row.chain_head),
                    Some(expected.to_owned()),
                    "the head the cloud received at length {seq} is the one it keeps"
                );
            }
        });
    }

    #[test]
    fn conflicts_are_listed_newest_first_and_scoped_to_their_tenant() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let anchors = store.chain_anchors();

            let conflict = |chain_seq: i64, offered: &str| ConflictRow {
                chain_seq,
                held_head: "ab".to_owned(),
                offered_head: offered.to_owned(),
                offered_event_id: "EVT0000000000000000000001".to_owned(),
            };
            anchors
                .record_conflict("c1", TENANT_A, "store-1", &conflict(42, "f1"))
                .await
                .expect("record c1");
            anchors
                .record_conflict("c2", TENANT_A, "store-2", &conflict(43, "f2"))
                .await
                .expect("record c2");
            anchors
                .record_conflict("c3", TENANT_B, "store-1", &conflict(44, "f3"))
                .await
                .expect("record c3");

            let tenant_wide = anchors
                .list_conflicts(TENANT_A, None, 10)
                .await
                .expect("list tenant conflicts");
            let ids: Vec<&str> = tenant_wide
                .iter()
                .map(|row| row.conflict_id.as_str())
                .collect();
            assert_eq!(ids.len(), 2, "tenant-b's conflict does not leak in");
            assert!(ids.contains(&"c1") && ids.contains(&"c2"));

            let one_store = anchors
                .list_conflicts(TENANT_A, Some("store-1"), 10)
                .await
                .expect("list one store's conflicts");
            assert_eq!(one_store.len(), 1);
            let found = one_store.first().expect("one row");
            assert_eq!(found.conflict_id, "c1");
            assert_eq!(found.conflict.held_head, "ab");
            assert_eq!(found.conflict.offered_head, "f1");
            assert!(
                found.noticed_at > 0,
                "the cloud's own clock stamped it, which is the timestamp an investigation can rely \
                 on — the store's is on the anchor and a tamperer controls that one"
            );
        });
    }

    #[test]
    fn recording_the_same_conflict_twice_is_one_row() {
        block_on(async {
            // Reconciliation re-pushes whole windows, so the anchor that was refused will arrive
            // again. One finding must stay one row rather than flooding the list it is read from.
            let (store, _admin) = prepared().await.expect("prepare the database");
            let anchors = store.chain_anchors();
            let row = ConflictRow {
                chain_seq: 42,
                held_head: "ab".to_owned(),
                offered_head: "ff".to_owned(),
                offered_event_id: "EVT0000000000000000000001".to_owned(),
            };
            for _ in 0..3 {
                anchors
                    .record_conflict("c1", TENANT_A, "store-1", &row)
                    .await
                    .expect("a re-record must not fail");
            }
            let listed = anchors
                .list_conflicts(TENANT_A, None, 10)
                .await
                .expect("list conflicts");
            assert_eq!(listed.len(), 1);
        });
    }
}

mod chain_window {
    //! The day-bounded window a chain recomputation walks, against a real planner
    //! ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)).
    //!
    //! The read is scoped by tenant, store and a trading-day range, and the range arithmetic is
    //! PostgreSQL's — `pos-proto`'s `BusinessDate` offers none, and inventing calendar arithmetic in
    //! Rust to avoid one `interval` would be the worse trade. Which means the day maths is only
    //! really checked here.

    use super::{TENANT_A, TENANT_B, block_on, prepared, seed_row_on};

    #[test]
    fn a_window_reads_one_store_one_tenant_and_the_days_it_was_asked_for() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            // Three days of this store's trading, plus another store's and another tenant's on the
            // same dates — neither of which may appear in this store's window.
            seed_row_on(&admin, 1, TENANT_A, "store-1", "2026-03-13")
                .await
                .expect("seed the day before yesterday");
            seed_row_on(&admin, 2, TENANT_A, "store-1", "2026-03-14")
                .await
                .expect("seed yesterday");
            seed_row_on(&admin, 3, TENANT_A, "store-1", "2026-03-15")
                .await
                .expect("seed today");
            seed_row_on(&admin, 4, TENANT_A, "store-2", "2026-03-15")
                .await
                .expect("seed another store");
            seed_row_on(&admin, 5, TENANT_B, "store-1", "2026-03-15")
                .await
                .expect("seed another tenant");

            let anchors = store.chain_anchors();

            // No look-back: today only.
            let today = anchors
                .events_in_window(TENANT_A, "store-1", "2026-03-15", 0, 100)
                .await
                .expect("read today");
            assert_eq!(
                today.len(),
                1,
                "one day, one store, one tenant — not the neighbours on the same date"
            );

            // One day back, which is what a shift crossing the store's cut-off needs.
            let two_days = anchors
                .events_in_window(TENANT_A, "store-1", "2026-03-15", 1, 100)
                .await
                .expect("read today and yesterday");
            assert_eq!(two_days.len(), 2);

            // And the bound holds: the day before yesterday stays out.
            assert_eq!(
                anchors
                    .events_in_window(TENANT_A, "store-1", "2026-03-15", 2, 100)
                    .await
                    .expect("read three days")
                    .len(),
                3,
                "the range is inclusive at both ends, so two days back reaches the third day"
            );
        });
    }

    #[test]
    fn the_look_back_crosses_a_month_boundary() {
        block_on(async {
            // The reason this arithmetic is PostgreSQL's rather than mine: the day before the first
            // of a month is not the zeroth, and February is not thirty days long.
            let (store, admin) = prepared().await.expect("prepare the database");
            seed_row_on(&admin, 1, TENANT_A, "store-1", "2026-02-28")
                .await
                .expect("seed the last day of February");
            seed_row_on(&admin, 2, TENANT_A, "store-1", "2026-03-01")
                .await
                .expect("seed the first of March");

            let found = store
                .chain_anchors()
                .events_in_window(TENANT_A, "store-1", "2026-03-01", 1, 100)
                .await
                .expect("read across the month boundary");
            assert_eq!(
                found.len(),
                2,
                "a shift that opened on the last night of February closes on the first of March"
            );
        });
    }

    #[test]
    fn a_window_is_capped_and_ordered_by_event_id() {
        block_on(async {
            // A short read is what the caller sees as an incomplete window rather than a clean one,
            // so the cap has to actually cap — and the order has to be the log's.
            let (store, admin) = prepared().await.expect("prepare the database");
            for seed in 1..=5 {
                seed_row_on(&admin, seed, TENANT_A, "store-1", "2026-03-15")
                    .await
                    .expect("seed a day's trading");
            }

            let capped = store
                .chain_anchors()
                .events_in_window(TENANT_A, "store-1", "2026-03-15", 0, 3)
                .await
                .expect("read a capped window");
            assert_eq!(capped.len(), 3, "the cap is honoured, not advisory");
        });
    }

    #[test]
    fn a_store_that_traded_on_no_such_day_has_an_empty_window() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            assert!(
                store
                    .chain_anchors()
                    .events_in_window(TENANT_A, "store-1", "2026-03-15", 1, 100)
                    .await
                    .expect("read an empty window")
                    .is_empty()
            );
        });
    }
}

// ---------------------------------------------------------------------------
// A tenant's vendor connections (ADR-0153, migration 0070).
// ---------------------------------------------------------------------------

/// The connection table's per-record shape against a real PostgreSQL: a taken id is refused, an
/// edit needs the version it read, a tenant never reads another's row — by the adapter's own
/// `WHERE` and, as the query role, by the migration's policy — and a delete takes the row.
mod integration_connections {
    use super::{TENANT_A, TENANT_B, block_on, prepared};

    use store_postgres::RowUpdate;

    const ID: &str = "01J0000000000000000000CONN";

    /// A stored record as `pos-cloud` writes one, its secret already a sealed value. The adapter
    /// treats `doc` as opaque JSON, so the test crate needs none of the cloud's types.
    fn doc(display_name: &str) -> String {
        serde_json::json!({
            "connection_id": ID,
            "provider_id": "courier.ahamove",
            "scope_level": "CONNECTION_SCOPE_TENANT",
            "display_name": display_name,
            "enabled": true,
            "settings": { "base_url": "https://api.example" },
            "sealed_secrets": { "api_key": "00ff" },
        })
        .to_string()
    }

    #[test]
    fn a_connection_is_created_once_edited_at_its_version_and_deleted() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let connections = store.connections();

            let version = connections
                .insert(TENANT_A, ID, &doc("Ahamove"))
                .await
                .expect("insert")
                .expect("the id was free");
            assert_eq!(
                connections
                    .insert(TENANT_A, ID, &doc("Again"))
                    .await
                    .expect("insert"),
                None,
                "a taken id is refused, not replaced"
            );

            let listed = connections.fetch(TENANT_A).await.expect("list");
            assert_eq!(listed.len(), 1);
            assert!(
                listed
                    .first()
                    .expect("the row")
                    .doc_json
                    .contains("Ahamove")
            );
            assert!(
                connections.fetch(TENANT_B).await.expect("list").is_empty(),
                "another tenant reads nothing"
            );

            let updated = connections
                .update_at(TENANT_A, ID, &doc("Ahamove, District 1"), &version)
                .await
                .expect("update");
            assert!(matches!(updated, RowUpdate::Updated(_)));
            let stale = connections
                .update_at(TENANT_A, ID, &doc("Lost update"), &version)
                .await
                .expect("update");
            assert!(
                matches!(stale, RowUpdate::VersionMismatch),
                "an edit made from a stale read is refused"
            );

            connections.delete(TENANT_A, ID).await.expect("delete");
            connections
                .delete(TENANT_A, ID)
                .await
                .expect("deleting it again is not an error");
            assert!(
                connections
                    .fetch_one(TENANT_A, ID)
                    .await
                    .expect("read")
                    .is_none()
            );
        });
    }

    #[test]
    fn the_query_role_sees_only_its_own_tenants_connections() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let connections = store.connections();
            connections
                .insert(TENANT_A, ID, &doc("Tenant A's"))
                .await
                .expect("insert A")
                .expect("free");
            connections
                .insert(TENANT_B, ID, &doc("Tenant B's"))
                .await
                .expect("insert B")
                .expect("free");

            admin
                .batch_execute("SET ROLE app_tenant")
                .await
                .expect("assume the app_tenant role");
            admin
                .execute(
                    "SELECT set_config('app.tenant_id', $1, false)",
                    &[&TENANT_A],
                )
                .await
                .expect("scope the session to tenant A");
            let visible: Vec<String> = admin
                .query("SELECT tenant_id FROM integration_connections", &[])
                .await
                .expect("readable")
                .iter()
                .map(|row| row.get(0))
                .collect();
            admin
                .batch_execute("RESET ROLE")
                .await
                .expect("reset the role");
            assert_eq!(visible, vec![TENANT_A.to_owned()]);
        });
    }
}

// ---------------------------------------------------------------------------
// A tenant's fee rules (ADR-0159, migration 0075).
// ---------------------------------------------------------------------------

/// The fee-rule table against a real PostgreSQL, held to the rules `pos-cloud`'s `FeeRuleStore`
/// contract suite holds its in-memory store to: a second write to a slot replaces the first, the
/// same fee at another scope is a row of its own, the list is in scope, scope-id and fee-id order by
/// bytes, a delete says whether there was a rule, and a tenant never reads or deletes another's — by
/// the adapter's own `WHERE` and, as the query role, by the migration's policy.
mod fee_rules {
    use super::{TENANT_A, TENANT_B, block_on, prepared};

    use store_postgres::{FeeRuleSlot, FeeRuleWrite, PostgresFeeRules};

    const FEE: &str = "01J000000000000000000000FE";

    const STORE_SLOT: FeeRuleSlot<'static> = FeeRuleSlot {
        scope: "FEE_SCOPE_STORE",
        scope_id: "01J0000000000000000000STOR",
        fee_id: FEE,
    };

    /// A rule as `pos-cloud` writes one. The adapter treats `doc` as opaque JSON, so the test crate
    /// needs none of the cloud's types.
    fn doc(code: &str) -> String {
        serde_json::json!({
            "fee_id": FEE,
            "code": code,
            "display_name": "Service charge",
            "kind": "FEE_KIND_PERCENT",
            "rate": { "numerator": 5, "denominator": 100 },
        })
        .to_string()
    }

    /// Writes `doc_json` into `slot` for `tenant`, at `update_time_ms`, as `updated_by`.
    async fn write(
        rules: &PostgresFeeRules,
        tenant: &str,
        slot: FeeRuleSlot<'_>,
        doc_json: &str,
        update_time_ms: i64,
        updated_by: &str,
    ) {
        rules
            .upsert(
                tenant,
                slot,
                FeeRuleWrite {
                    doc_json,
                    update_time_ms,
                    updated_by,
                },
            )
            .await
            .expect("write a fee rule");
    }

    #[test]
    fn a_rule_is_written_replaced_and_deleted_in_its_slot() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let rules = store.fee_rules();

            let first = doc("SERVICE");
            write(&rules, TENANT_A, STORE_SLOT, &first, 1_000, "admin-1").await;
            write(
                &rules,
                TENANT_A,
                STORE_SLOT,
                &doc("SERVICE_2"),
                2_000,
                "admin-2",
            )
            .await;
            let tenant_slot = FeeRuleSlot {
                scope: "FEE_SCOPE_TENANT",
                scope_id: TENANT_A,
                ..STORE_SLOT
            };
            write(&rules, TENANT_A, tenant_slot, &first, 3_000, "admin-1").await;
            // Two brands whose ids sort one way by bytes and the other way in a natural-language
            // collation, so the list's order is pinned to the bytes, as the in-memory store's is.
            for brand in ["a-brand", "B-brand"] {
                let brand_slot = FeeRuleSlot {
                    scope: "FEE_SCOPE_BRAND",
                    scope_id: brand,
                    ..STORE_SLOT
                };
                write(&rules, TENANT_A, brand_slot, &first, 4_000, "admin-1").await;
            }

            let listed = rules.fetch(TENANT_A).await.expect("list");
            assert_eq!(
                listed
                    .iter()
                    .map(|row| (row.scope.as_str(), row.scope_id.as_str()))
                    .collect::<Vec<_>>(),
                [
                    ("FEE_SCOPE_BRAND", "B-brand"),
                    ("FEE_SCOPE_BRAND", "a-brand"),
                    ("FEE_SCOPE_STORE", STORE_SLOT.scope_id),
                    ("FEE_SCOPE_TENANT", TENANT_A),
                ],
                "the second write to a slot replaced the first, each scope's is its own row, and \
                 the list is in scope and scope-id order, by bytes"
            );
            let store_rule = listed
                .iter()
                .find(|row| row.scope == "FEE_SCOPE_STORE")
                .expect("the store's rule");
            assert!(store_rule.doc_json.contains("SERVICE_2"), "the later rule");
            assert_eq!(store_rule.update_time_ms, 2_000, "the cloud's clock, kept");
            assert_eq!(store_rule.updated_by, "admin-2");
            assert_eq!(store_rule.fee_id, FEE);
            assert!(
                rules.fetch(TENANT_B).await.expect("list").is_empty(),
                "another tenant reads nothing"
            );

            assert!(
                !rules.delete(TENANT_B, STORE_SLOT).await.expect("delete"),
                "another tenant cannot delete it"
            );
            assert!(rules.delete(TENANT_A, STORE_SLOT).await.expect("delete"));
            assert!(
                !rules
                    .delete(TENANT_A, STORE_SLOT)
                    .await
                    .expect("delete again"),
                "deleting what is not there is no error"
            );
            assert_eq!(rules.fetch(TENANT_A).await.expect("list").len(), 3);
        });
    }

    /// A rule with every field set comes back from the `jsonb` column as the rule it was written:
    /// the column keeps its own spacing and key order, and what `pos-cloud` parses from that text is
    /// still the same rule, its money, its translated name and its open enums included.
    #[test]
    fn a_whole_rule_reads_back_from_jsonb_as_it_was_written() {
        use std::collections::BTreeMap;

        use pos_proto::Ulid;
        use pos_proto::enums::SalesChannel;
        use pos_proto::fees::{FeeCode, FeeItems, FeeKind, FeeTax, PublishedFee};
        use pos_proto::ids::{FeeId, MenuItemId, TaxClassId};
        use pos_proto::money::{CurrencyCode, Money, Ratio};
        use pos_proto::text::DisplayName;
        use pos_proto::wire_enum::Open;

        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let rules = store.fee_rules();
            let fee_id = FeeId::new(Ulid::from_u128(0xFE));
            let rule = PublishedFee {
                fee_id,
                code: FeeCode::new("PACKAGING"),
                display_name: DisplayName::new("Packaging"),
                display_name_translations: BTreeMap::from([(
                    "vi".to_owned(),
                    DisplayName::new("Phí đóng gói"),
                )]),
                kind: Open::from_known(FeeKind::AmountPerUnit),
                rate: Some(Ratio::percent(5).expect("a rate")),
                amount: Some(Money::new(CurrencyCode::VND, 5_000)),
                channels: vec![
                    Open::from_known(SalesChannel::Takeaway),
                    Open::from_known(SalesChannel::Delivery),
                ],
                item_scope: Open::from_known(FeeItems::Include),
                menu_item_ids: vec![
                    MenuItemId::new(Ulid::from_u128(1)),
                    MenuItemId::new(Ulid::from_u128(2)),
                ],
                base_discounted: false,
                base_tax_inclusive: true,
                tax: Open::from_known(FeeTax::TaxClass),
                tax_class_id: Some(TaxClassId::new(Ulid::from_u128(3))),
                waivable: true,
                active: false,
            };
            let doc_json = serde_json::to_string(&rule).expect("encode the rule");
            let fee_id_text = fee_id.to_string();
            let slot = FeeRuleSlot {
                fee_id: &fee_id_text,
                ..STORE_SLOT
            };
            write(&rules, TENANT_A, slot, &doc_json, 1_000, "admin-1").await;

            let listed = rules.fetch(TENANT_A).await.expect("list");
            assert_eq!(listed.len(), 1);
            let row = listed.first().expect("the rule");
            assert_ne!(row.doc_json, doc_json, "jsonb keeps its own spacing");
            let read: PublishedFee =
                serde_json::from_str(&row.doc_json).expect("the stored rule parses");
            assert_eq!(read, rule);
        });
    }

    #[test]
    fn the_query_role_sees_only_its_own_tenants_rules() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let rules = store.fee_rules();
            let rule = doc("SERVICE");
            for tenant in [TENANT_A, TENANT_B] {
                write(&rules, tenant, STORE_SLOT, &rule, 1_000, "admin-1").await;
            }

            admin
                .batch_execute("SET ROLE app_tenant")
                .await
                .expect("assume the app_tenant role");
            admin
                .execute(
                    "SELECT set_config('app.tenant_id', $1, false)",
                    &[&TENANT_A],
                )
                .await
                .expect("scope the session to tenant A");
            let visible: Vec<String> = admin
                .query("SELECT tenant_id FROM fee_rules", &[])
                .await
                .expect("readable")
                .iter()
                .map(|row| row.get(0))
                .collect();
            admin
                .batch_execute("RESET ROLE")
                .await
                .expect("reset the role");
            assert_eq!(visible, vec![TENANT_A.to_owned()]);
        });
    }
}

// ---------------------------------------------------------------------------
// A tenant's setting values (ADR-0160, migration 0072).
// ---------------------------------------------------------------------------

/// The setting table against a real PostgreSQL: a second write to a slot replaces the first, a
/// delete says whether there was a value, and a tenant never reads another's — by the adapter's own
/// `WHERE` and, as the query role, by the migration's policy.
mod setting_values {
    use super::{TENANT_A, TENANT_B, block_on, prepared};

    use store_postgres::SettingSlot;

    const STORE_SLOT: SettingSlot<'static> = SettingSlot {
        setting_key: "shift.no_shift_selling",
        scope: "SETTING_SCOPE_STORE",
        scope_id: "01J0000000000000000000STOR",
    };

    /// A stored value as `pos-cloud` writes one. The adapter treats `doc` as opaque JSON, so the
    /// test crate needs none of the cloud's types.
    fn doc(token: &str) -> String {
        serde_json::json!({
            "setting_key": STORE_SLOT.setting_key,
            "scope": STORE_SLOT.scope,
            "scope_id": STORE_SLOT.scope_id,
            "value": token,
            "update_time": "2026-10-01T00:00:00Z",
        })
        .to_string()
    }

    #[test]
    fn a_value_is_written_replaced_and_deleted_in_its_slot() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let settings = store.settings();

            settings
                .upsert(TENANT_A, STORE_SLOT, &doc("NO_SHIFT_SELLING_REFUSE"))
                .await
                .expect("write");
            settings
                .upsert(TENANT_A, STORE_SLOT, &doc("NO_SHIFT_SELLING_ALLOW"))
                .await
                .expect("write again");
            let tenant_slot = SettingSlot {
                scope: "SETTING_SCOPE_TENANT",
                scope_id: TENANT_A,
                ..STORE_SLOT
            };
            settings
                .upsert(TENANT_A, tenant_slot, &doc("NO_SHIFT_SELLING_REFUSE"))
                .await
                .expect("write at another scope");

            let listed = settings.fetch(TENANT_A).await.expect("list");
            assert_eq!(
                listed.len(),
                2,
                "the second write to a slot replaced the first"
            );
            assert!(
                listed
                    .iter()
                    .any(|row| row.contains("NO_SHIFT_SELLING_ALLOW")),
                "the store's slot holds the later value"
            );
            assert!(
                settings.fetch(TENANT_B).await.expect("list").is_empty(),
                "another tenant reads nothing"
            );

            assert!(settings.delete(TENANT_A, STORE_SLOT).await.expect("delete"));
            assert!(
                !settings
                    .delete(TENANT_A, STORE_SLOT)
                    .await
                    .expect("delete again"),
                "deleting what is not there is no error"
            );
            assert_eq!(settings.fetch(TENANT_A).await.expect("list").len(), 1);
        });
    }

    #[test]
    fn the_query_role_sees_only_its_own_tenants_values() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let settings = store.settings();
            settings
                .upsert(TENANT_A, STORE_SLOT, &doc("NO_SHIFT_SELLING_REFUSE"))
                .await
                .expect("write A");
            settings
                .upsert(TENANT_B, STORE_SLOT, &doc("NO_SHIFT_SELLING_ALLOW"))
                .await
                .expect("write B");

            admin
                .batch_execute("SET ROLE app_tenant")
                .await
                .expect("assume the app_tenant role");
            admin
                .execute(
                    "SELECT set_config('app.tenant_id', $1, false)",
                    &[&TENANT_A],
                )
                .await
                .expect("scope the session to tenant A");
            let visible: Vec<String> = admin
                .query("SELECT tenant_id FROM setting_values", &[])
                .await
                .expect("readable")
                .iter()
                .map(|row| row.get(0))
                .collect();
            admin
                .batch_execute("RESET ROLE")
                .await
                .expect("reset the role");
            assert_eq!(visible, vec![TENANT_A.to_owned()]);
        });
    }
}

/// The record of the one-time changes the cloud makes through its own code (`data_migrations`):
/// a change is unrecorded until it is recorded, recording it twice keeps the first row, and a name
/// recorded by a migration's own SQL reads as made too, since the two share the table.
mod data_migrations {
    use super::{block_on, prepared};

    const CHANGE: &str = "qr_ordering_one_switch";

    #[test]
    fn a_change_reads_as_made_at_its_time_once_recorded_and_recording_it_again_keeps_the_first() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let markers = store.data_migrations();

            assert_eq!(
                markers
                    .recorded_time(CHANGE)
                    .await
                    .expect("read the record"),
                None,
                "nothing has recorded the change yet"
            );
            markers
                .record(CHANGE, 1_790_000_000_123)
                .await
                .expect("record the change");
            assert_eq!(
                markers
                    .recorded_time(CHANGE)
                    .await
                    .expect("read the record"),
                Some(1_790_000_000_123),
                "the time comes back to the millisecond"
            );

            markers
                .record(CHANGE, 1_790_000_999_000)
                .await
                .expect("record the change again");
            let rows = admin
                .query(
                    "SELECT name FROM data_migrations WHERE name = $1",
                    &[&CHANGE],
                )
                .await
                .expect("read the rows");
            assert_eq!(rows.len(), 1, "one row per change");
            assert_eq!(
                markers
                    .recorded_time(CHANGE)
                    .await
                    .expect("read the record"),
                Some(1_790_000_000_123),
                "the second record keeps the first row, and its time"
            );

            // A name a migration's SQL claimed is in the same table, so it reads as made as well.
            admin
                .execute(
                    "INSERT INTO data_migrations (name) VALUES ('0073_roles_keep_every_till_action')",
                    &[],
                )
                .await
                .expect("claim a SQL migration's row");
            assert!(
                markers
                    .recorded_time("0073_roles_keep_every_till_action")
                    .await
                    .expect("read the record")
                    .is_some()
            );
        });
    }
}
