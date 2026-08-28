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
                 TRUNCATE events, event_outbox, rollups, api_keys, super_admin, admin_sessions, \
                 admin_invites, admin_recovery_codes, admin_users, config_trees, store_liveness, \
                 task_health, audit_log, alerts, catalog_tax_rates, media_assets, stores, \
                 order_queue, subjects, webhook_endpoints, device_proposals, activation_codes, \
                 device_credentials RESTART IDENTITY;",
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
            "TRUNCATE events, event_outbox, rollups, api_keys, super_admin, admin_sessions, \
             config_trees, store_liveness, task_health, audit_log, alerts, catalog_tax_rates, \
             media_assets, stores, order_queue, subjects, webhook_endpoints, device_proposals, \
             activation_codes, device_credentials RESTART IDENTITY",
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
            let scopes = vec!["read_rollups".to_owned(), "read_events".to_owned()];

            keys.insert(
                "KEY0000000000000000000001",
                "TENANT000000000000000000AA",
                hash,
                &scopes,
                None,
            )
            .await
            .expect("insert the key");

            let row = keys
                .fetch("KEY0000000000000000000001")
                .await
                .expect("fetch")
                .expect("the inserted key is present");
            assert_eq!(row.tenant_id, "TENANT000000000000000000AA");
            assert_eq!(row.secret_hash, vec![3_u8; 32]);
            assert!(!row.revoked, "a fresh key is live");
            assert_eq!(row.expires_at_ms, None);

            let listed = keys
                .list_for_tenant("TENANT000000000000000000AA")
                .await
                .expect("list");
            assert_eq!(listed.len(), 1);
            let only = listed.first().expect("exactly one key");
            assert_eq!(only.id, "KEY0000000000000000000001");
            assert_eq!(only.scopes, scopes, "the granted scopes are listed");

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

    fn parsed(json: &str) -> serde_json::Value {
        serde_json::from_str(json).expect("valid json")
    }

    /// A tree state saves, loads back equal, upserts in place, and is scoped to its tenant.
    #[test]
    fn save_load_upsert_and_tenant_scope() {
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
            trees
                .save_state(tenant, store_id, first)
                .await
                .expect("save");
            let loaded = trees
                .load_state(tenant, store_id)
                .await
                .expect("load")
                .expect("present");
            assert_eq!(
                parsed(&loaded),
                parsed(first),
                "the stored document round-trips (compared as JSON, since jsonb reorders keys)"
            );

            // Upsert in place: a second save replaces the row rather than erroring or duplicating.
            let second = r#"{"k":20,"layers":[{"currency_code":"JPY"},{},{},{}],"history":[]}"#;
            trees
                .save_state(tenant, store_id, second)
                .await
                .expect("upsert");
            let reloaded = trees
                .load_state(tenant, store_id)
                .await
                .expect("load")
                .expect("present");
            assert_eq!(
                parsed(&reloaded),
                parsed(second),
                "the upsert replaced the state"
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
                .record_ota_report(tenant, store_id, "v1.2.3", true, 5000)
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
                .record_ota_report(tenant, store_id, "v1.3.0", false, 9000)
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
                .save_state(tenant, seen, &state)
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
    use store_postgres::TaxRateRow;

    use super::{TENANT_A, TENANT_B, block_on, prepared};

    /// A save replaces the tenant's whole table (not append), reads back class-then-channel ordered,
    /// and never touches a neighbour tenant's rows.
    #[test]
    fn replaces_wholesale_and_stays_tenant_scoped() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let rates = store.tax_rates();

            let neighbour = vec![TaxRateRow {
                tax_class_id: "class-standard".to_owned(),
                sales_channel: "SALES_CHANNEL_DINE_IN".to_owned(),
                rate_bps: 500,
            }];
            rates
                .replace(TENANT_B, &neighbour)
                .await
                .expect("set neighbour");

            let first = vec![
                TaxRateRow {
                    tax_class_id: "class-standard".to_owned(),
                    sales_channel: "SALES_CHANNEL_DINE_IN".to_owned(),
                    rate_bps: 1000,
                },
                TaxRateRow {
                    tax_class_id: "class-standard".to_owned(),
                    sales_channel: "SALES_CHANNEL_TAKEAWAY".to_owned(),
                    rate_bps: 800,
                },
            ];
            rates.replace(TENANT_A, &first).await.expect("set ours");
            let listed = rates.fetch(TENANT_A).await.expect("fetch ours");
            assert_eq!(listed.len(), 2);
            assert_eq!(
                listed.first().expect("first row").sales_channel,
                "SALES_CHANNEL_DINE_IN",
                "rows read back class-then-channel ordered"
            );

            // A second save replaces rather than appends.
            let second = vec![TaxRateRow {
                tax_class_id: "class-standard".to_owned(),
                sales_channel: "SALES_CHANNEL_DINE_IN".to_owned(),
                rate_bps: 1000,
            }];
            rates
                .replace(TENANT_A, &second)
                .await
                .expect("replace ours");
            let replaced = rates.fetch(TENANT_A).await.expect("fetch replaced");
            assert_eq!(replaced, second, "the whole table is replaced");

            // The neighbour is untouched.
            let neighbour_after = rates.fetch(TENANT_B).await.expect("fetch neighbour");
            assert_eq!(neighbour_after, neighbour);
        });
    }
}

