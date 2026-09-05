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
//! Every case shares one database, so `fresh()` truncates between cases and the whole file must
//! run with `--test-threads=1`. Cargo runs separate integration-test *binaries* concurrently, so
//! splitting these across files would reintroduce exactly the races the truncation is there to
//! avoid — hence one file. See the crate's `integration` feature note in `Cargo.toml`.
//!
//! Run it:
//!
//! ```text
//! DATABASE_URL='host=localhost port=55432 user=pos dbname=poscloud' \
//!   cargo test -p store-postgres --features integration -- --test-threads=1
//! ```

// Gated so the pull-request build neither compiles nor runs it — the merge-to-`main` integration
// job turns the feature on against a pinned `postgres:16` service.
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
                 api_keys, audit_log, brands, campaigns, catalog_display_categories, \
                 catalog_display_subcategories, catalog_item_categories, catalog_item_subcategories, \
                 catalog_items, catalog_layout_buttons, catalog_menu_sections, catalog_menus, \
                 catalog_modifier_groups, catalog_placements, catalog_tax_classes, catalog_tax_rate_versions, \
                 catalog_tax_rates, config_trees, device_credentials, device_proposals, devices, \
                 employee_store_assignments, employees, event_outbox, events, floor_areas, floor_tables, \
                 inventory_items, kitchen_stations, media_assets, order_queue, ota_releases, reconcile_runs, \
                 role_templates, rollups, scheduled_publishes, station_routing_rules, store_liveness, stores, \
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
             api_keys, audit_log, brands, campaigns, catalog_display_categories, \
             catalog_display_subcategories, catalog_item_categories, catalog_item_subcategories, \
             catalog_items, catalog_layout_buttons, catalog_menu_sections, catalog_menus, \
             catalog_modifier_groups, catalog_placements, catalog_tax_classes, catalog_tax_rate_versions, \
             catalog_tax_rates, config_trees, device_credentials, device_proposals, devices, \
             employee_store_assignments, employees, event_outbox, events, floor_areas, floor_tables, \
             inventory_items, kitchen_stations, media_assets, order_queue, ota_releases, reconcile_runs, \
             role_templates, rollups, scheduled_publishes, station_routing_rules, store_liveness, stores, \
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

mod admin_store {
    use super::{block_on, prepared};

    /// Seeds the single super-admin row with obviously-fake credentials — never real key material.
    async fn seed_admin(admin: &tokio_postgres::Client) {
        let phc: &str = "$argon2id$v=19$m=19456,t=2,p=1$ZmFrZXNhbHQ$ZmFrZWhhc2h2YWx1ZQ";
        let secret: &[u8] = b"fake-totp-seed-not-real";
        admin
            .execute(
                "INSERT INTO super_admin (id, password_phc, totp_secret, last_used_totp_step) \
                 VALUES (true, $1, $2, NULL)",
                &[&phc, &secret],
            )
            .await
            .expect("seed the super-admin row");
    }

    /// The credential reads back exactly as stored, and the TOTP step advances only forward.
    #[test]
    fn the_credential_round_trips_and_the_step_never_moves_backwards() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let credentials = store.admin();
            assert!(
                credentials
                    .fetch_credential()
                    .await
                    .expect("fetch")
                    .is_none(),
                "no credential before provisioning"
            );

            seed_admin(&admin).await;
            let row = credentials
                .fetch_credential()
                .await
                .expect("fetch")
                .expect("provisioned");
            assert_eq!(
                row.password_phc,
                "$argon2id$v=19$m=19456,t=2,p=1$ZmFrZXNhbHQ$ZmFrZWhhc2h2YWx1ZQ"
            );
            assert_eq!(row.totp_secret, b"fake-totp-seed-not-real");
            assert_eq!(
                row.last_used_totp_step, None,
                "unused until the first login"
            );

            credentials.advance_totp_step(100).await.expect("advance");
            // A lower step must not move it backwards — the guard is a no-op here.
            credentials
                .advance_totp_step(50)
                .await
                .expect("attempt lower");
            let row = credentials
                .fetch_credential()
                .await
                .expect("fetch")
                .expect("provisioned");
            assert_eq!(
                row.last_used_totp_step,
                Some(100),
                "the step advanced to 100 and a lower write did not move it back"
            );
        });
    }

    /// A session is live only up to its expiry, and revoking it removes it.
    #[test]
    fn a_session_is_live_until_its_expiry_and_gone_after_revoke() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let sessions = store.admin();
            let hash = [7_u8; 32];
            // A legacy fixed-TTL session (no absolute cap / idle window): it does not slide.
            sessions
                .insert_session(store_postgres::NewSessionRow {
                    token_hash: &hash,
                    created_at_ms: 1000,
                    expires_at_ms: 2000,
                    absolute_expires_at_ms: None,
                    idle_ttl_ms: None,
                    admin_id: None,
                    ip: None,
                    user_agent: None,
                })
                .await
                .expect("insert the session");

            assert!(
                sessions.session_valid(&hash, 1999).await.expect("query"),
                "live one millisecond before expiry"
            );
            assert!(
                !sessions.session_valid(&hash, 2000).await.expect("query"),
                "not live at the expiry instant (the check is strictly greater-than)"
            );
            assert!(
                !sessions
                    .session_valid(&[9_u8; 32], 1999)
                    .await
                    .expect("query"),
                "an unknown token names no session"
            );

            sessions
                .delete_session(&hash)
                .await
                .expect("revoke the session");
            assert!(
                !sessions.session_valid(&hash, 1999).await.expect("query"),
                "a revoked session is gone even before its expiry"
            );
        });
    }

    /// A modern session slides its idle TTL forward on a real request, but never past its absolute
    /// cap ([ADR-0067] slice 4).
    #[test]
    fn a_modern_session_slides_up_to_its_absolute_cap() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let sessions = store.admin();
            sessions
                .insert_admin_user(
                    "adm-slide",
                    "slide@x.test",
                    "S",
                    "owner",
                    "active",
                    "$phc",
                    b"s",
                )
                .await
                .expect("seed admin");
            let hash = [11_u8; 32];
            // Idle window 60_000 ms; absolute cap at 10_000_000.
            sessions
                .insert_session(store_postgres::NewSessionRow {
                    token_hash: &hash,
                    created_at_ms: 0,
                    expires_at_ms: 60_000,
                    absolute_expires_at_ms: Some(10_000_000),
                    idle_ttl_ms: Some(60_000),
                    admin_id: Some("adm-slide"),
                    ip: None,
                    user_agent: None,
                })
                .await
                .expect("insert the session");

            // A real request at 50_000 ms slides the expiry to 50_000 + 60_000 = 110_000.
            assert_eq!(
                sessions
                    .fetch_session_admin(&hash, 50_000)
                    .await
                    .expect("query"),
                Some(Some("adm-slide".to_owned())),
            );
            assert!(
                sessions.session_valid(&hash, 100_000).await.expect("query"),
                "the slid session outlives its original 60_000 ms boundary"
            );

            // Sliding right before the cap cannot push the expiry past it.
            sessions
                .fetch_session_admin(&hash, 9_995_000)
                .await
                .expect("query");
            assert!(
                !sessions
                    .session_valid(&hash, 10_000_001)
                    .await
                    .expect("query"),
                "no amount of sliding lets a session outlive its absolute cap"
            );
        });
    }

    /// An admin lists and revokes only their own sessions; "revoke others" keeps the current one.
    #[test]
    fn admin_sessions_list_and_revoke_are_scoped_to_the_owner() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let sessions = store.admin();
            for (id, email) in [("own-1", "o1@x.test"), ("own-2", "o2@x.test")] {
                sessions
                    .insert_admin_user(id, email, "N", "admin", "active", "$phc", b"s")
                    .await
                    .expect("seed admin");
            }
            let insert = |token: &'static [u8; 32], created: i64, admin: &'static str| {
                let sessions = &sessions;
                async move {
                    sessions
                        .insert_session(store_postgres::NewSessionRow {
                            token_hash: token,
                            created_at_ms: created,
                            expires_at_ms: 1_000_000,
                            absolute_expires_at_ms: Some(9_000_000),
                            idle_ttl_ms: Some(1_000_000),
                            admin_id: Some(admin),
                            ip: None,
                            user_agent: None,
                        })
                        .await
                        .expect("insert session");
                }
            };
            let (current, other, theirs) = (&[21_u8; 32], &[22_u8; 32], &[23_u8; 32]);
            insert(current, 10, "own-1").await;
            insert(other, 20, "own-1").await;
            insert(theirs, 30, "own-2").await;

            // Listing is scoped and newest-first, and created_at round-trips to ms.
            let mine = sessions
                .list_admin_sessions("own-1", 100)
                .await
                .expect("list");
            assert_eq!(mine.len(), 2, "own-1 sees only their own sessions");
            assert_eq!(
                mine.first().expect("a first session").created_at_ms,
                20,
                "newest first"
            );
            assert_eq!(mine.get(1).expect("a second session").created_at_ms, 10);

            // Revocation is scoped: own-1 cannot revoke own-2's session.
            assert!(
                !sessions
                    .delete_admin_session("own-1", theirs)
                    .await
                    .expect("revoke"),
                "an admin cannot revoke another's session"
            );
            // "Revoke others" keeps the current session and drops own-1's other one.
            assert_eq!(
                sessions
                    .delete_other_admin_sessions("own-1", current)
                    .await
                    .expect("revoke others"),
                1,
            );
            let remaining = sessions
                .list_admin_sessions("own-1", 100)
                .await
                .expect("list");
            assert_eq!(remaining.len(), 1);
            assert_eq!(
                remaining.first().expect("one remaining").token_hash,
                current.to_vec(),
                "the current session survives"
            );
            assert_eq!(
                sessions
                    .list_admin_sessions("own-2", 100)
                    .await
                    .expect("list")
                    .len(),
                1,
                "the other admin's session is untouched"
            );
        });
    }

    /// TOTP re-enrolment replaces the secret and resets the last-used step ([ADR-0067] slice 6).
    #[test]
    fn totp_re_enrolment_replaces_the_secret_and_resets_the_step() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let admin = store.admin();
            admin
                .insert_credential("$argon2id$phc", b"old-secret-value")
                .await
                .expect("provision");
            admin.advance_totp_step(42).await.expect("advance the step");

            admin
                .rotate_totp_secret(b"a-freshly-enrolled-secret")
                .await
                .expect("rotate");
            let row = admin
                .fetch_credential()
                .await
                .expect("fetch")
                .expect("a credential is present");
            assert_eq!(row.totp_secret, b"a-freshly-enrolled-secret".to_vec());
            assert_eq!(
                row.last_used_totp_step, None,
                "a fresh secret resets the last-used step"
            );
        });
    }

    /// Recovery codes store, count, burn single-use, and regenerate — scoped to the admin
    /// ([ADR-0067] slice 6).
    #[test]
    fn recovery_codes_store_consume_count_and_regenerate() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let admin = store.admin();
            admin
                .insert_admin_user("rec-1", "rec@x.test", "R", "owner", "active", "$phc", b"s")
                .await
                .expect("seed admin");
            let (h1, h2, h3) = (vec![1_u8; 32], vec![2_u8; 32], vec![3_u8; 32]);
            admin
                .replace_recovery_codes(
                    "rec-1",
                    &[("c1".to_owned(), h1.clone()), ("c2".to_owned(), h2.clone())],
                )
                .await
                .expect("store");
            assert_eq!(admin.count_recovery_codes("rec-1").await.expect("count"), 2);

            // Single-use: the first match spends the code, a replay matches nothing.
            assert!(
                admin
                    .consume_recovery_code("rec-1", &h1, 1000)
                    .await
                    .expect("consume")
            );
            assert!(
                !admin
                    .consume_recovery_code("rec-1", &h1, 1000)
                    .await
                    .expect("consume"),
                "a spent code cannot be reused"
            );
            assert_eq!(admin.count_recovery_codes("rec-1").await.expect("count"), 1);

            // Regenerating replaces the set: the previous unused code is gone, the new one works.
            admin
                .replace_recovery_codes("rec-1", &[("c3".to_owned(), h3.clone())])
                .await
                .expect("regenerate");
            assert_eq!(admin.count_recovery_codes("rec-1").await.expect("count"), 1);
            assert!(
                !admin
                    .consume_recovery_code("rec-1", &h2, 1000)
                    .await
                    .expect("consume"),
                "a code from the replaced set no longer matches"
            );
            assert!(
                admin
                    .consume_recovery_code("rec-1", &h3, 1000)
                    .await
                    .expect("consume")
            );
        });
    }
}

// ---------------------------------------------------------------------------
// The scoped per-tenant API-key store (ADR-0037).
// ---------------------------------------------------------------------------

mod api_keys_store {
    use super::{block_on, prepared};

    /// Insert reads back by id, lists by tenant without the secret, and revoke is idempotent.
    #[test]
    fn insert_fetch_list_and_revoke_round_trip() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let keys = store.api_keys();
            let hash: &[u8] = &[3_u8; 32];
            // Two names, one of which this build does not know: the adapter stores whatever it is
            // given and the domain drops unknown names on the way back out (`Scope::from_wire`), so
            // a key issued by a newer cloud round-trips through an older one without loss.
            let scopes = vec!["read_rollups".to_owned(), "read_config".to_owned()];

