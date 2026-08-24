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
                 config_trees, subjects, webhook_endpoints, device_proposals, activation_codes, \
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
             config_trees, subjects, webhook_endpoints, device_proposals, activation_codes, \
             device_credentials RESTART IDENTITY",
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
            sessions
                .insert_session(&hash, 2000)
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
