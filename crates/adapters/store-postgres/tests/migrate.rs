// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! [`PostgresStore::migrate`], held to what it promises before its body is split
//! ([ADR-0170](../../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md)).
//!
//! `migrate` runs on every boot of the cloud, against a database that is empty the first time and
//! fully migrated every time after. Three things must stay true however its body is arranged:
//!
//!  1. On an empty database it builds exactly the schema the files in `migrations/` describe,
//!     applied in the order their names sort in — no file skipped, none applied out of turn.
//!  2. Run again, it changes nothing: not the schema, not a row of what the first run seeded.
//!  3. What was written between two runs survives the second.
//!
//! Each live case creates a database of its own and drops it afterwards, so these cases neither
//! need nor disturb the shared database `tests/integration.rs` truncates between its cases. They
//! need a role that may create databases, which the CI image's `pos` superuser is.
//!
//! The one case that needs no database — an unreachable database is reported as unavailable —
//! runs in every `cargo test`; the rest are gated behind the `integration` feature, as
//! `tests/integration.rs` is:
//!
//! ```text
//! DATABASE_URL='host=localhost port=55432 user=pos dbname=poscloud' \
//!   cargo test -p store-postgres --features integration -- --test-threads=1
//! ```

#![allow(
    clippy::expect_used,
    reason = "test scaffolding: an unreachable database or a statement that fails while setting a \
              case up is an unrecoverable test-setup fault, not a migration failure"
)]

use std::future::Future;

use pos_proto::error::ErrorStatus;
use store_postgres::PostgresStore;

/// Drives a future on a fresh multi-thread runtime with IO enabled: `tokio-postgres` needs the IO
/// driver for its socket, and `deadpool` spawns each connection's driver task.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a multi-thread tokio runtime")
        .block_on(future)
}

/// A database that cannot be reached is the one failure every boot can meet, and the cloud's
/// startup reads it as "try again", not as a broken schema: `migrate` reports it unavailable.
#[test]
fn an_unreachable_database_is_reported_unavailable() {
    // Port 1 on the loopback refuses at once; nothing listens there.
    let store = PostgresStore::connect("host=127.0.0.1 port=1 user=pos dbname=poscloud")
        .expect("a well-formed connection string builds a pool");
    let refused = block_on(store.migrate()).expect_err("nothing is listening");
    assert_eq!(refused.status(), ErrorStatus::Unavailable);
    assert!(refused.is_retryable());
}

#[cfg(feature = "integration")]
mod live {
    use std::hash::{BuildHasher, Hasher, RandomState};
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    use pos_contract_tests::fixtures;
    use pos_ports::{EventStore, Transactional, TxContext};
    use pos_proto::{StoreId, Ulid};
    use store_postgres::PostgresStore;
    use tokio_postgres::{Client, NoTls};

    use super::block_on;

    /// The libpq connection string of the shared database, which every scratch database is
    /// created from.
    ///
    /// # Panics
    ///
    /// If `DATABASE_URL` is unset: the `integration` feature is only enabled where a database is.
    fn database_url() -> String {
        std::env::var("DATABASE_URL").expect(
            "DATABASE_URL must be set for the `integration` feature — e.g. \
             host=localhost port=55432 user=pos dbname=poscloud",
        )
    }

    /// Opens a plain connection and spawns its driver.
    async fn client(url: &str) -> Client {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .expect("connect to PostgreSQL");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    }

    /// An empty database of this case's own, dropped when the case ends.
    struct Scratch {
        name: String,
    }

