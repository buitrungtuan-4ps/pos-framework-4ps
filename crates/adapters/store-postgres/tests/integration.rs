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

#[path = "integration/subjects_store.rs"]
mod subjects_store;

// ---------------------------------------------------------------------------
// The webhook-endpoint store (ADR-0032): registration facts, the admin CRUD's
// tenant-scoped listing, and the delivery task's fleet-wide enabled load.
// ---------------------------------------------------------------------------

#[path = "integration/webhooks_store.rs"]
mod webhooks_store;

// ---------------------------------------------------------------------------
// The reconciliation membership query (ADR-0040): which candidate ids the
// cloud already holds, scoped by tenant and store.
// ---------------------------------------------------------------------------

#[path = "integration/reconcile_query.rs"]
mod reconcile_query;

// ---------------------------------------------------------------------------
// The device-proposal onboarding queue (ADR-0041): propose, list by status,
// and the one-way pending → approved/rejected resolve.
// ---------------------------------------------------------------------------

#[path = "integration/device_proposals.rs"]
mod device_proposals;

// ---------------------------------------------------------------------------
// Inventory authoring (ADR-0079): ingredients, recipes, and suppliers as
// per-(tenant, kind, id) jsonb rows, round-tripped and tenant-scoped.
// ---------------------------------------------------------------------------

#[path = "integration/inventory_authoring.rs"]
mod inventory_authoring;

// ---------------------------------------------------------------------------
// Conditional writes over the `xmin` system column (ADR-0094).
// ---------------------------------------------------------------------------

#[path = "integration/conditional_writes.rs"]
mod conditional_writes;

// ---------------------------------------------------------------------------
// Catalog placements and layout buttons: the two caller-keyed rows, on the insert/update-at split
// (ADR-0095). Both were single upserts, and both have their whole write path here for the first
// time — the class of defect #152 fixed (a presence probe naming the wrong table) is invisible to
// every test that does not run the SQL.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The item master, paged: window, total, order and the index that carries it (ADR-0098).
// ---------------------------------------------------------------------------

#[path = "integration/catalog_item_pages.rs"]
mod catalog_item_pages;

#[path = "integration/catalog_keyed_rows.rs"]
mod catalog_keyed_rows;

/// The lease bump against the real column, real default and real `CHECK`
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md), migration 0052).
///
/// The unit tests upstream run against a fake that *mirrors* the `COALESCE` semantics, which proves
/// the callers agree with the fake and nothing about whether the fake agrees with PostgreSQL. The
/// two branches of the upsert behave differently and only one of them is exercised by a first bump,
/// so the keep-what-you-had rule — the one whose failure would quietly readvertise a hosted store as
/// offline-capable — lives entirely in SQL this file is the only thing that runs.
#[path = "integration/edge_placement.rs"]
mod edge_placement;

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
#[path = "integration/reason_code_timestamps.rs"]
mod reason_code_timestamps;

// ---------------------------------------------------------------------------
// A release's pairs: the release id, the report, and the failure a row carries (ADR-0125).
// ---------------------------------------------------------------------------

#[path = "integration/release_pairs.rs"]
mod release_pairs;

// ---------------------------------------------------------------------------
// Releases: create, list, read one, the status roll-up, and the in-flight scan (ADR-0125).
// ---------------------------------------------------------------------------

#[path = "integration/releases.rs"]
mod releases;

#[path = "integration/chain_anchors.rs"]
mod chain_anchors;

#[path = "integration/chain_window.rs"]
mod chain_window;

// ---------------------------------------------------------------------------
// A tenant's vendor connections (ADR-0153, migration 0070).
// ---------------------------------------------------------------------------

/// The connection table's per-record shape against a real PostgreSQL: a taken id is refused, an
/// edit needs the version it read, a tenant never reads another's row — by the adapter's own
/// `WHERE` and, as the query role, by the migration's policy — and a delete takes the row.
#[path = "integration/integration_connections.rs"]
mod integration_connections;

// ---------------------------------------------------------------------------
// A tenant's fee rules (ADR-0159, migration 0075).
// ---------------------------------------------------------------------------

/// The fee-rule table against a real PostgreSQL, held to the rules `pos-cloud`'s `FeeRuleStore`
/// contract suite holds its in-memory store to: a second write to a slot replaces the first, the
/// same fee at another scope is a row of its own, the list is in scope, scope-id and fee-id order by
/// bytes, a delete says whether there was a rule, and a tenant never reads or deletes another's — by
/// the adapter's own `WHERE` and, as the query role, by the migration's policy.
#[path = "integration/fee_rules.rs"]
mod fee_rules;

// ---------------------------------------------------------------------------
// A tenant's setting values (ADR-0160, migration 0072).
// ---------------------------------------------------------------------------

/// The setting table against a real PostgreSQL: a second write to a slot replaces the first, a
/// delete says whether there was a value, and a tenant never reads another's — by the adapter's own
/// `WHERE` and, as the query role, by the migration's policy.
#[path = "integration/setting_values.rs"]
mod setting_values;

/// The record of the one-time changes the cloud makes through its own code (`data_migrations`):
/// a change is unrecorded until it is recorded, recording it twice keeps the first row, and a name
/// recorded by a migration's own SQL reads as made too, since the two share the table.
#[path = "integration/data_migrations.rs"]
mod data_migrations;
