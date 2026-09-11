// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos_edge` entry point.
//!
//! Deliberately tiny: install logging, load the bootstrap config, open the SQLite store, compose the
//! application [`Edge`], serve. Everything testable lives in the [`pos_edge`] library. The config
//! path comes from `POS_EDGE_CONFIG`, defaulting to `config.toml`; on a real store it is written at
//! activation. To run the edge on fakes with no config file, use the `minimal-edge` example
//! (`just run-edge`).
//!
//! One flag: `--self-test`, which the over-the-air installer runs against a *staged* binary before
//! swapping it in ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md) Amendment 1).
//!
//! One subcommand: `archive`, which seals, opens and checks a store archive
//! ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md)). It is here rather than in
//! a tool of its own because the place a store archive is restored *to* is a till, and a till has
//! this binary on it already. See [`archive`] for the shape.
//!
//! # Why `main` is not `#[tokio::main]`
//!
//! On Windows the process may be started by the Service Control Manager, which requires the **main
//! thread** to be handed to a blocking dispatcher before anything else happens (roadmap v3 **E4**,
//! [`service`]). A main already inside an async runtime has no main thread left to hand over. So the
//! runtime is built explicitly, by whichever of the two entry paths turns out to be the real one.

use std::path::PathBuf;
use std::sync::Arc;

use pos_edge::{
    ArchiveKey, Edge, EdgeConfig, EdgeError, EdgeSession, ServeOutcome, StoreIdentity, serve_until,
    shutdown_signal, telemetry,
};
use pos_proto::ids::StoreId;
use store_sqlite::SqliteStore;

#[cfg(windows)]
mod service;

fn main() -> Result<(), EdgeError> {
    telemetry::init();

    let path = std::env::var_os("POS_EDGE_CONFIG")
        .map_or_else(|| PathBuf::from("config.toml"), PathBuf::from);

    if std::env::args().any(|argument| argument == pos_edge::SELF_TEST_FLAG) {
        return self_test(&path);
    }

    // Before anything opens a database or binds a socket: `archive` is a one-shot tool run by a
    // technician on a bench, not the store server starting up.
    if std::env::args().nth(1).as_deref() == Some(ARCHIVE_COMMAND) {
        return archive(&std::env::args().skip(2).collect::<Vec<_>>());
    }

    // On Windows, hand the main thread to the Service Control Manager when SCM is the one that
    // started us. `false` means this is an ordinary console run — a technician on the shop floor, or
    // the operator's rescue copy — and it falls through to exactly the same path Linux takes.
    #[cfg(windows)]
    if service::dispatch(path.clone())? {
        return Ok(());
    }

    // A console run: nobody is going to restart this process, so the outcome is logged and not
    // acted on. Under a service manager it is the manager's business — `systemd`'s `Restart=always`
    // does it unconditionally, and the Windows wrapper reads the outcome to decide.
    let outcome = runtime()?.block_on(run(path, shutdown_signal()))?;
    if outcome == ServeOutcome::RestartWanted {
        tracing::info!(
            "the binary on disk changed; start pos_edge again to run it (a service manager does \
             this by itself)"
        );
    }
    Ok(())
}

/// The multi-threaded runtime the edge serves on.
///
/// Built by hand rather than by `#[tokio::main]` so that the Windows service dispatcher can own the
/// main thread first; the configuration is the attribute's own default.
///
/// # Errors
///
/// [`EdgeError::Runtime`] if the runtime's threads or I/O driver could not be created.
fn runtime() -> Result<tokio::runtime::Runtime, EdgeError> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(EdgeError::Runtime)
}