    impl Scratch {
        /// Creates an empty database whose name no other run can share: a random suffix drawn
        /// from the operating system's entropy (through `RandomState`, which seeds itself from
        /// it), this process's id, and a counter within the process. A rerun, or a second job
        /// against the same server, makes names of its own.
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let random = RandomState::new().build_hasher().finish();
            let name = format!(
                "migrate_{random:016x}_{}_{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            block_on(async {
                client(&database_url())
                    .await
                    .batch_execute(&format!("CREATE DATABASE {name}"))
                    .await
                    .expect("create a scratch database");
            });
            Self { name }
        }

        /// The connection string for this database: the shared one, with its `dbname` replaced.
        /// A later key overrides an earlier one in libpq's key=value form.
        fn url(&self) -> String {
            format!("{} dbname={}", database_url(), self.name)
        }
    }

    /// Drops the database when the case ends, however it ends: a failed assertion unwinds through
    /// this too, so a red run leaves nothing behind on the server. Nothing here panics, since a
    /// panic while unwinding would abort the run; a database that cannot be dropped is left to
    /// the next cleanup of the server rather than turned into a second failure.
    impl Drop for Scratch {
        fn drop(&mut self) {
            let statement = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name);
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                if let Ok((admin, connection)) =
                    tokio_postgres::connect(&database_url_or_none()?, NoTls).await
                {
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });
                    let _ = admin.batch_execute(&statement).await;
                }
                Some(())
            });
        }
    }

    /// [`database_url`] without the panic, for the cleanup that runs while unwinding.
    fn database_url_or_none() -> Option<String> {
        std::env::var("DATABASE_URL").ok()
    }

    /// Every file in `migrations/`, in the order their names sort in.
    fn migration_files() -> Vec<(String, String)> {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut files: Vec<(String, String)> = std::fs::read_dir(&directory)
            .expect("read the migrations directory")
            .map(|entry| entry.expect("a directory entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
            .map(|path| {
                let name = path
                    .file_name()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned();
                let sql = std::fs::read_to_string(&path).expect("read a migration");
                (name, sql)
            })
            .collect();
        files.sort();
        files
    }

    /// The schema as the catalog describes it, one sorted line per fact: every table and view
    /// with its columns, every constraint, index, policy, trigger and function, whether a table
    /// enforces row-level security, and what each role is granted on each table.
    ///
    /// Two databases whose fingerprints are equal have the same schema for every purpose a
    /// migration has ever served.
    async fn fingerprint(client: &Client) -> Vec<String> {
        const QUERIES: &[&str] = &[
            "SELECT 'column ' || table_name || '.' || column_name || ' ' || data_type || \
             ' null=' || is_nullable || ' default=' || coalesce(column_default, '-') \
             FROM information_schema.columns WHERE table_schema = 'public'",
            "SELECT 'relation ' || c.relname || ' kind=' || c.relkind::text || ' rls=' || \
             c.relrowsecurity || ' forced=' || c.relforcerowsecurity \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'v', 'm', 'S')",
            "SELECT 'partition ' || c.relname || ' of ' || p.relname || ' ' || \
             pg_get_expr(c.relpartbound, c.oid) \
             FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid \
             JOIN pg_class p ON p.oid = i.inhparent \
             JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public'",
            "SELECT 'constraint ' || cl.relname || '.' || co.conname || ' ' || \
             pg_get_constraintdef(co.oid) \
             FROM pg_constraint co JOIN pg_class cl ON cl.oid = co.conrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace WHERE n.nspname = 'public'",
            "SELECT 'index ' || indexdef FROM pg_indexes WHERE schemaname = 'public'",
            "SELECT 'policy ' || tablename || '.' || policyname || ' ' || cmd || ' to=' || \
             array_to_string(roles, ',') || ' using=' || coalesce(qual, '-') || \
             ' check=' || coalesce(with_check, '-') \
             FROM pg_policies WHERE schemaname = 'public'",
            "SELECT 'trigger ' || pg_get_triggerdef(t.oid) \
             FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND NOT t.tgisinternal",
            "SELECT 'function ' || pg_get_functiondef(p.oid) \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'public'",
            "SELECT 'grant ' || grantee || ' ' || privilege_type || ' on ' || table_name \
             FROM information_schema.role_table_grants WHERE table_schema = 'public'",
        ];
        let mut lines = Vec::new();
        for query in QUERIES {
            // As text whatever the catalog's own type, and a fact that reads as NULL as a word.
            let query = format!("SELECT coalesce(fact::text, '<null>') FROM ({query}) AS q(fact)");
            for row in client.query(&query, &[]).await.expect("read the catalog") {
                lines.push(row.get::<_, String>(0));
            }
        }
        lines.sort();
        lines
    }

    /// How many rows each table in `public` holds, one sorted line per table: what a migration
    /// seeded, and what was written since.
    async fn row_counts(client: &Client) -> Vec<String> {
        let tables = client
            .query(
                "SELECT c.relname FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') AND NOT c.relispartition \
                 ORDER BY c.relname",
                &[],
            )
            .await
            .expect("list the tables");
        let mut counts = Vec::new();
        for table in tables {
            let name: String = table.get(0);
            let count: i64 = client
                .query_one(&format!("SELECT count(*) FROM \"{name}\""), &[])
                .await
                .expect("count a table's rows")
                .get(0);
            counts.push(format!("{name} {count}"));
        }
        counts
    }

    /// `migrate` on an empty database builds what applying every file in `migrations/`, in name
    /// order, builds — so a file left out of `migrate`, or applied out of turn where the order
    /// shows, fails here.
    #[test]
    fn an_empty_database_is_built_as_the_migration_files_describe() {
        let migrated = Scratch::new();
        let by_hand = Scratch::new();
        let files = migration_files();
        assert!(
            !files.is_empty(),
            "the migrations directory holds the schema"
        );
        block_on(async {
            PostgresStore::connect(&migrated.url())
                .expect("a pool over the scratch database")
                .migrate()
                .await
                .expect("migrate an empty database");
            let applied = client(&by_hand.url()).await;
            for (name, sql) in &files {
                applied
                    .batch_execute(sql)
                    .await
                    .unwrap_or_else(|error| panic!("apply {name} by hand: {error}"));
            }
            let from_migrate = fingerprint(&client(&migrated.url()).await).await;
            let from_files = fingerprint(&applied).await;
            assert!(
                from_migrate.len() > files.len(),
                "the fingerprint reads the schema rather than nothing"
            );
            assert_eq!(from_migrate, from_files);
            assert_eq!(
                row_counts(&client(&migrated.url()).await).await,
                row_counts(&applied).await,
                "the rows the migrations seed are the same either way"
            );
        });
    }

    /// The cloud runs `migrate` on every boot, so a second run on a migrated database must be a
    /// no-op: the same schema, and the rows the first run seeded neither doubled nor lost.
    #[test]
    fn a_second_run_changes_nothing() {
        let scratch = Scratch::new();
        block_on(async {
            let store = PostgresStore::connect(&scratch.url()).expect("a pool");
            store.migrate().await.expect("the first run");
            let admin = client(&scratch.url()).await;
            let schema = fingerprint(&admin).await;
            let rows = row_counts(&admin).await;

            store.migrate().await.expect("the second run");
            assert_eq!(fingerprint(&admin).await, schema);
            assert_eq!(row_counts(&admin).await, rows);
        });
    }

    /// A database migrated part of the way — a cloud upgraded across several releases at once —
    /// is brought the rest of the way: to the schema an empty database is built to, and with the
    /// rows it already held put right by the migrations it had not run. The row here is a device
    /// proposal in the connection spelling `0055` rewrites, a data fix that leaves no record of
    /// its own, so only a database that held the old spelling shows whether it ran.
    #[test]
    fn a_database_migrated_part_of_the_way_is_brought_the_rest_with_its_rows() {
        let part_way = Scratch::new();
        let from_empty = Scratch::new();
        block_on(async {
            let admin = client(&part_way.url()).await;
            for (name, sql) in migration_files()
                .iter()
                .filter(|(name, _)| name.as_str() < "0055")
            {
                admin
                    .batch_execute(sql)
                    .await
                    .unwrap_or_else(|error| panic!("apply {name} by hand: {error}"));
            }
            admin
                .batch_execute(
                    "INSERT INTO device_proposals \
                     (id, tenant_id, store_id, kind, name, address, connection) \
                     VALUES ('proposal-1', 'tenant-a', 'store-1', 'PRINTER', 'Bar printer', \
                     '192.0.2.10:9100', 'DEVICE_CONNECTION_TCP')",
                )
                .await
                .expect("seed a proposal in the old spelling");

            PostgresStore::connect(&part_way.url())
                .expect("a pool")
                .migrate()
                .await
                .expect("migrate the rest of the way");
            PostgresStore::connect(&from_empty.url())
                .expect("a pool")
                .migrate()
                .await
                .expect("migrate an empty database");

            let connection: String = admin
                .query_one(
                    "SELECT connection FROM device_proposals WHERE id = 'proposal-1'",
                    &[],
                )
                .await
                .expect("read the proposal back")
                .get(0);
            assert_eq!(connection, "tcp");
            assert_eq!(
                fingerprint(&admin).await,
                fingerprint(&client(&from_empty.url()).await).await
            );
        });
    }

    /// What the cloud wrote between two boots is still there after the second boot's `migrate`.
    #[test]
    fn what_was_written_between_two_runs_survives_the_second() {
        let scratch = Scratch::new();
        block_on(async {
            let store = PostgresStore::connect(&scratch.url()).expect("a pool");
            store.migrate().await.expect("the first run");
            let store_id = StoreId::new(Ulid::from_u128(0x0123));
            let events: Vec<_> = (1..=3)
                .map(|seed| fixtures::activation(store_id, seed))
                .collect();
            let mut tx = store.begin().await.expect("begin");
            store.append(&mut tx, &events).await.expect("append");
            tx.commit().await.expect("commit");
            let admin = client(&scratch.url()).await;
            let rows = row_counts(&admin).await;

            store.migrate().await.expect("the second run");
            assert_eq!(row_counts(&admin).await, rows);
            let held: i64 = admin
                .query_one("SELECT count(*) FROM events", &[])
                .await
                .expect("count the events")
                .get(0);
            assert_eq!(held, 3);
        });
    }
}