// ---------------------------------------------------------------------------
// Campaign authoring: jsonb upsert/list/get/delete, id ordering, tenant isolation (ADR-0077).
// ---------------------------------------------------------------------------

mod campaigns {
    use super::{TENANT_A, TENANT_B, block_on, prepared};

    fn doc(name: &str) -> String {
        format!("{{\"name\":\"{name}\"}}")
    }

    /// Per-campaign upsert/get/delete over the jsonb column: rows read back id-ordered, an upsert of an
    /// existing id replaces rather than appends, a tenant cannot read another's campaign by id, and a
    /// neighbour tenant's rows are never touched.
    #[test]
    fn upsert_get_delete_stay_tenant_scoped() {
        block_on(async {
            let (store, _admin) = prepared().await.expect("prepare the database");
            let campaigns = store.campaigns();

            // A neighbour's campaign must survive our writes.
            campaigns
                .upsert(TENANT_B, "camp-z", &doc("Neighbour"))
                .await
                .expect("neighbour");

            // Insert out of id order to prove the read sorts.
            campaigns
                .upsert(TENANT_A, "camp-2", &doc("Dinner"))
                .await
                .expect("create 2");
            campaigns
                .upsert(TENANT_A, "camp-1", &doc("Lunch"))
                .await
                .expect("create 1");
            let listed = campaigns.fetch(TENANT_A).await.expect("fetch ours");
            assert_eq!(listed.len(), 2);
            assert_eq!(
                listed.first().expect("first row").campaign_id,
                "camp-1",
                "rows read back id-ordered"
            );

            // Upsert of an existing id replaces the document rather than adding a row.
            campaigns
                .upsert(TENANT_A, "camp-1", &doc("Lunch (renamed)"))
                .await
                .expect("update");
            let after = campaigns.fetch(TENANT_A).await.expect("fetch again");
            assert_eq!(
                after.len(),
                2,
                "upsert of an existing id does not add a row"
            );
            let one = campaigns
                .fetch_one(TENANT_A, "camp-1")
                .await
                .expect("fetch one")
                .expect("present");
            assert!(
                one.campaign_json.contains("Lunch (renamed)"),
                "the replaced document is what reads back"
            );

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

            // The neighbour is untouched throughout.
            assert_eq!(
                campaigns
                    .fetch(TENANT_B)
                    .await
                    .expect("fetch neighbour")
                    .len(),
                1
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
    use super::{TENANT_A, TENANT_B, block_on, prepared};

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
}

// ---------------------------------------------------------------------------
// The console audit trail: append-only, tenant-scoped, INSERT/SELECT-only (ADR-0069).
// ---------------------------------------------------------------------------

mod audit_log {
    use super::{block_on, prepared};

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
}

// ---------------------------------------------------------------------------
// The employee store: tenant-scoped, RLS-isolated, PIN held only as its hash (ADR-0070).
// ---------------------------------------------------------------------------

mod employees_store {
    use super::{block_on, prepared};

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

            // Rename + archive.
            assert!(
                people
                    .set(
                        "tenant-a",
                        "01EMP0000000000000000000A1",
                        "Alice Nguyen",
                        "archived"
                    )
                    .await
                    .expect("update"),
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
}

// ---------------------------------------------------------------------------
// Role templates + per-store assignments: people & access (ADR-0070, M1 slice 2).
// ---------------------------------------------------------------------------

mod role_templates_and_assignments {
    use super::{block_on, prepared};

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

            // Update the permission set + archive.
            assert!(
                people
                    .set_role_template(
                        "tenant-a",
                        "01ROLE000000000000000000A1",
                        "Cashier",
                        r#"["sales.item.open"]"#,
                        "archived",
                    )
                    .await
                    .expect("update"),
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