/// Opens the store, composes the edge and serves it until `stop` resolves or an installed update
/// asks for a restart.
///
/// Shared by both entry paths — the console run above and the Windows service wrapper — so a store
/// started by the Service Control Manager is composed exactly like one started from a terminal. The
/// only difference between them is what a stop *is*, which is why that arrives as an argument.
///
/// # Errors
///
/// Whatever loading the config, opening the store, rebuilding the projection or serving reports.
async fn run<F>(path: PathBuf, stop: F) -> Result<ServeOutcome, EdgeError>
where
    F: Future<Output = ()> + Send + 'static,
{
    let config = EdgeConfig::load(&path)?;

    // The real edge stores events in SQLite (ADR-0015); the example uses the in-memory fakes.
    let store = SqliteStore::open(&config.store_path).map_err(EdgeError::Store)?;
    let identity = StoreIdentity::for_store(config.store_id);
    // The store's own writer thread is the gapless receipt authority (ADR-0025); a clone shares it,
    // so the loop that appends the settled event and the authority that numbers it are one store.
    let receipts = Arc::new(store.clone());
    // The same single writer thread is the durable daily queue-number authority (ADR-0064), so a
    // relayed takeaway order gets a number that survives a restart; another clone carries it into
    // `serve`, which builds the relay's intake from it (ADR-0087).
    let queue = store.clone();
    // And the same writer thread is the durable OTA self-test authority (ADR-0048's
    // highest-precedence rule reads it, and an install deliberately restarts the edge, so the
    // verdict has to be on disk rather than in process memory — ADR-0055 Amendment 1).
    let ota_state = store.clone();
    // …and the durable lease authority (ADR-0108). The generation this box holds must survive the
    // same restart for the same reason: a held generation rebuilt from config on every boot would be
    // re-adopted on every boot, and a machine a replacement superseded would promote itself back.
    let lease_state = store.clone();
    // …and the durable record of which paired device answers for which terminal (ADR-0112). Same
    // reason again: the binding is a managerial act performed once at the box, and re-doing it after
    // every restart would mean a manager at the till in the middle of service.
    let print_agents = store.clone();
    // …and the durable print queue itself (ADR-0112). Same writer thread again, and it must be the
    // same *store*: the dispatch that enqueues a ticket and the route the agent claims it from are
    // two halves of one table.
    let print_queue = store.clone();
    let edge = Arc::new(
        Edge::new(store, identity, EdgeSession::bootstrap(), receipts)
            .map_err(EdgeError::Entropy)?,
    );

    // Replay the durable log into the projection before serving, so a restart resumes exactly where
    // the last committed transaction left off (ADR-0015, the crash-recovery half of P5).
    edge.rebuild().await.map_err(EdgeError::Rebuild)?;

    serve_until(
        config,
        edge,
        queue,
        ota_state,
        lease_state,
        print_agents,
        print_queue,
        stop,
    )
    .await
}

/// The pre-commit smoke test the OTA installer runs against a *staged* binary: can these bytes run
/// on this box, and can they read this store's configuration?
///
/// It answers the questions that are worth answering before a swap — the wrong architecture, a
/// truncated download, a missing shared library, a config the new version's parser rejects — and it
/// does so by the act of getting this far: reaching `main` means the loader accepted the executable.
///
/// **It deliberately does not open the database.** The binary being tested is a *second* process
/// while the running edge still owns that file, and `SqliteStore::open` migrates; two writers and a
/// schema change against a live store is a worse risk than the coverage it would buy. Whether this
/// version can come up for real is the question the boot confirmation answers, after the swap, in
/// the only process that owns the store (ADR-0055 Amendment 1).
fn self_test(config_path: &std::path::Path) -> Result<(), EdgeError> {
    let config = EdgeConfig::load(config_path)?;
    tracing::info!(
        version = pos_edge::VERSION,
        store_id = %config.store_id,
        "self-test passed: this binary runs and reads this store's configuration"
    );
    Ok(())
}

/// The subcommand that seals, opens and checks a store archive.
const ARCHIVE_COMMAND: &str = "archive";

/// Where the archive key is read from.
///
/// An environment variable and not an argument, deliberately: an argument is in `ps` output, in
/// the shell history, and in the terminal recording of the incident everybody is watching. The key
/// that opens a shop's personal data does not go there.
const ARCHIVE_KEY_VAR: &str = "POS_EDGE_ARCHIVE_KEY";