            keys.insert(
                "KEY0000000000000000000001",
                "TENANT000000000000000000AA",
                Some("STORE0000000000000000000A"),
                hash,
                &scopes,
                None,
            )
            .await
            .expect("insert the key");
            // A second key in the same tenant, bound to no store — the tenant-wide integration key
            // (S1). Both shapes must round-trip, because the guard that reads `store_id` treats
            // `NULL` and a store id as different authorities, not as a present-or-missing detail.
            keys.insert(
                "KEY0000000000000000000002",
                "TENANT000000000000000000AA",
                None,
                hash,
                &scopes,
                None,
            )
            .await
            .expect("insert the tenant-wide key");

            let row = keys
                .fetch("KEY0000000000000000000001")
                .await
                .expect("fetch")
                .expect("the inserted key is present");
            assert_eq!(row.tenant_id, "TENANT000000000000000000AA");
            assert_eq!(
                row.store_id.as_deref(),
                Some("STORE0000000000000000000A"),
                "a store's key round-trips the store it is bound to"
            );
            assert_eq!(row.secret_hash, vec![3_u8; 32]);
            assert!(!row.revoked, "a fresh key is live");
            assert_eq!(row.expires_at_ms, None);

            let tenant_wide = keys
                .fetch("KEY0000000000000000000002")
                .await
                .expect("fetch")
                .expect("the tenant-wide key is present");
            assert_eq!(
                tenant_wide.store_id, None,
                "a tenant-wide key reads back bound to no store"
            );

            let listed = keys
                .list_for_tenant("TENANT000000000000000000AA")
                .await
                .expect("list");
            assert_eq!(listed.len(), 2);
            let only = listed
                .iter()
                .find(|key| key.id == "KEY0000000000000000000001")
                .expect("the store-bound key is listed");
            assert_eq!(only.scopes, scopes, "the granted scopes are listed");
            assert_eq!(
                only.store_id.as_deref(),
                Some("STORE0000000000000000000A"),
                "the listing says which store a key belongs to"
            );

            // Another tenant sees nothing.
            let other = keys
                .list_for_tenant("TENANT000000000000000000BB")
                .await
                .expect("list other");
            assert!(other.is_empty(), "the listing is scoped to the tenant");

            // Revoke: the first call changes a row, the second is a no-op, and the key reads revoked.
            assert!(
                keys.revoke("KEY0000000000000000000001")
                    .await
                    .expect("revoke")
            );
            assert!(
                !keys
                    .revoke("KEY0000000000000000000001")
                    .await
                    .expect("revoke again"),
                "revoking an already-revoked key changes nothing"
            );
            let row = keys
                .fetch("KEY0000000000000000000001")
                .await
                .expect("fetch")
                .expect("still present");
            assert!(row.revoked, "the key now reads revoked");
        });
    }
}

// ---------------------------------------------------------------------------
// The activation-code and device-credential store (ADR-0050).
// ---------------------------------------------------------------------------

mod activation_codes {
    use super::{block_on, prepared};

    const TENANT: &str = "TENANT000000000000000000AA";
    const STORE: &str = "STORE0000000000000000000BB";

    /// Issue reads back as `issued`; the exchange consumes it and mints a credential atomically; a
    /// replay is refused (single-use); and revoke cancels a slot's still-issued code.
    #[test]
    fn issue_redeem_replay_and_revoke_round_trip() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let codes = store.activation_codes();
            let hash: &[u8] = &[7_u8; 32];
            let device = "DEVICE000000000000000000CC";

            codes
                .issue(hash, TENANT, STORE, device)
                .await
                .expect("issue the code");

            let row = codes
                .lookup(hash)
                .await
                .expect("lookup")
                .expect("the issued code is present");
            assert_eq!(row.status, "issued");
            assert_eq!(row.tenant_id, TENANT);
            assert_eq!(row.device_id, device);

            // The first redemption wins: the code is consumed and the credential provisioned together.
            let secret_hash: &[u8] = &[9_u8; 32];
            assert!(
                codes
                    .consume_and_provision(hash, "CRED00000000000000000000DD", secret_hash)
                    .await
                    .expect("consume"),
                "the issued code is redeemed"
            );

            // It now reads redeemed, and a second attempt changes nothing — single-use.
            let row = codes
                .lookup(hash)
                .await
                .expect("lookup")
                .expect("still present");
            assert_eq!(row.status, "redeemed");
            assert!(
                !codes
                    .consume_and_provision(hash, "CRED00000000000000000000EE", secret_hash)
                    .await
                    .expect("second consume"),
                "a spent code cannot be redeemed twice"
            );

            // The credential landed for the code's slot, exactly once (the atomic mint, not two).
            let minted: i64 = admin
                .query_one(
                    "SELECT count(*) FROM device_credentials WHERE tenant_id = $1 AND device_id = $2",
                    &[&TENANT, &device],
                )
                .await
                .expect("count credentials")
                .get(0);
            assert_eq!(minted, 1, "exactly one credential was minted");

            // Revoke cancels a still-issued code for a slot and is idempotent.
            let other_device = "DEVICE000000000000000000FF";
            codes
                .issue(&[1_u8; 32], TENANT, STORE, other_device)
                .await
                .expect("issue a second code");
            assert_eq!(
                codes
                    .revoke_slot(TENANT, STORE, other_device)
                    .await
                    .expect("revoke"),
                1,
                "the one issued code is cancelled"
            );
            assert_eq!(
                codes
                    .revoke_slot(TENANT, STORE, other_device)
                    .await
                    .expect("revoke again"),
                0,
                "nothing is left to revoke"
            );
            let revoked = codes
                .lookup(&[1_u8; 32])
                .await
                .expect("lookup")
                .expect("present");
            assert_eq!(revoked.status, "revoked");
        });
    }
}

// ---------------------------------------------------------------------------
// The four-level config-tree store (ADR-0033).
// ---------------------------------------------------------------------------

mod config_tree_store {
    use super::{block_on, prepared};
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

    /// A heartbeat advances only `last_seen_at`, preserving the held version and last-config-pull
    /// instant a prior pull recorded; a heartbeat before any pull creates the row with those NULL
    /// ([ADR-0068] slice 2).
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
                .record_heartbeat(tenant, store_id, 5000)
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
                .record_heartbeat(tenant, fresh, 2000)
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
}

// ---------------------------------------------------------------------------
// The fleet read model: identity + liveness + config drift + relay backlog (ADR-0068 slice 3).
// ---------------------------------------------------------------------------

mod fleet_store {
    use super::{block_on, prepared};
    use pos_proto::{StoreId, TenantId, Ulid};

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
}

// ---------------------------------------------------------------------------
// Background-task health: last-tick per loop, upserted (ADR-0068 slice 4).
// ---------------------------------------------------------------------------

mod task_health {
    use super::{block_on, prepared};

    /// A loop's tick records a row; a second tick upserts it in place (one row, latest instant and
    /// detail win); and the detail JSON round-trips through the `jsonb` column.
    #[test]
    fn records_and_upserts_a_loop_tick() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let health = store.task_health();

            // First tick: the projector at t=1000ms, folding four stores.
            health
                .record(
                    "rollup_projector",
                    1000,
                    r#"{"ok":true,"interval_secs":30,"folded":4}"#,
                )
                .await
                .expect("record the first tick");
            let rows = health.fetch_all().await.expect("fetch all");
            assert_eq!(rows.len(), 1, "one loop, one row");
            let first = rows.first().expect("the recorded row");
            assert_eq!(first.task, "rollup_projector");
            assert_eq!(first.last_tick_at_ms, 1000);
            let detail: serde_json::Value =
                serde_json::from_str(&first.detail_json).expect("detail is valid json");
            assert_eq!(
                detail.get("folded").and_then(serde_json::Value::as_i64),
                Some(4),
                "the detail round-trips through jsonb"
            );

            // A second tick for the same loop upserts in place: still one row, advanced instant.
            health
                .record(
                    "rollup_projector",
                    5000,
                    r#"{"ok":true,"interval_secs":30,"folded":0}"#,
                )
                .await
                .expect("record the second tick");
            let count: i64 = admin
                .query_one("SELECT count(*) FROM task_health", &[])
                .await
                .expect("count")
                .get(0);
            assert_eq!(count, 1, "the second tick upserts rather than duplicating");
            let rows = health.fetch_all().await.expect("fetch all");
            assert_eq!(
                rows.first().expect("the upserted row").last_tick_at_ms,
                5000,
                "the instant advanced"
            );

            // A second, distinct loop gets its own row; fetch_all orders most-recently-ticked first.
            health
                .record("retention", 9000, r#"{"ok":true,"interval_secs":86400}"#)
                .await
                .expect("record a second loop");
            let rows = health.fetch_all().await.expect("fetch all");
            assert_eq!(rows.len(), 2, "two distinct loops, two rows");
            assert_eq!(
                rows.first().expect("the newest row").task,
                "retention",
                "the most recently ticked loop sorts first"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Operational alerts: open→resolved lifecycle, partial-unique dedup (ADR-0073, Track O2).
// ---------------------------------------------------------------------------

mod alerts {
    use super::{block_on, prepared};

    /// The full lifecycle: open (tenant-scoped and server-wide), refresh in place (the partial unique
    /// index dedups the one *open* alert per key), acknowledge, resolve (drops from active, stays in
    /// recent), and reopen the same key past a resolved row.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end lifecycle: it opens a tenant-scoped and a server-wide alert, \
                  refreshes each in place through the partial-unique dedup, acknowledges, resolves \
                  (drops from active, stays in recent), and reopens the same key past the resolved \
                  row — splitting it would duplicate the multi-alert setup"
    )]
    fn upserts_refreshes_resolves_and_lists_alerts() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let alerts = store.alerts();

            // A tenant-scoped store-offline alert and a server-wide (NULL tenant) JetStream alert.
            alerts
                .upsert(
                    "alert-1",
                    Some("tenant-a"),
                    "store_offline",
                    "store-x",
                    "warning",
                    "offline 6m",
                    r#"{"minutes_offline":6}"#,
                    1000,
                    1000,
                )
                .await
                .expect("open the store alert");
            alerts
                .upsert(
                    "alert-2",
                    None,
                    "jetstream_capacity",
                    "",
                    "critical",
                    "at 85%",
                    r#"{"threshold_percent":80}"#,
                    1000,
                    1000,
                )
                .await
                .expect("open the server-wide alert");
            assert_eq!(alerts.list_active().await.expect("active").len(), 2);

            // A second upsert of the same (tenant, kind, dedup_key) refreshes in place: still two
            // rows, the original id and first_seen kept, last_seen and detail advanced.
            alerts
                .upsert(
                    "alert-3",
                    Some("tenant-a"),
                    "store_offline",
                    "store-x",
                    "warning",
                    "offline 12m",
                    r#"{"minutes_offline":12}"#,
                    9999,
                    5000,
                )
                .await
                .expect("refresh the store alert");
            let count: i64 = admin
                .query_one("SELECT count(*) FROM alerts", &[])
                .await
                .expect("count")
                .get(0);
            assert_eq!(count, 2, "a refresh does not duplicate");
            let active = alerts.list_active().await.expect("active");
            let store_alert = active
                .iter()
                .find(|a| a.kind == "store_offline")
                .expect("the store alert");
            assert_eq!(
                store_alert.id, "alert-1",
                "the original id is kept on refresh"
            );
            assert_eq!(store_alert.first_seen_at_ms, 1000, "first_seen is kept");
            assert_eq!(store_alert.last_seen_at_ms, 5000, "last_seen advanced");

            // Acknowledge keeps it active; resolve drops it from the active list but keeps it in the
            // recent history with both timestamps.
            alerts
                .acknowledge("alert-1", 6000)
                .await
                .expect("acknowledge");
            alerts.resolve("alert-1", 7000).await.expect("resolve");
            let active = alerts.list_active().await.expect("active after resolve");
            assert_eq!(active.len(), 1, "the resolved alert leaves the active list");
            assert_eq!(
                active.first().expect("the remaining active alert").kind,
                "jetstream_capacity"
            );
            let recent = alerts.list_recent(10).await.expect("recent");
            assert_eq!(recent.len(), 2, "the resolved alert stays in history");
            let resolved = recent
                .iter()
                .find(|a| a.id == "alert-1")
                .expect("the resolved alert");
            assert_eq!(resolved.resolved_at_ms, Some(7000));
            assert_eq!(resolved.acknowledged_at_ms, Some(6000));

            // The same condition can open a fresh alert past the resolved one — the partial unique
            // index constrains only unresolved rows.
            alerts
                .upsert(
                    "alert-4",
                    Some("tenant-a"),
                    "store_offline",
                    "store-x",
                    "warning",
                    "offline again",
                    "{}",
                    12000,
                    12000,
                )
                .await
                .expect("reopen the same key");
            let count: i64 = admin
                .query_one("SELECT count(*) FROM alerts", &[])
                .await
                .expect("count")
                .get(0);
            assert_eq!(
                count, 3,
                "a resolved alert does not block a fresh open of the same key"
            );
            assert_eq!(alerts.list_active().await.expect("active").len(), 2);
        });
    }
}

// ---------------------------------------------------------------------------
// The per-(tax class × channel) tax rate table: wholesale replace, tenant-scoped (ADR-0074, M4).
// ---------------------------------------------------------------------------

mod tax_rates {
    use store_postgres::{RowUpdate, TaxRateRow};

    use super::{TENANT_A, TENANT_B, block_on, prepared};