/// `pos-edge archive` — the store side of [ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md).
///
/// ```text
/// POS_EDGE_ARCHIVE_KEY=<64 hex characters> pos-edge archive <verb> --store <ULID> [paths]
///
///   seal   --database <store.sqlite> --archive <out>   snapshot a store and seal it
///   open   --archive <in> --into <store.sqlite>        restore an archive to a database
///   verify --archive <in>                              open it, check it, say what is inside
/// ```
///
/// `verify` is the drill: it proves an archive is not merely present but *loadable*, which is the
/// only thing that makes it a backup ([ADR-0046](../../../docs/adr/0046-backups-and-restore.md)).
///
/// # Errors
///
/// [`EdgeError::Archive`] if an argument is missing or wrong, the key is absent or malformed, or
/// the archive does not open.
fn archive(arguments: &[String]) -> Result<(), EdgeError> {
    let verb = arguments.first().map(String::as_str).unwrap_or_default();
    let store = StoreId::new(
        parse_ulid(&flag(arguments, "--store")?)
            .ok_or_else(|| EdgeError::Archive("--store is not a ULID".to_owned()))?,
    );
    let key = ArchiveKey::parse(
        &std::env::var(ARCHIVE_KEY_VAR)
            .map_err(|_error| EdgeError::Archive(format!("set {ARCHIVE_KEY_VAR}")))?,
    )
    .map_err(|error| EdgeError::Archive(error.to_string()))?;

    match verb {
        "seal" => {
            let database = PathBuf::from(flag(arguments, "--database")?);
            let destination = PathBuf::from(flag(arguments, "--archive")?);
            let work = tempdir_beside(&destination)?;
            let snapshot = work.join("snapshot.sqlite");
            // `VACUUM INTO` refuses an existing file, so clear whatever an interrupted run left.
            let _ = std::fs::remove_file(&snapshot);
            store_sqlite::snapshot_to(&database, &snapshot).map_err(EdgeError::Store)?;
            let sealed = pos_edge::backup::seal_file(&snapshot, &key, store)
                .map_err(|error| EdgeError::Archive(error.to_string()))?;
            let _ = std::fs::remove_file(&snapshot);
            let _ = std::fs::remove_dir(&work);
            std::fs::write(&destination, &sealed)
                .map_err(|error| EdgeError::Archive(error.to_string()))?;
            tracing::info!(
                bytes = sealed.len(),
                archive = %destination.display(),
                "sealed this store's archive"
            );
            Ok(())
        }
        "open" => {
            let sealed = read_archive(arguments)?;
            let destination = PathBuf::from(flag(arguments, "--into")?);
            let bytes = pos_edge::backup::open_file(&sealed, &key, store, &destination)
                .map_err(|error| EdgeError::Archive(error.to_string()))?;
            tracing::info!(
                bytes,
                database = %destination.display(),
                "opened the archive; check it with `archive verify` before serving from it"
            );
            Ok(())
        }
        "verify" => {
            let source = PathBuf::from(flag(arguments, "--archive")?);
            let sealed = read_archive(arguments)?;
            // Beside the archive, not in the working directory: a technician runs this from
            // wherever they happen to be, and a restored database is the size of a database.
            let work = tempdir_beside(&source)?;
            let restored = work.join("verify.sqlite");
            let _ = std::fs::remove_file(&restored);
            let bytes = pos_edge::backup::open_file(&sealed, &key, store, &restored)
                .map_err(|error| EdgeError::Archive(error.to_string()))?;
            let verdict = store_sqlite::integrity_check(&restored).map_err(EdgeError::Store)?;
            let _ = std::fs::remove_file(&restored);
            let _ = std::fs::remove_dir(&work);
            if verdict == "ok" {
                tracing::info!(
                    bytes,
                    "the archive opens and the database inside it is sound"
                );
                Ok(())
            } else {
                Err(EdgeError::Archive(format!(
                    "the archive opened but the database inside it is damaged: {verdict}"
                )))
            }
        }
        other => Err(EdgeError::Archive(format!(
            "unknown archive verb {other:?}; expected seal, open or verify"
        ))),
    }
}

fn read_archive(arguments: &[String]) -> Result<Vec<u8>, EdgeError> {
    std::fs::read(flag(arguments, "--archive")?)
        .map_err(|error| EdgeError::Archive(error.to_string()))
}

/// The value that follows `name`.
fn flag(arguments: &[String], name: &str) -> Result<String, EdgeError> {
    arguments
        .iter()
        .position(|argument| argument == name)
        .and_then(|at| arguments.get(at + 1))
        .cloned()
        .ok_or_else(|| EdgeError::Archive(format!("{name} <value> is required")))
}

/// A scratch directory next to where the caller is writing, so a multi-gigabyte restore does not
/// land on a small `/tmp` and so the temporary file is on the same filesystem as its destination.
fn tempdir_beside(destination: &std::path::Path) -> Result<PathBuf, EdgeError> {
    let parent = destination.parent().unwrap_or(std::path::Path::new("."));
    let work = parent.join(".pos-edge-archive");
    std::fs::create_dir_all(&work).map_err(|error| EdgeError::Archive(error.to_string()))?;
    Ok(work)
}

/// A ULID from its 26-character text, or `None`.
fn parse_ulid(text: &str) -> Option<pos_proto::ulid::Ulid> {
    text.parse().ok()
}