    fn row(channel: &str, rate_bps: i32) -> TaxRateRow {
        TaxRateRow {
            tax_class_id: "class-standard".to_owned(),
            sales_channel: channel.to_owned(),
            rate_bps,
        }
    }

    /// The version an applied save left the table at, or `None` if the store refused it — so a call
    /// site can `.expect` inside its own test, where the lint config allows it.
    fn applied(outcome: RowUpdate) -> Option<String> {
        match outcome {
            RowUpdate::Updated(version) => Some(version),
            RowUpdate::VersionMismatch | RowUpdate::NotFound => None,
        }
    }

    /// A save replaces the tenant's whole table (not append), reads back class-then-channel ordered,
    /// carries the version it was saved at, and never touches a neighbour tenant's rows.
    #[test]
    fn replaces_wholesale_under_its_version_and_stays_tenant_scoped() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let rates = store.tax_rates();

            assert!(
                rates
                    .fetch(TENANT_A)
                    .await
                    .expect("fetch before any save")
                    .1
                    .is_none(),
                "a tenant that has never saved rates has no version"
            );

            let neighbour = vec![row("SALES_CHANNEL_DINE_IN", 500)];
            applied(
                rates
                    .replace(TENANT_B, &neighbour, None)
                    .await
                    .expect("set neighbour"),
            )
            .expect("the save applies");

            let first = vec![
                row("SALES_CHANNEL_DINE_IN", 1000),
                row("SALES_CHANNEL_TAKEAWAY", 800),
            ];
            let created = applied(
                rates
                    .replace(TENANT_A, &first, None)
                    .await
                    .expect("set ours"),
            )
            .expect("the save applies");
            let (listed, version) = rates.fetch(TENANT_A).await.expect("fetch ours");
            assert_eq!(listed.len(), 2);
            assert_eq!(
                listed.first().expect("first row").sales_channel,
                "SALES_CHANNEL_DINE_IN",
                "rows read back class-then-channel ordered"
            );
            let version = version.expect("a saved table has a version");
            assert_eq!(
                version, created,
                "the version a save returns is the one the next fetch reads"
            );

            // A second save at that version replaces rather than appends, and moves the version.
            let second = vec![row("SALES_CHANNEL_DINE_IN", 1000)];
            let moved = applied(
                rates
                    .replace(TENANT_A, &second, Some(&version))
                    .await
                    .expect("replace ours"),
            )
            .expect("the save applies");
            let (replaced, _) = rates.fetch(TENANT_A).await.expect("fetch replaced");
            assert_eq!(replaced, second, "the whole table is replaced");
            assert_ne!(
                moved, version,
                "xmin on the version row must move, or the next save would be unguarded"
            );

            // The neighbour is untouched — the version is per tenant, like the rows it guards.
            let (neighbour_after, _) = rates.fetch(TENANT_B).await.expect("fetch neighbour");
            assert_eq!(neighbour_after, neighbour);
        });
    }

    /// The four ways a save is refused, and the proof that none of them touched the rate rows.
    ///
    /// This is the case the `xmin`-on-the-row scheme cannot cover, and the reason migration 0039
    /// exists: a replace deletes and reinserts every rate row, so no rate row's version survives one
    /// to be compared against. The version row is what does.
    #[test]
    fn a_stale_garbled_or_already_taken_save_is_refused_and_writes_nothing() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let rates = store.tax_rates();
            let kept = vec![row("SALES_CHANNEL_DINE_IN", 1000)];
            let clobber = vec![row("SALES_CHANNEL_DINE_IN", 9900)];

            // Naming a version for a tenant that has never saved is an absence, not a conflict.
            assert_eq!(
                rates
                    .replace(TENANT_A, &clobber, Some("1"))
                    .await
                    .expect("the probe"),
                RowUpdate::NotFound
            );

            let stale = applied(
                rates
                    .replace(TENANT_A, &kept, None)
                    .await
                    .expect("the first save"),
            )
            .expect("the save applies");
            applied(
                rates
                    .replace(TENANT_A, &kept, Some(&stale))
                    .await
                    .expect("a second save"),
            )
            .expect("the save applies");

            // Replaying a version the table has moved past is the lost update this refuses.
            assert_eq!(
                rates
                    .replace(TENANT_A, &clobber, Some(&stale))
                    .await
                    .expect("the stale save"),
                RowUpdate::VersionMismatch
            );

            // A garbled tag is a mismatch too, not a database error — the comparison is on
            // `xmin::text`, because casting caller text to `xid` would raise instead of refusing.
            assert_eq!(
                rates
                    .replace(TENANT_A, &clobber, Some("not-a-transaction-id"))
                    .await
                    .expect("the comparison must not raise"),
                RowUpdate::VersionMismatch
            );

            // Claiming "nothing saved yet" about a table that has been saved: refused, not upserted.
            assert_eq!(
                rates
                    .replace(TENANT_A, &clobber, None)
                    .await
                    .expect("the create path"),
                RowUpdate::VersionMismatch
            );

            let (survived, _) = rates
                .fetch(TENANT_A)
                .await
                .expect("fetch after the refusals");
            assert_eq!(
                survived, kept,
                "no refused save deleted or reinserted a rate row"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// The translation grid: one jsonb row per tenant, versioned by its own xmin (ADR-0095).
// ---------------------------------------------------------------------------

mod translations {
    use store_postgres::RowUpdate;

    use super::{TENANT_A, TENANT_B, block_on, prepared};

    /// The grid saves under its version and refuses a save made against a version it has moved past.
    ///
    /// The counterpart to the tax-rate case above, and the reason that one needed a migration and
    /// this one did not: the grid is *one row*, so a save updates it in place and its own `xmin`
    /// moves. ADR-0095 had booked both as needing a version table; measuring the schema showed only
    /// one does.
    #[test]
    fn the_grid_saves_under_its_version_and_refuses_a_stale_one() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let grids = store.translations();
            let first = r#"{"menu.pho":{"en":"Pho"}}"#;
            let second = r#"{"menu.pho":{"en":"Pho noodles"}}"#;
            let clobber = r#"{"menu.pho":{"en":"Clobbered"}}"#;

            assert!(
                grids
                    .load_grid(TENANT_A)
                    .await
                    .expect("load before any save")
                    .is_none(),
                "no row before the first save"
            );

            let created = match grids
                .save_grid(TENANT_A, first, None)
                .await
                .expect("the create")
            {
                RowUpdate::Updated(version) => version,
                other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                    panic!("expected the create to apply, got {other:?}")
                }
            };
            let (loaded, version) = grids
                .load_grid(TENANT_A)
                .await
                .expect("load")
                .expect("present");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&loaded).expect("valid json"),
                serde_json::from_str::<serde_json::Value>(first).expect("valid json"),
                "the grid round-trips (compared as JSON, since jsonb reorders keys)"
            );
            assert_eq!(version, created);

            let moved = match grids
                .save_grid(TENANT_A, second, Some(&version))
                .await
                .expect("the replace")
            {
                RowUpdate::Updated(version) => version,
                other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                    panic!("expected the replace to apply, got {other:?}")
                }
            };
            assert_ne!(moved, version, "xmin must move on every UPDATE");

            // Replaying the old version, a garbled tag, and a false "nothing here yet" are all
            // refused; a version-gated save against a tenant with no row at all is a NotFound.
            for (expected, outcome) in [
                (Some(version.as_str()), RowUpdate::VersionMismatch),
                (Some("not-a-transaction-id"), RowUpdate::VersionMismatch),
                (None, RowUpdate::VersionMismatch),
            ] {
                assert_eq!(
                    grids
                        .save_grid(TENANT_A, clobber, expected)
                        .await
                        .expect("the comparison must not raise"),
                    outcome
                );
            }
            assert_eq!(
                grids
                    .save_grid(TENANT_B, clobber, Some(&version))
                    .await
                    .expect("the probe"),
                RowUpdate::NotFound
            );

            let (survived, _) = grids
                .load_grid(TENANT_A)
                .await
                .expect("load")
                .expect("present");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&survived).expect("valid json"),
                serde_json::from_str::<serde_json::Value>(second).expect("valid json"),
                "no refused save changed the stored grid"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Campaign authoring: jsonb insert/list/get/update-at/delete, id ordering, tenant isolation
// (ADR-0077, ADR-0095).
// ---------------------------------------------------------------------------

mod campaigns {
    use store_postgres::RowUpdate;

    use super::{TENANT_A, TENANT_B, block_on, prepared};

    fn doc(name: &str) -> String {
        format!("{{\"name\":\"{name}\"}}")
    }

    /// Per-campaign insert/get/delete over the jsonb column: rows read back id-ordered and carrying
    /// the version they are at, an insert at a taken id writes nothing, a tenant cannot read
    /// another's campaign by id, and a neighbour tenant's rows are never touched.
    ///
    /// The insert/update split is [ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md):
    /// what this asserted before was the `ON CONFLICT DO UPDATE` that made "add a campaign" and
    /// "replace a campaign" the same statement, so a second admin creating at an id another had
    /// just used silently replaced their document.
    #[test]
    fn an_insert_writes_nothing_when_the_id_is_taken_and_reads_stay_tenant_scoped() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let campaigns = store.campaigns();

            // A neighbour's campaign must survive our writes.
            campaigns
                .insert(TENANT_B, "camp-z", &doc("Neighbour"))
                .await
                .expect("neighbour")
                .expect("the neighbour's id was free");

            // Insert out of id order to prove the read sorts.
            campaigns
                .insert(TENANT_A, "camp-2", &doc("Dinner"))
                .await
                .expect("create 2")
                .expect("free");
            let at_create = campaigns
                .insert(TENANT_A, "camp-1", &doc("Lunch"))
                .await
                .expect("create 1")
                .expect("free");
            let listed = campaigns.fetch(TENANT_A).await.expect("fetch ours");
            assert_eq!(listed.len(), 2);
            let first = listed.first().expect("first row");
            assert_eq!(first.campaign_id, "camp-1", "rows read back id-ordered");
            assert_eq!(
                first.version, at_create,
                "the read carries the version the insert minted, which is the only way a caller \
                 that did not perform the insert can obtain it"
            );

            // An insert at a taken id writes nothing and says so by returning no version — one round
            // trip, so there is no window between a check and the write.
            assert!(
                campaigns
                    .insert(TENANT_A, "camp-1", &doc("Lunch (renamed)"))
                    .await
                    .expect("the conflict must not raise")
                    .is_none(),
                "the id is taken"
            );
            let unchanged = campaigns
                .fetch_one(TENANT_A, "camp-1")
                .await
                .expect("fetch one")
                .expect("present");
            assert!(
                unchanged.campaign_json.contains("Lunch"),
                "and the document it refused to overwrite is untouched"
            );
            assert!(!unchanged.campaign_json.contains("renamed"));

            // A tenant cannot read another tenant's campaign by id.
            assert!(
                campaigns
                    .fetch_one(TENANT_A, "camp-z")
                    .await
                    .expect("cross-tenant read")
                    .is_none(),
                "camp-z belongs to the neighbour, not us"
            );

            campaigns.delete(TENANT_A, "camp-1").await.expect("delete");
            assert_eq!(
                campaigns
                    .fetch(TENANT_A)
                    .await
                    .expect("fetch after delete")
                    .len(),
                1
            );
            // The id is free again, so an insert at it succeeds.
            assert!(
                campaigns
                    .insert(TENANT_A, "camp-1", &doc("Lunch again"))
                    .await
                    .expect("insert after the delete")
                    .is_some()
            );
            assert_eq!(
                campaigns
                    .fetch(TENANT_B)
                    .await
                    .expect("fetch neighbour")
                    .len(),
                1,
                "the neighbour is untouched throughout"
            );
        });
    }

    /// A rename applies at the version the read carried, refuses a spent one, and answers `NotFound`
    /// for an id that is not there — three answers, because the caller's next move differs for each.
    #[test]
    fn an_update_applies_only_at_the_version_the_read_carried() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let campaigns = store.campaigns();
            let at_create = campaigns
                .insert(TENANT_A, "camp-1", &doc("Lunch"))
                .await
                .expect("create")
                .expect("the id was free");

            let at_update = match campaigns
                .update_at(TENANT_A, "camp-1", &doc("Lunch (renamed)"), &at_create)
                .await
                .expect("the update")
            {
                RowUpdate::Updated(version) => version,
                other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                    panic!("expected the update to apply, got {other:?}")
                }
            };
            assert_ne!(at_update, at_create, "the version moves on every write");
            let one = campaigns
                .fetch_one(TENANT_A, "camp-1")
                .await
                .expect("fetch one")
                .expect("present");
            assert!(
                one.campaign_json.contains("Lunch (renamed)"),
                "the replaced document is what reads back"
            );
            assert_eq!(one.version, at_update);
            assert_eq!(
                campaigns.fetch(TENANT_A).await.expect("fetch").len(),
                1,
                "an update does not add a row"
            );

            assert_eq!(
                campaigns
                    .update_at(TENANT_A, "camp-1", &doc("Lunch (again)"), &at_create)
                    .await
                    .expect("the comparison must not raise"),
                RowUpdate::VersionMismatch,
                "replaying a spent version is the lost update"
            );
            assert_eq!(
                campaigns
                    .update_at(TENANT_A, "camp-none", &doc("Ghost"), &at_update)
                    .await
                    .expect("the comparison must not raise"),
                RowUpdate::NotFound
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Voucher instances: batch insert, code uniqueness per tenant, list-by-campaign, isolation (ADR-0077).
// ---------------------------------------------------------------------------

mod vouchers {
    use store_postgres::NewVoucherRow;

    use super::{TENANT_A, TENANT_B, block_on, prepared};

    /// A batch inserts atomically, lists by campaign newest-first, rejects a duplicate code within the
    /// tenant, and lets a neighbour tenant reuse the same code (codes are unique per tenant).
    #[test]
    fn mint_batch_list_and_code_uniqueness() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let vouchers = store.vouchers();

            let batch = [
                NewVoucherRow {
                    voucher_id: "v-1",
                    campaign_id: "camp-1",
                    code: "ALPHA",
                },
                NewVoucherRow {
                    voucher_id: "v-2",
                    campaign_id: "camp-1",
                    code: "BRAVO",
                },
            ];
            vouchers.insert_batch(TENANT_A, &batch).await.expect("mint");

            let listed = vouchers
                .list_by_campaign(TENANT_A, "camp-1")
                .await
                .expect("list");
            assert_eq!(listed.len(), 2);
            assert!(listed.iter().all(|row| row.status == "ACTIVE"));
            assert!(listed.iter().any(|row| row.code == "ALPHA"));

            // A neighbour tenant may reuse the same code — codes are unique per tenant, not globally.
            vouchers
                .insert_batch(
                    TENANT_B,
                    &[NewVoucherRow {
                        voucher_id: "v-1",
                        campaign_id: "camp-1",
                        code: "ALPHA",
                    }],
                )
                .await
                .expect("neighbour reuse");

            // A duplicate code within the tenant violates the unique constraint and fails the batch.
            let collision = vouchers
                .insert_batch(
                    TENANT_A,
                    &[NewVoucherRow {
                        voucher_id: "v-9",
                        campaign_id: "camp-1",
                        code: "ALPHA",
                    }],
                )
                .await;
            assert!(collision.is_err(), "a duplicate code is rejected");

            // A different campaign in the same tenant lists separately.
            assert!(
                vouchers
                    .list_by_campaign(TENANT_A, "camp-2")
                    .await
                    .expect("list other")
                    .is_empty()
            );
        });
    }

    /// Mints `count` codes for one campaign of one tenant.
    ///
    /// Codes are `CODE0000`-style so they sort predictably, and the ids likewise, because the read's
    /// order is `created_at DESC, voucher_id DESC` and a batch inserted in one statement shares a
    /// `created_at` — so `voucher_id` is what actually breaks the tie, and a test that could not tell
    /// the two apart would pass on a query that dropped the second sort key.
    async fn mint(
        vouchers: &store_postgres::PostgresVouchers,
        tenant: &str,
        campaign: &str,
        count: u32,
    ) {
        let ids: Vec<(String, String)> = (0..count)
            .map(|n| (format!("v-{n:04}"), format!("CODE{n:04}")))
            .collect();
        let batch: Vec<NewVoucherRow<'_>> = ids
            .iter()
            .map(|(voucher_id, code)| NewVoucherRow {
                voucher_id,
                campaign_id: campaign,
                code,
            })
            .collect();
        vouchers
            .insert_batch(tenant, &batch)
            .await
            .expect("mint the batch");
    }

    /// A page carries its own window and the size of the whole set, and consecutive pages partition
    /// that set without overlap or gaps (ADR-0098 slice B3-1).
    ///
    /// This is the half only real PostgreSQL can answer: `count(*) OVER()` is a window function whose
    /// value depends on the frame the planner builds, and `LIMIT`/`OFFSET` interact with the
    /// `ORDER BY` in ways a `Vec::skip().take()` fake cannot reproduce. A fake agreeing with itself
    /// proves nothing about the SQL.
    #[test]
    fn a_page_carries_the_window_and_the_total_and_pages_do_not_overlap() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let vouchers = store.vouchers();
            mint(&vouchers, TENANT_A, "camp-1", 25).await;

            let (first, total) = vouchers
                .list_by_campaign_page(TENANT_A, "camp-1", 10, 0)
                .await
                .expect("first page");
            assert_eq!(first.len(), 10, "the window is the limit");
            assert_eq!(total, 25, "the total is the set, not the window");

            let mut seen: Vec<String> = first.into_iter().map(|row| row.code).collect();
            for offset in [10, 20] {
                let (page, page_total) = vouchers
                    .list_by_campaign_page(TENANT_A, "camp-1", 10, offset)
                    .await
                    .expect("later page");
                assert_eq!(page_total, 25, "the total does not change as pages advance");
                seen.extend(page.into_iter().map(|row| row.code));
            }
            assert_eq!(seen.len(), 25, "three pages of ten cover twenty-five rows");
            let mut unique = seen.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), 25, "no code appears on two pages");
        });
    }

    /// The paged read orders the same way the unpaged one does, is tenant-scoped the same way, and
    /// answers a page past the end with no rows rather than an error.
    #[test]
    fn the_paged_read_agrees_with_the_unpaged_one_on_order_and_scope() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let vouchers = store.vouchers();
            mint(&vouchers, TENANT_A, "camp-1", 6).await;
            // A neighbour's codes for the same campaign id must not reach either read.
            mint(&vouchers, TENANT_B, "camp-1", 4).await;

            let unpaged: Vec<String> = vouchers
                .list_by_campaign(TENANT_A, "camp-1")
                .await
                .expect("unpaged")
                .into_iter()
                .map(|row| row.code)
                .collect();
            assert_eq!(unpaged.len(), 6, "only this tenant's codes");

            let (paged, total) = vouchers
                .list_by_campaign_page(TENANT_A, "camp-1", 6, 0)
                .await
                .expect("paged");
            assert_eq!(total, 6, "the count is tenant-scoped too");
            let paged: Vec<String> = paged.into_iter().map(|row| row.code).collect();
            assert_eq!(
                paged, unpaged,
                "a full-width page is the unpaged read, in the same order"
            );

            // A page past the end of the set: empty, but `total` still reports the whole set. The
            // window count rides on the rows and there are none, so the adapter falls back to a
            // second count — without it the pager would be told the campaign has no codes and
            // would offer no page to go back to.
            let (beyond, beyond_total) = vouchers
                .list_by_campaign_page(TENANT_A, "camp-1", 10, 100)
                .await
                .expect("a page past the end still reads");
            assert!(beyond.is_empty());
            assert_eq!(
                beyond_total, 6,
                "an empty window still reports the size of the set it is past the end of"
            );
        });
    }

    /// The index migration 0040 added covers this query's `ORDER BY`, so `LIMIT` stops the scan
    /// instead of truncating a sort of every matching row.
    ///
    /// Asserted through `EXPLAIN` rather than by timing, because a timing test on 25 rows would pass
    /// either way and a timing test large enough to fail would be slow and flaky. What this catches
    /// is the migration being dropped or the query's `ORDER BY` drifting away from the index it was
    /// built for — at which point the plan grows a `Sort` node and paging becomes a page-shaped scan.
    #[test]
    fn the_paged_query_is_served_by_the_index_and_not_by_a_sort() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let vouchers = store.vouchers();
            mint(&vouchers, TENANT_A, "camp-1", 40).await;
            // Without statistics the planner will pick a sequential scan on any table this small
            // regardless of the index, so the assertion would be about row count rather than about
            // the index. `ANALYZE` plus a disabled seqscan asks the question the test means: given
            // that it must use an index, is there one that already carries this order?
            admin
                .batch_execute("ANALYZE vouchers")
                .await
                .expect("analyze");

            // `EXPLAIN` through the admin client rather than a method on the adapter: a plan probe
            // is a test's business, and adding one to the production surface to satisfy a test is
            // how a diagnostic becomes an API.
            let plan = {
                admin
                    .batch_execute("SET enable_seqscan = off")
                    .await
                    .expect("prefer an index if one fits");
                let rows = admin
                    .query(
                        "EXPLAIN SELECT voucher_id FROM vouchers \
                         WHERE tenant_id = $1 AND campaign_id = $2 \
                         ORDER BY created_at DESC, voucher_id DESC LIMIT $3 OFFSET $4",
                        &[&TENANT_A, &"camp-1", &10_i64, &0_i64],
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
                plan.contains("vouchers_by_campaign_newest"),
                "the page should be served by the sort-carrying index; plan was:\n{plan}"
            );
            assert!(
                !plan.contains("Sort Key"),
                "an index that carries the order needs no sort step; plan was:\n{plan}"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Scheduled publishes: schedule, due-by-time, list-for-store, cancel, mark-applied (ADR-0077).
// ---------------------------------------------------------------------------

mod scheduled_publishes {
    use store_postgres::NewScheduledPublishRow;

    use super::{TENANT_A, block_on, prepared};

    const STORE: &str = "store-1";

    fn row(id: &str, effective_at_ms: i64) -> NewScheduledPublishRow<'_> {
        NewScheduledPublishRow {
            id,
            tenant_id: TENANT_A,
            store_id: STORE,
            node_key: "campaigns",
            node_value_json: "{\"campaigns\":[]}",
            effective_at_ms,
            created_by: "admin-1",
        }
    }

    /// A schedule inserts pending; `due` returns only ripe pending rows; `list_for_store` sees all
    /// pending; `cancel` and `mark_applied` withdraw a row from the pending/due sets.
    #[test]
    fn schedule_due_list_cancel_and_apply() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let scheduled = store.scheduled_publishes();

            scheduled
                .schedule(&row("sp-past", 1_000))
                .await
                .expect("past");
            scheduled
                .schedule(&row("sp-future", 9_999_999_999))
                .await
                .expect("future");

            // Only the ripe one is due at t=5000.
            let due = scheduled.due(5_000).await.expect("due");
            assert_eq!(due.len(), 1);
            let ripe = due.first().expect("row");
            assert_eq!(ripe.id, "sp-past");
            assert_eq!(ripe.node_key, "campaigns");
            assert_eq!(ripe.status, "PENDING");
            assert!(ripe.node_value_json.contains("campaigns"));

            // Both are pending for the store.
            assert_eq!(
                scheduled
                    .list_for_store(TENANT_A, STORE)
                    .await
                    .expect("list")
                    .len(),
                2
            );

            // Applying the past one drops it from due and records the version.
            scheduled
                .mark_applied("sp-past", "ver-1")
                .await
                .expect("apply");
            assert!(
                scheduled
                    .due(5_000)
                    .await
                    .expect("due after apply")
                    .is_empty()
            );

            // Cancelling the future one removes it from the store's pending list.
            assert!(
                scheduled
                    .cancel(TENANT_A, "sp-future")
                    .await
                    .expect("cancel")
            );
            assert!(
                scheduled
                    .list_for_store(TENANT_A, STORE)
                    .await
                    .expect("list after cancel")
                    .is_empty()
            );
            // Cancelling an unknown id changes nothing.
            assert!(
                !scheduled
                    .cancel(TENANT_A, "nope")
                    .await
                    .expect("cancel nope")
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Media renditions: bytea round-trip, single-rendition read, tenant isolation, delete (ADR-0075).
// ---------------------------------------------------------------------------

mod media {
    use core::fmt::Write as _;

    use super::{TENANT_A, TENANT_B, block_on, prepared};

    /// Stores `count` assets for one tenant, each in its own transaction, with ids that sort the
    /// same way whichever column the read leans on.
    async fn upload(media: &store_postgres::PostgresMedia, tenant: &str, count: u32) {
        for index in 0..count {
            media
                .insert(
                    &format!("{tenant}-asset-{index:04}"),
                    tenant,
                    "image/jpeg",
                    &[0x01],
                    &[0x02, 0x03],
                    2,
                )
                .await
                .expect("insert an asset");
        }
    }

    #[test]
    fn stores_reads_one_rendition_lists_and_stays_tenant_scoped() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let media = store.media();

            let thumbnail = vec![0xFFu8, 0xD8, 0xFF, 0x00, 0x01];
            let detail = vec![0xFFu8, 0xD8, 0xFF, 0x10, 0x11, 0x12, 0x13];
            media
                .insert(
                    "asset-1",
                    TENANT_A,
                    "image/jpeg",
                    &thumbnail,
                    &detail,
                    i32::try_from(detail.len()).expect("fits"),
                )
                .await
                .expect("insert ours");
            // A neighbour tenant owns a different asset. Media ids are globally-unique ULIDs in
            // production (a minted id is never shared across tenants — `media_id` is the table's primary
            // key), so isolation is what the `tenant_id` predicate on every read enforces: a tenant
            // cannot reach another's asset by id, and a listing shows only its own.
            media
                .insert("asset-2", TENANT_B, "image/jpeg", &[0x01], &[0x02], 1)
                .await
                .expect("insert neighbour");

            // Each rendition round-trips its exact bytes.
            assert_eq!(
                media
                    .fetch_rendition(TENANT_A, "asset-1", false)
                    .await
                    .expect("thumbnail"),
                Some(thumbnail),
            );
            assert_eq!(
                media
                    .fetch_rendition(TENANT_A, "asset-1", true)
                    .await
                    .expect("detail"),
                Some(detail.clone()),
            );

            // A listing shows the size without the bytes, tenant-scoped.
            let listed = media.fetch_summaries(TENANT_A).await.expect("list ours");
            assert_eq!(listed.len(), 1);
            let row = listed.first().expect("row");
            assert_eq!(row.media_id, "asset-1");
            assert_eq!(row.content_type, "image/jpeg");
            assert_eq!(
                usize::try_from(row.detail_bytes).expect("non-negative"),
                detail.len()
            );

            // A tenant cannot read another tenant's asset by id — the read is `None`, not a leak —
            // while the neighbour reads its own, and neither can reach across.
            assert_eq!(
                media
                    .fetch_rendition(TENANT_B, "asset-1", true)
                    .await
                    .expect("neighbour read"),
                None,
                "a tenant cannot read another tenant's asset by id"
            );
            assert_eq!(
                media
                    .fetch_rendition(TENANT_B, "asset-2", true)
                    .await
                    .expect("neighbour detail"),
                Some(vec![0x02]),
                "the neighbour reads its own asset"
            );
            assert_eq!(
                media
                    .fetch_rendition(TENANT_A, "asset-2", true)
                    .await
                    .expect("cross read"),
                None,
                "and cannot reach the neighbour's asset"
            );

            // Delete removes only the named asset and reports it.
            assert!(media.remove(TENANT_A, "asset-1").await.expect("remove"));
            assert!(
                !media.remove(TENANT_A, "asset-1").await.expect("remove"),
                "removing an absent asset reports no row removed"
            );
            assert!(
                media
                    .fetch_summaries(TENANT_A)
                    .await
                    .expect("list")
                    .is_empty()
            );
            assert_eq!(
                media
                    .fetch_summaries(TENANT_B)
                    .await
                    .expect("neighbour")
                    .len(),
                1,
                "the neighbour's asset is untouched"
            );
        });
    }

    /// A page carries its own window and the size of the whole library, and consecutive pages
    /// partition that library without overlap or gaps (ADR-0098 slice B3-2).
    ///
    /// This is the half only real PostgreSQL can answer: `count(*) OVER()` is a window function
    /// whose value depends on the frame the planner builds, and `LIMIT`/`OFFSET` interact with the
    /// `ORDER BY` in ways a `Vec::skip().take()` fake cannot reproduce.
    #[test]
    fn a_page_of_the_library_carries_the_window_and_the_total_and_pages_do_not_overlap() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let media = store.media();
            upload(&media, TENANT_A, 25).await;

            let (first, total) = media
                .fetch_summaries_page(TENANT_A, 10, 0)
                .await
                .expect("first page");
            assert_eq!(first.len(), 10, "the window is the limit");
            assert_eq!(total, 25, "the total is the library, not the window");

            let mut seen: Vec<String> = first.into_iter().map(|row| row.media_id).collect();
            for offset in [10, 20] {
                let (page, page_total) = media
                    .fetch_summaries_page(TENANT_A, 10, offset)
                    .await
                    .expect("later page");
                assert_eq!(page_total, 25, "the total does not change as pages advance");
                seen.extend(page.into_iter().map(|row| row.media_id));
            }
            assert_eq!(
                seen.len(),
                25,
                "three pages of ten cover twenty-five assets"
            );
            let mut unique = seen.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), 25, "no asset appears on two pages");
        });
    }

    /// The paged read orders the same way the unpaged one does, is tenant-scoped the same way, and
    /// answers a page past the end with no rows rather than an error.
    #[test]
    fn the_paged_library_agrees_with_the_unpaged_one_on_order_and_scope() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let media = store.media();
            upload(&media, TENANT_A, 6).await;
            // A neighbour's assets must reach neither read nor the count.
            upload(&media, TENANT_B, 4).await;

            let unpaged: Vec<String> = media
                .fetch_summaries(TENANT_A)
                .await
                .expect("unpaged")
                .into_iter()
                .map(|row| row.media_id)
                .collect();
            assert_eq!(unpaged.len(), 6, "only this tenant's assets");

            let (paged, total) = media
                .fetch_summaries_page(TENANT_A, 6, 0)
                .await
                .expect("paged");
            assert_eq!(total, 6, "the count is tenant-scoped too");
            let paged: Vec<String> = paged.into_iter().map(|row| row.media_id).collect();
            assert_eq!(
                paged, unpaged,
                "a full-width page is the unpaged read, in the same order"
            );

            // A page past the end: empty, but `total` still reports the whole library. The window
            // count rides on the rows and there are none, so the adapter falls back to a second
            // count rather than claiming the tenant has no assets.
            let (beyond, beyond_total) = media
                .fetch_summaries_page(TENANT_A, 10, 100)
                .await
                .expect("a page past the end still reads");
            assert!(beyond.is_empty());
            assert_eq!(
                beyond_total, 6,
                "an empty window still reports the size of the set it is past the end of"
            );
        });
    }

    /// The premise ADR-0098 decision 9 rests on, measured here rather than trusted: `created_at`
    /// defaults to `now()`, which is *transaction* time, so assets written in one transaction share
    /// one timestamp exactly — and `created_at DESC` alone therefore orders nothing across them.
    ///
    /// The assertion on the tie is deterministic and is the point of the test. The partition check
    /// below it is the property that follows, but note honestly what it does and does not prove:
    /// with the tiebreaker dropped, PostgreSQL *may* return a stable order anyway for a given plan,
    /// so this test alone would not reliably catch that. What catches it is the pair — this test
    /// pins the premise, and `the_paged_library_query_is_served_by_the_index_and_not_by_a_sort`
    /// pins the index that carries the total order.
    #[test]
    fn a_batch_written_in_one_transaction_shares_one_timestamp_and_still_pages() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let media = store.media();
            // One transaction, six rows — the shape a bulk import or a seeded fixture produces.
            let mut inserts = String::new();
            for index in 0..6 {
                write!(
                    inserts,
                    "INSERT INTO media_assets \
                     (media_id, tenant_id, content_type, thumbnail, detail, detail_bytes) \
                     VALUES ('batch-{index:04}', '{TENANT_A}', 'image/jpeg', \
                     '\\x01'::bytea, '\\x0203'::bytea, 2);"
                )
                .expect("writing to a String cannot fail");
            }
            admin
                .batch_execute(&format!("BEGIN; {inserts} COMMIT;"))
                .await
                .expect("write the batch in one transaction");

            let distinct: i64 = admin
                .query_one(
                    "SELECT count(DISTINCT created_at) FROM media_assets WHERE tenant_id = $1",
                    &[&TENANT_A],
                )
                .await
                .expect("count the distinct timestamps")
                .get(0);
            assert_eq!(
                distinct, 1,
                "six rows in one transaction carry one created_at, not six close ones — \
                 which is why the read's ORDER BY needs media_id as a tiebreaker"
            );

            let mut seen = Vec::new();
            for offset in [0, 2, 4] {
                let (page, total) = media
                    .fetch_summaries_page(TENANT_A, 2, offset)
                    .await
                    .expect("page over the tied batch");
                assert_eq!(total, 6);
                seen.extend(page.into_iter().map(|row| row.media_id));
            }
            let mut unique = seen.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                unique.len(),
                6,
                "three pages of two cover the tied batch exactly once each; got {seen:?}"
            );
        });
    }

    /// The index migration 0041 added covers this query's `ORDER BY`, so `LIMIT` stops the scan
    /// instead of truncating a sort of the whole library.
    ///
    /// Asserted through `EXPLAIN` rather than by timing: a timing test on 40 rows would pass either
    /// way. What this catches is the migration being dropped or the read's `ORDER BY` drifting away
    /// from the index it was built for — including the tiebreaker, whose absence from the index is
    /// what would put a `Sort` node back above the scan.
    #[test]
    fn the_paged_library_query_is_served_by_the_index_and_not_by_a_sort() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let media = store.media();
            upload(&media, TENANT_A, 40).await;
            // Without statistics the planner picks a sequential scan on a table this small
            // whatever indexes exist, so the assertion would be about row count rather than about
            // the index. `ANALYZE` plus a disabled seqscan asks the question the test means.
            admin
                .batch_execute("ANALYZE media_assets")
                .await
                .expect("analyze");

            let plan = {
                admin
                    .batch_execute("SET enable_seqscan = off")
                    .await
                    .expect("prefer an index if one fits");
                let rows = admin
                    .query(
                        "EXPLAIN SELECT media_id FROM media_assets WHERE tenant_id = $1 \
                         ORDER BY created_at DESC, media_id DESC LIMIT $2 OFFSET $3",
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
                plan.contains("media_assets_by_tenant_newest"),
                "the page should be served by the sort-carrying index; plan was:\n{plan}"
            );
            assert!(
                !plan.contains("Sort Key"),
                "an index that carries the order needs no sort step; plan was:\n{plan}"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// The console audit trail: append-only, tenant-scoped, INSERT/SELECT-only (ADR-0069).
// ---------------------------------------------------------------------------

mod audit_log {
    use super::{block_on, prepared};
    use store_postgres::AuditOrder;

    /// Entries append and read back newest-first, scoped to their tenant (a NULL-tenant global entry
    /// shows only in the fleet-wide read); before/after round-trip through jsonb; and the grant is
    /// append-only — `app_tenant` has SELECT/INSERT but not UPDATE or DELETE ([ADR-0069]).
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end scenario: it appends three entries (tenant-scoped and global), \
                  reads them back scoped and fleet-wide, checks the jsonb round-trip, and asserts \
                  the append-only grant — splitting it would duplicate the multi-row setup"
    )]
    fn appends_scoped_newest_first_and_is_append_only() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let audit = store.audit();

            // Two entries for tenant-a (an update, then an archive carrying before/after), and one
            // tenant-global entry (NULL tenant) for a tenant create.
            audit
                .insert(
                    "01AUDIT000000000000000001A",
                    Some("tenant-a"),
                    "01ADMIN0000000000000000OPS",
                    "ops@pizza4ps.test",
                    "ops",
                    "store.update",
                    "store",
                    "store-1",
                    None,
                    Some(r#"{"name":"Bến Thành"}"#),
                    None,
                    1000,
                )
                .await
                .expect("append the first entry");
            audit
                .insert(
                    "01AUDIT000000000000000002A",
                    Some("tenant-a"),
                    "01ADMIN0000000000000000OPS",
                    "ops@pizza4ps.test",
                    "ops",
                    "store.archive",
                    "store",
                    "store-1",
                    Some(r#"{"name":"Bến Thành","status":"active"}"#),
                    Some(r#"{"name":"Bến Thành","status":"archived"}"#),
                    Some("req-9"),
                    2000,
                )
                .await
                .expect("append the second entry");
            audit
                .insert(
                    "01AUDIT000000000000000003G",
                    None,
                    "01ADMIN00000000000000OWNER",
                    "owner@pizza4ps.test",
                    "owner",
                    "tenant.create",
                    "tenant",
                    "tenant-a",
                    None,
                    Some(r#"{"name":"Pizza 4P's"}"#),
                    None,
                    3000,
                )
                .await
                .expect("append the global entry");

            // Tenant-scoped read: only tenant-a's two, newest-first.
            let scoped = audit
                .fetch(Some("tenant-a"), 10)
                .await
                .expect("fetch tenant-a");
            assert_eq!(scoped.len(), 2, "only the tenant's own entries");
            let newest = scoped.first().expect("an entry");
            assert_eq!(newest.action, "store.archive", "newest-first");
            assert_eq!(newest.at_ms, 2000);
            assert_eq!(newest.request_id.as_deref(), Some("req-9"));
            let after: serde_json::Value =
                serde_json::from_str(newest.after_json.as_deref().expect("an after value"))
                    .expect("after is valid json");
            assert_eq!(
                after.get("status").and_then(serde_json::Value::as_str),
                Some("archived"),
                "before/after round-trip through jsonb"
            );

            // Fleet-wide read: all three, including the NULL-tenant global entry.
            let all = audit.fetch(None, 10).await.expect("fetch all");
            assert_eq!(all.len(), 3, "the fleet-wide read spans every tenant");
            assert!(
                all.iter()
                    .any(|row| row.tenant_id.is_none() && row.action == "tenant.create"),
                "the tenant-global entry is included"
            );

            // A different tenant sees nothing.
            let other = audit
                .fetch(Some("tenant-b"), 10)
                .await
                .expect("fetch tenant-b");
            assert!(other.is_empty(), "the read is scoped to the tenant");

            // Append-only at the grant: app_tenant may SELECT/INSERT but never UPDATE or DELETE.
            let can_update: bool = admin
                .query_one(
                    "SELECT has_table_privilege('app_tenant', 'audit_log', 'UPDATE')",
                    &[],
                )
                .await
                .expect("privilege check")
                .get(0);
            let can_delete: bool = admin
                .query_one(
                    "SELECT has_table_privilege('app_tenant', 'audit_log', 'DELETE')",
                    &[],
                )
                .await
                .expect("privilege check")
                .get(0);
            assert!(
                !can_update,
                "audit_log is append-only: no UPDATE for app_tenant"
            );
            assert!(
                !can_delete,
                "audit_log is append-only: no DELETE for app_tenant"
            );
        });
    }

    /// `search` applies each non-`None` filter in SQL before the limit: by entity type, by action,
    /// by acting admin, and by a time window — so a narrow filter reaches the matching rows and a
    /// tenant filter still excludes the tenant-global `NULL` rows ([ADR-0069] slice 4).
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario: it seeds four rows across tenants/actors/times then exercises each \
                  filter dimension (entity, action, actor, time window, tenant) in turn; splitting \
                  it would duplicate the shared multi-row setup"
    )]
    fn search_filters_by_entity_action_actor_and_time() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let audit = store.audit();
            let seed = |id: &'static str,
                        tenant: Option<&'static str>,
                        actor: &'static str,
                        action: &'static str,
                        entity_type: &'static str,
                        at_ms: i64| {
                let audit = audit.clone();
                async move {
                    audit
                        .insert(
                            id,
                            tenant,
                            actor,
                            "a@pizza4ps.test",
                            "ops",
                            action,
                            entity_type,
                            "entity-1",
                            None,
                            None,
                            None,
                            at_ms,
                        )
                        .await
                        .expect("append");
                }
            };
            seed(
                "01SRCH00000000000000000S1",
                Some("tenant-a"),
                "admin-x",
                "store.update",
                "store",
                1000,
            )
            .await;
            seed(
                "01SRCH00000000000000000S2",
                Some("tenant-a"),
                "admin-y",
                "store.create",
                "store",
                2000,
            )
            .await;
            seed(
                "01SRCH00000000000000000M1",
                Some("tenant-a"),
                "admin-x",
                "menu.create",
                "menu",
                3000,
            )
            .await;
            seed(
                "01SRCH00000000000000000G1",
                None,
                "admin-x",
                "tenant.create",
                "tenant",
                4000,
            )
            .await;

            // By entity type: only the two store rows, newest first.
            let stores = audit
                .search(
                    Some("tenant-a"),
                    Some("store"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    10,
                )
                .await
                .expect("search by entity type");
            assert_eq!(stores.len(), 2);
            assert_eq!(
                stores.first().expect("a row").action,
                "store.create",
                "newest first"
            );

            // By action.
            let creates = audit
                .search(None, None, None, Some("store.create"), None, None, None, 10)
                .await
                .expect("search by action");
            assert_eq!(creates.len(), 1);
            assert_eq!(
                creates.first().expect("a row").id,
                "01SRCH00000000000000000S2"
            );

            // By acting admin, fleet-wide (includes the NULL-tenant global row).
            let by_x = audit
                .search(None, None, None, None, Some("admin-x"), None, None, 10)
                .await
                .expect("search by actor");
            assert_eq!(by_x.len(), 3, "admin-x acted three times across tenants");
            assert!(
                by_x.iter().any(|row| row.tenant_id.is_none()),
                "the fleet-wide read includes the global row"
            );

            // By time window [1500, 3500]: the create + the menu row.
            let window = audit
                .search(
                    Some("tenant-a"),
                    None,
                    None,
                    None,
                    None,
                    Some(1500),
                    Some(3500),
                    10,
                )
                .await
                .expect("search by time window");
            assert_eq!(window.len(), 2);
            assert!(
                window
                    .iter()
                    .all(|row| row.at_ms >= 1500 && row.at_ms <= 3500),
                "only rows inside the window"
            );

            // A tenant filter excludes the tenant-global row.
            let scoped = audit
                .search(Some("tenant-a"), None, None, None, None, None, None, 10)
                .await
                .expect("search scoped");
            assert_eq!(
                scoped.len(),
                3,
                "the three tenant-a rows, not the global one"
            );
            assert!(
                scoped
                    .iter()
                    .all(|row| row.tenant_id.as_deref() == Some("tenant-a")),
                "a tenant filter never returns NULL-tenant rows"
            );
        });
    }

    /// Appends `count` entries for one tenant, one per millisecond so the order is unambiguous
    /// before the `id` tiebreaker is even consulted.
    ///
    /// The id carries the tenant because `audit_log.id` is the primary key across every tenant: two
    /// calls with the same index range would collide, not partition.
    async fn trail(audit: &store_postgres::PostgresAudit, tenant: &str, count: i64) {
        for index in 0..count {
            audit
                .insert(
                    &format!("{tenant}-page-{index:04}"),
                    Some(tenant),
                    "01ADMIN0000000000000000OPS",
                    "ops@pizza4ps.test",
                    "ops",
                    "store.update",
                    "store",
                    "store-1",
                    None,
                    None,
                    None,
                    1_000 + index,
                )
                .await
                .expect("append an entry");
        }
    }

    /// A page of the trail carries its own window and the size of the *matching* set, and
    /// consecutive pages partition that set (ADR-0098 slice B3-2).
    ///
    /// `total` counting the filtered match rather than the whole log is the property that matters
    /// here: a pager over a filtered view whose total came from the unfiltered table would offer
    /// pages that are empty.
    #[test]
    fn a_page_of_the_trail_carries_the_window_and_the_matching_total() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let audit = store.audit();
            trail(&audit, "tenant-a", 25).await;
            // A neighbour's entries, and one tenant-global row, must reach neither the page nor the
            // count when a tenant is named.
            trail(&audit, "tenant-b", 7).await;

            let (first, total) = audit
                .search_page(
                    Some("tenant-a"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    AuditOrder::Newest,
                    10,
                    0,
                )
                .await
                .expect("first page");
            assert_eq!(first.len(), 10, "the window is the limit");
            assert_eq!(total, 25, "the total is the matching set, not the window");

            let mut seen: Vec<String> = first.into_iter().map(|row| row.id).collect();
            for offset in [10, 20] {
                let (page, page_total) = audit
                    .search_page(
                        Some("tenant-a"),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        AuditOrder::Newest,
                        10,
                        offset,
                    )
                    .await
                    .expect("later page");
                assert_eq!(page_total, 25, "the total does not change as pages advance");
                seen.extend(page.into_iter().map(|row| row.id));
            }
            assert_eq!(
                seen.len(),
                25,
                "three pages of ten cover twenty-five entries"
            );
            let mut unique = seen.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), 25, "no entry appears on two pages");

            // A narrower filter narrows the total too, not just the rows.
            let (rows, filtered_total) = audit
                .search_page(
                    Some("tenant-a"),
                    None,
                    None,
                    None,
                    None,
                    Some(1_005),
                    Some(1_009),
                    AuditOrder::Newest,
                    10,
                    0,
                )
                .await
                .expect("filtered page");
            assert_eq!(filtered_total, 5, "five entries fall inside the window");
            assert_eq!(rows.len(), 5);
        });
    }

    /// The paged read matches the same rows in the same order as the windowed one, and a page past
    /// the end is empty rather than an error.
    #[test]
    fn the_paged_trail_agrees_with_the_windowed_read_on_order_and_scope() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let audit = store.audit();
            trail(&audit, "tenant-a", 6).await;
            trail(&audit, "tenant-b", 4).await;

            let windowed: Vec<String> = audit
                .search(Some("tenant-a"), None, None, None, None, None, None, 6)
                .await
                .expect("windowed")
                .into_iter()
                .map(|row| row.id)
                .collect();
            assert_eq!(windowed.len(), 6, "only this tenant's entries");

            let (paged, total) = audit
                .search_page(
                    Some("tenant-a"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    AuditOrder::Newest,
                    6,
                    0,
                )
                .await
                .expect("paged");
            assert_eq!(total, 6, "the count is tenant-scoped too");
            let paged: Vec<String> = paged.into_iter().map(|row| row.id).collect();
            assert_eq!(
                paged, windowed,
                "a full-width page is the windowed read, in the same order"
            );

            // A page past the end: empty, but `total` still counts what the filters matched. The
            // window count rides on the rows and there are none, so the adapter falls back to a
            // second count carrying the same predicates.
            let (beyond, beyond_total) = audit
                .search_page(
                    Some("tenant-a"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    AuditOrder::Newest,
                    10,
                    100,
                )
                .await
                .expect("a page past the end still reads");
            assert!(beyond.is_empty());
            assert_eq!(
                beyond_total, 6,
                "an empty window still reports how many rows the filters matched"
            );
        });
    }

    /// Every page of `tenant-a`'s trail in one order, stitched into the sequence a caller paging
    /// through would see, ten at a time.
    ///
    /// Reads until a page comes back empty rather than dividing by `expected`, so a page that
    /// dropped or repeated a row lands in the stitched sequence instead of being hidden by the
    /// arithmetic. `expected` is checked against the window count on every non-empty page.
    async fn stitched_trail(
        audit: &store_postgres::PostgresAudit,
        order: AuditOrder,
        expected: i64,
    ) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        let mut offset = 0;
        loop {
            let (page, total) = audit
                .search_page(
                    Some("tenant-a"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    order,
                    10,
                    offset,
                )
                .await
                .expect("a page of the trail");
            if page.is_empty() {
                return ids;
            }
            assert_eq!(
                total, expected,
                "{order:?}: the order does not change how many matched",
            );
            ids.extend(page.into_iter().map(|row| row.id));
            offset += 10;
        }
    }

    /// The reversed order windows the same set backwards, and the same index still serves it.
    ///
    /// Two properties in one scenario because they are one claim: `?order=oldest` shipped without a
    /// migration on the grounds that `audit_log_by_tenant_newest` read *backwards* is the whole of
    /// the oldest-first order. The set assertion proves the pages are a partition; the plan
    /// assertion proves the database gets there by walking that index rather than by sorting.
    #[test]
    fn the_reversed_trail_partitions_the_same_set_and_is_still_served_by_the_index() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let audit = store.audit();
            trail(&audit, "tenant-a", 25).await;
            trail(&audit, "tenant-b", 7).await;

            let newest = stitched_trail(&audit, AuditOrder::Newest, 25).await;
            let oldest = stitched_trail(&audit, AuditOrder::Oldest, 25).await;
            let mut reversed = oldest.clone();
            reversed.reverse();
            assert_eq!(
                reversed, newest,
                "one order is the other read backwards — every page of it, not one page flipped",
            );
            let mut unique = oldest.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), 25, "no entry appears on two pages either way");

            admin
                .batch_execute("ANALYZE audit_log")
                .await
                .expect("analyze");
            let plan = {
                admin
                    .batch_execute("SET enable_seqscan = off")
                    .await
                    .expect("prefer an index if one fits");
                let rows = admin
                    .query(
                        "EXPLAIN SELECT id FROM audit_log WHERE tenant_id = $1 \
                         ORDER BY at ASC, id ASC LIMIT $2 OFFSET $3",
                        &[&"tenant-a", &10_i64, &0_i64],
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
                plan.contains("audit_log_by_tenant_newest"),
                "the newest-first index should serve the reversed order too; plan was:\n{plan}"
            );
            assert!(
                !plan.contains("Sort Key"),
                "reading that index backwards needs no sort step; plan was:\n{plan}"
            );
        });
    }

    /// Migration 0042's index carries the trail's whole order, tiebreaker included, so a page needs
    /// no sort step above the scan.
    ///
    /// `audit_log`'s read was already ordered totally (`at DESC, id DESC`) — this is the one paged
    /// table where nothing about the query changed. What was missing was the `id` column in the
    /// index, without which the plan finishes the order with a `Sort` and `LIMIT` truncates a
    /// completed sort. That is also what makes the `count(*) OVER()` walk index-only.
    #[test]
    fn the_paged_trail_query_is_served_by_the_index_and_not_by_a_sort() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let audit = store.audit();
            trail(&audit, "tenant-a", 40).await;
            admin
                .batch_execute("ANALYZE audit_log")
                .await
                .expect("analyze");

            let plan = {
                admin
                    .batch_execute("SET enable_seqscan = off")
                    .await
                    .expect("prefer an index if one fits");
                let rows = admin
                    .query(
                        "EXPLAIN SELECT id FROM audit_log WHERE tenant_id = $1 \
                         ORDER BY at DESC, id DESC LIMIT $2 OFFSET $3",
                        &[&"tenant-a", &10_i64, &0_i64],
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
                plan.contains("audit_log_by_tenant_newest"),
                "the page should be served by the sort-carrying index; plan was:\n{plan}"
            );
            assert!(
                !plan.contains("Sort Key"),
                "an index that carries the order needs no sort step; plan was:\n{plan}"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// The employee store: tenant-scoped, RLS-isolated, PIN held only as its hash (ADR-0070).
// ---------------------------------------------------------------------------

mod employees_store {
    use core::fmt::Write as _;

    use store_postgres::EmployeeOrder;

    use super::{TENANT_A, block_on, prepared};
    use store_postgres::RowUpdate;

    /// Employees insert and read back tenant-scoped and newest-first; the code is unique within a
    /// tenant but free across tenants; `has_pin` reflects `set_pin` without ever exposing the hash on
    /// a read (only `pin_phc` reads it back); an update renames/archives; and the grant is
    /// SELECT/INSERT/UPDATE only — no DELETE (an employee is archived, never removed) ([ADR-0070]).
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end scenario over the real table: insert across two tenants, the \
                  unique-code constraint, scoped + newest-first reads, the PIN hash round-trip and \
                  has_pin flag, an update, and the append-only-ish grant — splitting it would \
                  duplicate the multi-tenant setup"
    )]
    fn insert_scoped_pin_round_trip_and_grant() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let people = store.people();

            // Two tenants, both using the staff code "A01" — unique per tenant, not globally.
            people
                .insert("01EMP0000000000000000000A1", "tenant-a", "A01", "Alice")
                .await
                .expect("insert alice");
            people
                .insert("01EMP0000000000000000000A2", "tenant-a", "A02", "Anh")
                .await
                .expect("insert anh");
            people
                .insert("01EMP0000000000000000000B1", "tenant-b", "A01", "Bao")
                .await
                .expect("a duplicate code is fine under a different tenant");

            // The unique index refuses a second "A01" within tenant-a.
            assert!(
                people
                    .insert("01EMP0000000000000000000A3", "tenant-a", "A01", "Clone")
                    .await
                    .is_err(),
                "staff codes are unique within a tenant"
            );

            // Tenant-scoped, newest-first, and no PIN yet.
            let scoped = people.fetch("tenant-a").await.expect("fetch tenant-a");
            assert_eq!(scoped.len(), 2, "only tenant-a's own employees");
            assert_eq!(scoped.first().expect("a row").name, "Anh", "newest first");
            assert!(scoped.iter().all(|row| !row.has_pin), "no PIN set yet");

            // A different tenant sees only its own.
            let other = people.fetch("tenant-b").await.expect("fetch tenant-b");
            assert_eq!(other.len(), 1);
            assert_eq!(other.first().expect("a row").code, "A01");

            // Set a PIN hash: has_pin flips, and the hash round-trips only via pin_phc — fetch never
            // carries it.
            assert!(
                people
                    .set_pin(
                        "tenant-a",
                        "01EMP0000000000000000000A1",
                        "argon2id$phc$alice"
                    )
                    .await
                    .expect("set pin"),
                "the row was found"
            );
            let alice = people
                .fetch_one("tenant-a", "01EMP0000000000000000000A1")
                .await
                .expect("fetch one")
                .expect("present");
            assert!(alice.has_pin, "has_pin reflects the set PIN");
            assert_eq!(
                people
                    .pin_phc("tenant-a", "01EMP0000000000000000000A1")
                    .await
                    .expect("pin_phc"),
                Some("argon2id$phc$alice".to_owned()),
                "the trusted path reads the stored hash back"
            );

            // A PIN set scoped to the wrong tenant matches no row.
            assert!(
                !people
                    .set_pin("tenant-b", "01EMP0000000000000000000A1", "x")
                    .await
                    .expect("cross-tenant set"),
                "set_pin is tenant-scoped"
            );

            // Rename + archive, at the version the read handed out (ADR-0094).
            assert!(
                matches!(
                    people
                        .set(
                            "tenant-a",
                            "01EMP0000000000000000000A1",
                            "Alice Nguyen",
                            "archived",
                            &alice.version,
                        )
                        .await
                        .expect("update"),
                    RowUpdate::Updated(_)
                ),
                "the row changed"
            );
            let archived = people
                .fetch_one("tenant-a", "01EMP0000000000000000000A1")
                .await
                .expect("fetch one")
                .expect("present");
            assert_eq!(archived.name, "Alice Nguyen");
            assert_eq!(archived.status, "archived");

            // Append-only-ish grant: app_tenant may SELECT/INSERT/UPDATE but never DELETE.
            let can_delete: bool = admin
                .query_one(
                    "SELECT has_table_privilege('app_tenant', 'employees', 'DELETE')",
                    &[],
                )
                .await
                .expect("privilege check")
                .get(0);
            let can_update: bool = admin
                .query_one(
                    "SELECT has_table_privilege('app_tenant', 'employees', 'UPDATE')",
                    &[],
                )
                .await
                .expect("privilege check")
                .get(0);
            assert!(!can_delete, "employees is never DELETEd — archived instead");
            assert!(
                can_update,
                "app_tenant may UPDATE (rename / archive / set PIN)"
            );
        });
    }

    /// A page is a window onto the sequence the unpaged roster read returns: the pages partition it,
    /// every page reports the tenant's headcount rather than its own size, a page past the end is
    /// empty and still counts, and another tenant's staff are in neither ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md)).
    ///
    /// The whole roster is inserted in one transaction on purpose. `created_at` defaults to `now()`,
    /// which is *transaction* time, so those rows share one timestamp exactly — the case decision 9
    /// is about, and the case a `created_at DESC` order without the primary-key tiebreaker cannot
    /// partition. A CSV staff import is exactly this shape.
    #[test]
    fn the_roster_pages_partition_the_unpaged_read_even_when_a_batch_shares_one_timestamp() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let people = store.people();

            // Six of ours and one elsewhere, all in one transaction.
            let mut inserts = String::new();
            for index in 0..6 {
                write!(
                    inserts,
                    "INSERT INTO employees (id, tenant_id, code, name) \
                     VALUES ('01EMPPAGE000000000000000{index:02}', '{TENANT_A}', 'P{index:02}', \
                     'Person {index}');"
                )
                .expect("writing to a String cannot fail");
            }
            inserts.push_str(
                "INSERT INTO employees (id, tenant_id, code, name) \
                 VALUES ('01EMPPAGE000000000000000FF', 'tenant-b', 'P00', 'Elsewhere');",
            );
            admin
                .batch_execute(&format!("BEGIN; {inserts} COMMIT;"))
                .await
                .expect("write the batch in one transaction");

            let distinct: i64 = admin
                .query_one(
                    "SELECT count(DISTINCT created_at) FROM employees WHERE tenant_id = $1",
                    &[&TENANT_A],
                )
                .await
                .expect("count the distinct timestamps")
                .get(0);
            assert_eq!(
                distinct, 1,
                "six rows in one transaction carry one created_at, not six close ones — \
                 which is why the read's ORDER BY needs id as a tiebreaker"
            );

            let roster = people.fetch(TENANT_A).await.expect("the unpaged roster");
            assert_eq!(roster.len(), 6, "the unpaged read is unchanged by paging");

            let mut stitched = Vec::new();
            for offset in [0, 2, 4] {
                let (page, total) = people
                    .fetch_page(TENANT_A, None, EmployeeOrder::Newest, false, 2, offset)
                    .await
                    .expect("page over the tied batch");
                assert_eq!(
                    total, 6,
                    "every page reports the tenant's headcount, not the window's size"
                );
                assert!(
                    page.iter().all(|row| !row.has_pin),
                    "a page carries whether a PIN exists, never the hash (ADR-0070)"
                );
                stitched.extend(page.into_iter().map(|row| row.id));
            }
            assert_eq!(
                stitched,
                roster.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
                "three pages of two partition the roster in the unpaged read's own order"
            );

            // A page past the end is empty and still counts the roster — not an error, not zero.
            let (beyond, beyond_total) = people
                .fetch_page(TENANT_A, None, EmployeeOrder::Newest, false, 10, 50)
                .await
                .expect("a page past the end still reads");
            assert!(beyond.is_empty());
            assert_eq!(beyond_total, 6);

            // The other tenant's page sees only its own row, and counts only its own.
            let (theirs, their_total) = people
                .fetch_page("tenant-b", None, EmployeeOrder::Newest, false, 10, 0)
                .await
                .expect("the other tenant's page");
            assert_eq!(their_total, 1, "a headcount is per tenant");
            assert_eq!(
                theirs.first().expect("a row").name,
                "Elsewhere",
                "and the page is too"
            );
        });
    }

    /// `?q=` narrows the roster page on the person's **name or staff code**, and narrows `total`
    /// with it.
    ///
    /// Both columns, because those are the two handles an operator has on someone: a name they were
    /// told, or the code on a badge. The two matches here are deliberately *different people*, so a
    /// predicate that dropped either column would fail rather than pass on the other's row.
    ///
    /// This is the read an assign picker sits on, and having it is what lets the People screen's
    /// table be paged at all ([ADR-0098](../../../../docs/adr/0098-paged-admin-reads.md), B3-4).
    #[test]
    fn the_roster_search_narrows_the_page_and_its_total_on_name_or_code() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let people = store.people();

            for (id, code, name) in [
                ("01EMPFIND00000000000000A1", "C01", "Mai Anh"),
                ("01EMPFIND00000000000000A2", "C02", "Bao"),
                ("01EMPFIND00000000000000A3", "MAI99", "Linh"),
            ] {
                people
                    .insert(id, TENANT_A, code, name)
                    .await
                    .expect("insert the employee");
            }

            let (matched, matched_total) = people
                .fetch_page(TENANT_A, Some("mai"), EmployeeOrder::Newest, false, 10, 0)
                .await
                .expect("the searched page");
            assert_eq!(
                matched_total, 2,
                "the total counts what matched, not the roster"
            );
            let mut codes: Vec<String> = matched.into_iter().map(|row| row.code).collect();
            codes.sort();
            assert_eq!(
                codes,
                vec!["C01".to_owned(), "MAI99".to_owned()],
                "one matched on name, the other on code, both case-insensitively"
            );

            // No match is an empty page and a zero total — and this zero is the true one, because
            // the empty-window fallback carries the same predicate the page did.
            let (none, none_total) = people
                .fetch_page(
                    TENANT_A,
                    Some("nobody"),
                    EmployeeOrder::Newest,
                    false,
                    10,
                    0,
                )
                .await
                .expect("a search that matches nothing");
            assert!(none.is_empty());
            assert_eq!(
                none_total, 0,
                "the fallback count is filtered, not the whole roster"
            );

            // `%` is a character in the needle, not a wildcard: the read uses `position`, not
            // `ILIKE`, so an operator searching for a literal percent gets a literal search rather
            // than every row.
            let (wild, wild_total) = people
                .fetch_page(TENANT_A, Some("%"), EmployeeOrder::Newest, false, 10, 0)
                .await
                .expect("a literal percent");
            assert!(wild.is_empty(), "no name or code contains a percent sign");
            assert_eq!(wild_total, 0);

            // An unsearched page is still the whole roster.
            let (all, all_total) = people
                .fetch_page(TENANT_A, None, EmployeeOrder::Newest, false, 10, 0)
                .await
                .expect("the unsearched page");
            assert_eq!(all.len(), 3);
            assert_eq!(all_total, 3);
            assert!(
                all.iter().all(|row| !row.has_pin),
                "a searched or unsearched page carries whether a PIN exists, never the hash"
            );

            drop(admin);
        });
    }

    /// `?sort=` reorders the page, every order is total, and `?order=desc` is the exact reverse.
    ///
    /// The three orders are asserted against data whose three sequences differ, so no two can pass
    /// for one another: a name order that quietly fell back to `created_at` would fail rather than
    /// coincide. The `desc` case is the reverse of the `asc` one *element by element*, which is what
    /// the flipped tiebreaker in [`employee_order`] buys — without it, reversing only the leading
    /// column would give a different total order that happens to share a first row.
    #[test]
    fn the_roster_page_orders_by_name_or_code_and_reverses_exactly() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let people = store.people();

            // Inserted in an order matching neither sort, so insertion order cannot pass for either.
            // Two people share the name "Bao" — the ordinary case a staff code exists to
            // disambiguate — so the `id` tiebreaker actually fires and the reversal has something to
            // get wrong.
            for (id, code, name) in [
                ("01EMPSORT00000000000000A1", "C03", "Bao"),
                ("01EMPSORT00000000000000A2", "C01", "Mai"),
                ("01EMPSORT00000000000000A3", "C02", "An"),
                ("01EMPSORT00000000000000A4", "C04", "Bao"),
            ] {
                people
                    .insert(id, TENANT_A, code, name)
                    .await
                    .expect("insert the employee");
            }

            // Codes rather than names, because two rows share a name: only the code distinguishes
            // them, so only a code-level assertion can see the tiebreaker do its job.
            let codes = |rows: Vec<store_postgres::EmployeeRow>| {
                rows.into_iter().map(|row| row.code).collect::<Vec<_>>()
            };

            // "Bao"/C03 before "Bao"/C04, because the tiebreaker is the id and A1 precedes A4.
            let ascending = vec![
                "C02".to_owned(),
                "C03".to_owned(),
                "C04".to_owned(),
                "C01".to_owned(),
            ];
            let (by_name, _) = people
                .fetch_page(TENANT_A, None, EmployeeOrder::Name, false, 10, 0)
                .await
                .expect("by name");
            assert_eq!(codes(by_name), ascending);

            let (by_name_desc, _) = people
                .fetch_page(TENANT_A, None, EmployeeOrder::Name, true, 10, 0)
                .await
                .expect("by name, reversed");
            let mut reversed = ascending.clone();
            reversed.reverse();
            assert_eq!(
                codes(by_name_desc),
                reversed,
                "reversed is the exact reverse, element by element — which needs the tiebreaker to \
                 flip with the direction, not only the leading column"
            );

            let (by_code, _) = people
                .fetch_page(TENANT_A, None, EmployeeOrder::Code, false, 10, 0)
                .await
                .expect("by code");
            assert_eq!(
                codes(by_code),
                vec![
                    "C01".to_owned(),
                    "C02".to_owned(),
                    "C03".to_owned(),
                    "C04".to_owned()
                ],
                "a different sequence from the name order, so one cannot pass for the other"
            );

            // A sorted page still partitions the set: four pages of one, no repeat and no gap —
            // across the shared name, which is where a non-total order would drop or double a row.
            let mut stitched = Vec::new();
            for offset in [0, 1, 2, 3] {
                let (page, total) = people
                    .fetch_page(TENANT_A, None, EmployeeOrder::Name, false, 1, offset)
                    .await
                    .expect("a page of the name order");
                assert_eq!(total, 4, "the total is the set, whatever the order");
                stitched.extend(codes(page));
            }
            assert_eq!(
                stitched, ascending,
                "windows over the name order stitch back into it"
            );

            // The search composes with the order rather than replacing it.
            let (searched, searched_total) = people
                .fetch_page(TENANT_A, Some("bao"), EmployeeOrder::Name, false, 10, 0)
                .await
                .expect("searched and sorted");
            assert_eq!(searched_total, 2, "both people named Bao");
            assert_eq!(
                codes(searched),
                vec!["C03".to_owned(), "C04".to_owned()],
                "and the tiebreaker orders them inside the search too"
            );

            drop(admin);
        });
    }

    /// The name order rides `employees_by_tenant_name` (migration 0046) rather than sorting.
    ///
    /// The same reasoning as the default order's plan test, applied to the order a query parameter
    /// introduced: `?sort=name` that fell back to a sort of the tenant's whole roster would
    /// reintroduce, through a parameter, exactly the cost migration 0045 exists to avoid.
    #[test]
    fn the_name_ordered_roster_page_is_served_by_its_own_index() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let people = store.people();
            for index in 0..30_i32 {
                people
                    .insert(
                        &format!("01EMPNAME00000000000000{index:03}"),
                        TENANT_A,
                        &format!("Q{index:03}"),
                        &format!("Name {index:03}"),
                    )
                    .await
                    .expect("insert");
            }
            admin
                .batch_execute("ANALYZE employees")
                .await
                .expect("analyze");

            let plan = {
                admin
                    .batch_execute("SET enable_seqscan = off")
                    .await
                    .expect("prefer an index if one fits");
                let rows = admin
                    .query(
                        "EXPLAIN SELECT id FROM employees WHERE tenant_id = $1 \
                         ORDER BY name ASC, id ASC LIMIT $2 OFFSET $3",
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
                plan.contains("employees_by_tenant_name"),
                "the name order walks its own index: {plan}"
            );
            assert!(
                !plan.contains("Sort"),
                "and walking it *is* the sort, so no Sort node sits above the scan: {plan}"
            );

            drop(admin);
        });
    }

    /// The index migration 0045 added covers this query's `ORDER BY`, so `LIMIT` stops the scan
    /// instead of truncating a completed sort of the tenant's whole roster.
    ///
    /// Asserted through `EXPLAIN` rather than by timing: on 30 rows a timing test would pass either
    /// way. What this catches is the migration being dropped, or `EMPLOYEE_ORDER` drifting away from
    /// the index built for it — including the `id` tiebreaker, whose absence from the index is what
    /// would put a `Sort` node back above the scan.
    #[test]
    fn the_paged_roster_query_is_served_by_the_index_and_not_by_a_sort() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let people = store.people();
            for index in 0..30 {
                people
                    .insert(
                        &format!("01EMPSCAN00000000000000{index:03}"),
                        TENANT_A,
                        &format!("Q{index:03}"),
                        "Planned",
                    )
                    .await
                    .expect("insert");
            }
            // Without statistics the planner picks a sequential scan on a table this small whatever
            // indexes exist, so the assertion would be about row count rather than about the index.
            admin
                .batch_execute("ANALYZE employees")
                .await
                .expect("analyze");

            let plan = {
                admin
                    .batch_execute("SET enable_seqscan = off")
                    .await
                    .expect("prefer an index if one fits");
                let rows = admin
                    .query(
                        "EXPLAIN SELECT id FROM employees WHERE tenant_id = $1 \
                         ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3",
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
                plan.contains("employees_by_tenant_newest"),
                "the page should be served by the sort-carrying index; plan was:\n{plan}"
            );
            assert!(
                !plan.contains("Sort Key"),
                "an index that carries the order needs no sort step; plan was:\n{plan}"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Role templates + per-store assignments: people & access (ADR-0070, M1 slice 2).
// ---------------------------------------------------------------------------

mod role_templates_and_assignments {
    use super::{block_on, prepared};
    use store_postgres::RowUpdate;

    /// Role templates insert with a jsonb permission set that round-trips as its JSON text, read back
    /// tenant-scoped and newest-first, and update name/permissions/status. The grant is
    /// SELECT/INSERT/UPDATE only — no DELETE (a role is archived, never removed) ([ADR-0070]).
    #[test]
    fn role_templates_round_trip_permissions_and_grant() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let people = store.people();

            people
                .insert_role_template(
                    "01ROLE000000000000000000A1",
                    "tenant-a",
                    "Cashier",
                    r#"["billing.discount.apply","sales.item.open"]"#,
                )
                .await
                .expect("insert cashier");
            people
                .insert_role_template("01ROLE000000000000000000B1", "tenant-b", "Cashier", "[]")
                .await
                .expect("a duplicate name is fine under a different tenant");

            // The unique index refuses a second "Cashier" within tenant-a.
            assert!(
                people
                    .insert_role_template("01ROLE000000000000000000A2", "tenant-a", "Cashier", "[]")
                    .await
                    .is_err(),
                "role names are unique within a tenant"
            );

            // Tenant-scoped read; the jsonb permission set round-trips as its JSON text.
            let scoped = people
                .fetch_role_templates("tenant-a")
                .await
                .expect("fetch tenant-a");
            assert_eq!(scoped.len(), 1, "only tenant-a's own roles");
            let cashier = scoped.first().expect("a row");
            let permissions: Vec<String> =
                serde_json::from_str(&cashier.permissions_json).expect("permissions are JSON");
            assert_eq!(
                permissions,
                vec![
                    "billing.discount.apply".to_owned(),
                    "sales.item.open".to_owned()
                ]
            );

            // Update the permission set + archive, at the version the read handed out (ADR-0094).
            assert!(
                matches!(
                    people
                        .set_role_template(
                            "tenant-a",
                            "01ROLE000000000000000000A1",
                            "Cashier",
                            r#"["sales.item.open"]"#,
                            "archived",
                            &cashier.version,
                        )
                        .await
                        .expect("update"),
                    RowUpdate::Updated(_)
                ),
                "the row changed"
            );
            let updated = people
                .fetch_role_template("tenant-a", "01ROLE000000000000000000A1")
                .await
                .expect("fetch one")
                .expect("present");
            assert_eq!(updated.status, "archived");
            let updated_permissions: Vec<String> =
                serde_json::from_str(&updated.permissions_json).expect("permissions are JSON");
            assert_eq!(updated_permissions, vec!["sales.item.open".to_owned()]);

            // Roles are archived, never deleted: no DELETE grant.
            let can_delete: bool = admin
                .query_one(
                    "SELECT has_table_privilege('app_tenant', 'role_templates', 'DELETE')",
                    &[],
                )
                .await
                .expect("privilege check")
                .get(0);
            assert!(
                !can_delete,
                "role_templates is never DELETEd — archived instead"
            );
        });
    }

    /// An assignment read names the person it grants, and still lists one whose employee row is gone.
    ///
    /// The console used to turn an assignment's `employee_id` into a name by searching the tenant's
    /// whole roster, which is the read that stops working once the roster is paged
    /// ([ADR-0098](../../../../docs/adr/0098-paged-admin-reads.md), B3-4). The resolution is a join
    /// on the read that needs it.
    ///
    /// The second half is why that join is `LEFT`. Nothing declares a foreign key from an assignment
    /// to an employee, so an assignment can name a row that is not there; an inner join would drop it
    /// from the list, hiding a grant that still exists. It lists with no name instead, and the console
    /// falls back to showing the id.
    #[test]
    fn an_assignment_read_names_the_person_and_still_lists_one_whose_employee_is_gone() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let people = store.people();

            people
                .insert("01EMP00000000000000000MAI1", "tenant-a", "C77", "Mai")
                .await
                .expect("insert the employee");
            people
                .insert_assignment(
                    "01ASSIGN0000000000000NAMED",
                    "tenant-a",
                    "01EMP00000000000000000MAI1",
                    "01STORE000000000000000000S",
                    "01ROLE000000000000000000A1",
                )
                .await
                .expect("assign the person who exists");
            // No `employees` row is ever inserted for this id.
            people
                .insert_assignment(
                    "01ASSIGN000000000000DANGLE",
                    "tenant-a",
                    "01EMP0000000000000000GONE1",
                    "01STORE000000000000000000S",
                    "01ROLE000000000000000000A1",
                )
                .await
                .expect("assign an id with no employee row");

            let rows = people
                .fetch_assignments_for_store("tenant-a", "01STORE000000000000000000S")
                .await
                .expect("by store");
            assert_eq!(rows.len(), 2, "both grants are listed, resolvable or not");

            let named = rows
                .iter()
                .find(|row| row.id == "01ASSIGN0000000000000NAMED")
                .expect("the resolvable assignment");
            assert_eq!(
                named.employee_name.as_deref(),
                Some("Mai"),
                "the join names the person, so no roster read is needed"
            );
            assert_eq!(named.employee_code.as_deref(), Some("C77"));

            let dangling = rows
                .iter()
                .find(|row| row.id == "01ASSIGN000000000000DANGLE")
                .expect("the assignment whose employee row is gone is still listed");
            assert_eq!(
                dangling.employee_name, None,
                "an unresolvable grant reads as unnamed, not as absent"
            );
            assert_eq!(dangling.employee_code, None);

            // The other direction resolves the same way.
            let by_employee = people
                .fetch_assignments_for_employee("tenant-a", "01EMP00000000000000000MAI1")
                .await
                .expect("by employee");
            assert_eq!(by_employee.len(), 1);
            assert_eq!(
                by_employee.first().expect("a row").employee_name.as_deref(),
                Some("Mai")
            );

            drop(admin);
        });
    }

    /// Assignments bind a person to a store with a role, read both by store and by employee,
    /// tenant-scoped; the same person at the same store is refused; and — unlike employees/roles — an
    /// assignment IS removable (a DELETE grant), which offboards the person ([ADR-0070]).
    #[test]
    fn assignments_bind_read_and_remove_with_delete_grant() {
        block_on(async {
            let (store, admin) = prepared().await.expect("prepare the database");
            let people = store.people();

            people
                .insert_assignment(
                    "01ASSIGN00000000000000000A",
                    "tenant-a",
                    "01EMP0000000000000000000A1",
                    "01STORE000000000000000000S",
                    "01ROLE000000000000000000A1",
                )
                .await
                .expect("assign");
            // The same person at the same store twice is refused by the unique index.
            assert!(
                people
                    .insert_assignment(
                        "01ASSIGN00000000000000000B",
                        "tenant-a",
                        "01EMP0000000000000000000A1",
                        "01STORE000000000000000000S",
                        "01ROLE000000000000000000A1",
                    )
                    .await
                    .is_err(),
                "a person is assigned to a store at most once"
            );

            // Readable both ways, tenant-scoped.
            assert_eq!(
                people
                    .fetch_assignments_for_store("tenant-a", "01STORE000000000000000000S")
                    .await
                    .expect("by store")
                    .len(),
                1
            );
            assert_eq!(
                people
                    .fetch_assignments_for_employee("tenant-a", "01EMP0000000000000000000A1")
                    .await
                    .expect("by employee")
                    .len(),
                1
            );
            assert!(
                people
                    .fetch_assignments_for_store("tenant-b", "01STORE000000000000000000S")
                    .await
                    .expect("other tenant")
                    .is_empty(),
                "another tenant sees none of these assignments"
            );

            // Cross-tenant remove matches nothing; a scoped remove offboards.
            assert!(
                !people
                    .delete_assignment("tenant-b", "01ASSIGN00000000000000000A")
                    .await
                    .expect("cross-tenant remove"),
                "delete is tenant-scoped"
            );
            assert!(
                people
                    .delete_assignment("tenant-a", "01ASSIGN00000000000000000A")
                    .await
                    .expect("remove"),
                "the row was removed"
            );
            assert!(
                people
                    .fetch_assignments_for_employee("tenant-a", "01EMP0000000000000000000A1")
                    .await
                    .expect("after remove")
                    .is_empty(),
                "the assignment is gone"
            );

            // Assignments ARE removable — a DELETE grant, unlike employees/roles.
            let can_delete: bool = admin
                .query_one(
                    "SELECT has_table_privilege('app_tenant', 'employee_store_assignments', 'DELETE')",
                    &[],
                )
                .await
                .expect("privilege check")
                .get(0);
            assert!(
                can_delete,
                "an assignment is removed (offboarding), not archived"
            );
        });
    }
}

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
    use super::{TENANT_A, block_on, prepared};

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

            // Approve one; it leaves the pending list and joins the approved one.
            assert!(
                devices
                    .mark(TENANT_A, "DEV1", "approved")
                    .await
                    .expect("approve"),
                "a pending proposal is resolved"
            );
            let approved = devices
                .fetch(TENANT_A, Some("store-1"), "approved")
                .await
                .expect("store-1 approved");
            assert_eq!(approved.len(), 1);
            assert_eq!(approved.first().expect("one row").id, "DEV1");
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
                    .mark(TENANT_A, "DEV1", "rejected")
                    .await
                    .expect("re-resolve"),
                "an already-resolved proposal does not transition again"
            );
            // And another tenant cannot resolve this tenant's proposal.
            assert!(
                !devices
                    .mark("tenant-b", "DEV2", "approved")
                    .await
                    .expect("cross-tenant"),
                "the tenant scope stops one tenant resolving another's proposal"
            );
        });
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
                .insert_station(station, tenant, store_id, "Oven", None, true)
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
                    .set_station(tenant, station, "Nope", None, false, "active", "1")
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
                        "active",
                        &moved,
                    )
                    .await
                    .expect("the probe must not raise"),
                RowUpdate::NotFound
            );
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
                .insert_role_template(role, tenant, "Cashier", "[\"sales.item.open\"]")
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
            assert_eq!(row.version, first);

            assert_eq!(
                people
                    .set_role_template(tenant, role, "Nope", "[]", "active", "not-a-transaction-id")
                    .await
                    .expect("the comparison must not raise"),
                RowUpdate::VersionMismatch
            );
            let second = match people
                .set_role_template(tenant, role, "Cashier", "[]", "archived", &first)
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
