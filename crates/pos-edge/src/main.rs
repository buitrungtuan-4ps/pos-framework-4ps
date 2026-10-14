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
//! Subcommands: `claim`, which a box installed with no store runs to claim itself
//! ([ADR-0148](../../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md));
//! `install` ([ADR-0140](../../../docs/adr/0140-a-store-pc-installs-itself-from-one-file.md));
//! `promote`, which that installer's script runs to put a newer release in place of the one a
//! re-run finds (ADR-0140 Amendment 1); and
//! `archive`, which seals, opens and checks a store archive
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
use std::process::ExitCode;
use std::sync::Arc;

use pos_edge::{
    ArchiveKey, Edge, EdgeConfig, EdgeError, EdgeSession, ServeOutcome, StoreIdentity, serve_until,
    shutdown_signal, telemetry,
};
use pos_proto::chain::ChainStatus;
use pos_proto::ids::StoreId;
use store_sqlite::SqliteStore;

#[cfg(windows)]
mod service;

fn main() -> Result<ExitCode, EdgeError> {
    telemetry::init();

    let path = std::env::var_os("POS_EDGE_CONFIG")
        .map_or_else(|| PathBuf::from("config.toml"), PathBuf::from);

    if std::env::args().any(|argument| argument == pos_edge::SELF_TEST_FLAG) {
        return self_test(&path).map(|()| ExitCode::SUCCESS);
    }

    // Before anything opens a database or binds a socket: `archive` is a one-shot tool run by a
    // technician on a bench, not the store server starting up.
    if std::env::args().nth(1).as_deref() == Some(ARCHIVE_COMMAND) {
        return archive(&std::env::args().skip(2).collect::<Vec<_>>()).map(|()| ExitCode::SUCCESS);
    }

    // `claim`, run once on a box installed from the one image for every store: it shows a code,
    // waits for the console to bind it, and writes this box's config.toml (ADR-0148).
    if std::env::args().nth(1).as_deref() == Some(pos_edge::claim::CLAIM_COMMAND) {
        return pos_edge::claim::run(&std::env::args().skip(2).collect::<Vec<_>>(), &path)
            .map(|()| ExitCode::SUCCESS);
    }

    // `promote`, which the installer script runs from the copy of the release it carries, with the
    // service stopped, to put that release in place of an older one (ADR-0140 Amendment 1).
    // Before `install`'s name check, so a tagged file handed this subcommand promotes itself.
    if std::env::args().nth(1).as_deref() == Some(pos_edge::setup::PROMOTE_COMMAND) {
        return pos_edge::setup::promote(&path).map(|()| ExitCode::SUCCESS);
    }

    // And `install`, which sets this machine up as the store's service (ADR-0140) — asked for by
    // name, or implied by a file the console named for its store (`pos-edge-setup_<cloud>_<store>`),
    // which a technician double-clicks with no arguments at all. The installed copies are named
    // `current` and `pos-edge.exe`, so the service itself never takes this path.
    if std::env::args().nth(1).as_deref() == Some(pos_edge::setup::INSTALL_COMMAND) {
        return pos_edge::setup::run(&std::env::args().skip(2).collect::<Vec<_>>())
            .map(|()| ExitCode::SUCCESS);
    }
    if pos_edge::setup::launched_as_installer() {
        return pos_edge::setup::run(&[]).map(|()| ExitCode::SUCCESS);
    }

    // On Windows, hand the main thread to the Service Control Manager when SCM is the one that
    // started us. `false` means this is an ordinary console run — a technician on the shop floor, or
    // the operator's rescue copy — and it falls through to exactly the same path Linux takes.
    #[cfg(windows)]
    if service::dispatch(path.clone())? {
        return Ok(ExitCode::SUCCESS);
    }

    // A console run: nobody is going to restart this process, so the outcome is logged and not
    // acted on. Under a service manager it is the manager's business — `systemd`'s `Restart=always`
    // does it unconditionally, and the Windows wrapper reads the outcome to decide.
    //
    // The outcome is also the exit code (ADR-0113): what a host agent that started this process in a
    // container reads, with no channel into the container at all.
    let outcome = runtime()?.block_on(run(path, shutdown_signal()))?;
    match outcome {
        ServeOutcome::RestartWanted => tracing::info!(
            "the binary on disk changed; start pos_edge again to run it (a service manager does \
             this by itself)"
        ),
        ServeOutcome::DrainIncomplete => tracing::warn!(
            "stopped with events still in the outbox; they are safe in the store's database and go \
             out at the next start, and drain-status.json says how many"
        ),
        ServeOutcome::Stopped => {}
    }
    Ok(ExitCode::from(outcome.exit_code()))
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

    // A store created before 8 KiB pages is rewritten at them once, before anything opens the file
    // (finding F5): its database shrinks to about a quarter. On a blocking thread, because a large
    // store takes a while; and never fatal, because a store on its old page size still trades.
    rebuild_page_size(&config.store_path).await;

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
    // …and the one that answers "is this log still the log I wrote", below.
    let chain = store.clone();
    let edge = Arc::new(
        Edge::new(store, identity, EdgeSession::bootstrap(), receipts)
            .map_err(EdgeError::Entropy)?,
    );

    // Replay the durable log into the projection before serving, so a restart resumes exactly where
    // the last committed transaction left off (ADR-0015, the crash-recovery half of P5).
    edge.rebuild().await.map_err(EdgeError::Rebuild)?;

    // The store verifies its own chain at startup, with no internet
    // ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 3, for
    // [ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)'s reason: a shop that
    // cannot reach the cloud must still be able to detect its own corruption). Until now the chain
    // was computed on every append, stored, and published at shift close — and never walked by
    // anything but its own tests.
    //
    // Spawned rather than awaited, which is the decision's own instruction: *"A break is reported
    // and does not stop the store trading — a till that refuses to sell because yesterday's log is
    // damaged turns a record-keeping fault into a closed shop, which is the worse failure."* So the
    // till opens on time and the walk runs beside it. The walk is chunked, so it shares the single
    // writer thread with the day's first sales instead of holding it for the length of the log.
    //
    // Logged, not raised: a break here is evidence for a person, and the cloud has its own
    // detection in the anchor (decision 4, which catches the two cases a local chain cannot).
    let store_id = config.store_id;
    tokio::spawn(async move {
        match chain.verify_chain(store_id).await {
            Ok(ChainStatus::Intact {
                checked, unchained, ..
            }) => {
                tracing::info!(%store_id, checked, unchained, "the event chain verified");
            }
            Ok(ChainStatus::Broken { at_seq, reason }) => {
                // Deliberately loud, and deliberately not fatal. The figure an operator needs is
                // the position: it says how much of the log is above suspicion.
                tracing::error!(
                    %store_id,
                    at_seq,
                    ?reason,
                    "the event chain is broken — this store's log has been altered or damaged; \
                     trading continues and the cloud anchor is the second line"
                );
            }
            Err(error) => {
                tracing::warn!(%store_id, %error, "the event chain could not be verified");
            }
        }
    });

    // Retention (ADR-0145): once an hour, the events that are synced, old and needed by nothing
    // still open are deleted. It waits ten minutes before its first sweep, so it never runs
    // across the chain walk above.
    pos_edge::retention::spawn(Arc::clone(&edge));

    // The clock (roadmap-v3 PF4): every quarter hour, measured against a time server, and the
    // reading put where `GET /api/sync` reports it. Started here rather than in `serve`, like
    // retention, so the on-fakes example and the test compositions send no datagram anywhere.
    pos_edge::sntp::spawn(Arc::clone(&edge), &config.sntp_server);

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

/// Rewrites a store database created before 8 KiB pages at 8 KiB, once, and says what it did.
async fn rebuild_page_size(database: &std::path::Path) {
    if !store_sqlite::needs_page_rebuild(database).unwrap_or(false) {
        return;
    }
    tracing::info!(
        database = %database.display(),
        "rebuilding the store database at 8 KiB pages, once; a large store takes a minute"
    );
    let path = database.to_path_buf();
    match tokio::task::spawn_blocking(move || store_sqlite::rebuild_page_size(&path)).await {
        Ok(Ok(store_sqlite::PageRebuild::Rebuilt { from, pages })) => {
            tracing::info!(from, pages, "rebuilt the store database at 8 KiB pages");
        }
        Ok(Ok(store_sqlite::PageRebuild::NotNeeded)) => {}
        Ok(Err(error)) => tracing::warn!(
            %error,
            "could not rebuild the store database at 8 KiB pages; it opens on its old page size and \
             tries again next start"
        ),
        Err(error) => tracing::warn!(%error, "the page-size rebuild did not finish"),
    }
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

/// `run` driven end to end on a temporary directory, before it is split
/// ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md), phase 7:
/// tests first). It is the real composition — `SqliteStore` on a file in the directory, the real
/// router on a loopback port — with nothing that leaves the machine: no `cloud_url`, so no cloud
/// loops, no NATS, and the SNTP server pointed at loopback. The stop is a future the test resolves,
/// standing in for the signal handler.
#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use pos_core::decision::Actor;
    use pos_edge::{
        DeviceToken, Edge, EdgeError, EdgeSession, ServeOutcome, StoreIdentity, SystemClock,
    };
    use pos_ports::device_registry::{DeviceRegistry, DeviceSession, PairedDevice};
    use pos_proto::ClockSource as _;
    use pos_proto::ids::{DeviceId, EmployeeId, StoreId};
    use pos_proto::money::{CurrencyCode, Money};
    use store_sqlite::SqliteStore;
    use tokio::sync::oneshot;

    use super::run;

    const STORE_ID: &str = "01JQ0000000000000000000001";
    const DEVICE: &str = "01JQ0000000000000000000002";
    const EMPLOYEE: &str = "01JQ0000000000000000000003";
    /// A bearer token in the form the edge issues: 32 lowercase hex characters.
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// A loopback address nothing is listening on: bound, read, released.
    fn free_address() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        listener.local_addr().expect("its address")
    }

    /// Writes a `config.toml` for one store into `dir`, binding `bind` and keeping its database in
    /// `dir`, and returns its path.
    fn config_in(dir: &Path, bind: SocketAddr) -> PathBuf {
        let store = dir.join("store.sqlite");
        let text = format!(
            "store_id = \"{STORE_ID}\"\nbind = \"{bind}\"\nstore_path = {store:?}\nsntp_server = \"127.0.0.1\"\n"
        );
        let path = dir.join("config.toml");
        std::fs::write(&path, text).expect("write config.toml");
        path
    }

    /// `GET /healthz` over a plain socket, retried until the server answers or ten seconds pass.
    async fn healthz(bind: SocketAddr) -> String {
        get(bind, "/healthz", None).await
    }

    /// `GET path` over a plain socket, as the device holding `token` where one is given, retried
    /// until the server answers or ten seconds pass.
    async fn get(bind: SocketAddr, path: &'static str, token: Option<&'static str>) -> String {
        let authorization = token.map_or_else(String::new, |token| {
            format!("Authorization: Bearer {token}\r\n")
        });
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}Connection: close\r\n\r\n"
        );
        for _ in 0..200 {
            let request = request.clone();
            let answer = tokio::task::spawn_blocking(move || {
                let mut stream =
                    TcpStream::connect_timeout(&bind, Duration::from_millis(200)).ok()?;
                stream.write_all(request.as_bytes()).ok()?;
                let mut response = String::new();
                stream.read_to_string(&mut response).ok()?;
                Some(response)
            })
            .await
            .expect("the request task");
            if let Some(response) = answer {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the edge never answered on {bind}");
    }

    /// Leaves on a stopped store's database what a day's trading would: a paired till, a person
    /// signed in on it, and a shift that person opened. Written through the library, not `run`,
    /// so the shift exists only as events in the log and in nothing a restart reads directly.
    /// Returns the shift's id.
    async fn an_open_shift_on(store_path: &Path) -> String {
        let store = SqliteStore::open(store_path).expect("open the stopped store's database");
        let device_id = DeviceId::new(DEVICE.parse().expect("a device ulid"));
        let employee_id = EmployeeId::new(EMPLOYEE.parse().expect("an employee ulid"));
        let now = SystemClock.now();
        let token = DeviceToken::parse(TOKEN).expect("a well-formed token");
        DeviceRegistry::record_pairing(
            &store,
            PairedDevice {
                device_id,
                token_digest: token.digest(),
                paired_at: now,
                admitted_by: None,
            },
        )
        .await
        .expect("record the pairing");
        DeviceRegistry::record_sign_in(
            &store,
            DeviceSession {
                device_id,
                employee_id,
                signed_in_at: now,
                last_seen_at: now,
            },
        )
        .await
        .expect("record the sign-in");
        let store_id = StoreId::new(STORE_ID.parse().expect("a store ulid"));
        let receipts = Arc::new(store.clone());
        let edge = Edge::new(
            store,
            StoreIdentity::for_store(store_id),
            EdgeSession::bootstrap(),
            receipts,
        )
        .expect("an edge on the stopped store");
        let shift = edge
            .open_shift(
                Actor {
                    employee_id,
                    device_id,
                },
                Money::new(CurrencyCode::VND, 500_000),
            )
            .await
            .expect("open a shift");
        shift.shift_id.to_string()
    }

    /// Awaits a stopped `run`, failing rather than hanging if the stop never reaches it.
    async fn stopped(
        handle: tokio::task::JoinHandle<Result<ServeOutcome, EdgeError>>,
    ) -> ServeOutcome {
        tokio::time::timeout(Duration::from_secs(30), handle)
            .await
            .expect("the edge stops within thirty seconds of being told to")
            .expect("the run task")
            .expect("a clean stop")
    }

    /// Runs `config` expecting a refusal, failing rather than hanging if the edge serves instead.
    async fn refused(config: PathBuf) -> EdgeError {
        let result =
            tokio::time::timeout(Duration::from_secs(30), run(config, std::future::pending()))
                .await
                .expect("the edge refuses within thirty seconds rather than serving");
        match result {
            Err(error) => error,
            Ok(outcome) => panic!("the edge ran and stopped ({outcome:?}) instead of refusing"),
        }
    }

    /// Starts `run` on `config`, returning the handle and the sender that stops it.
    fn start(
        config: PathBuf,
    ) -> (
        tokio::task::JoinHandle<Result<ServeOutcome, EdgeError>>,
        oneshot::Sender<()>,
    ) {
        let (stop, stopped) = oneshot::channel::<()>();
        let handle = tokio::spawn(run(config, async move {
            let _ignored = stopped.await;
        }));
        (handle, stop)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_store_is_opened_served_and_stopped_cleanly() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let bind = free_address();
        let (handle, stop) = start(config_in(dir.path(), bind));

        let response = healthz(bind).await;
        assert!(response.starts_with("HTTP/1.1 200"), "got {response}");
        assert!(
            response.contains(STORE_ID),
            "the edge serves the store its config names: {response}"
        );
        assert!(
            dir.path().join("store.sqlite").exists(),
            "the store's database is created where the config says"
        );

        stop.send(()).expect("the edge is still running");
        let outcome = stopped(handle).await;
        assert_eq!(
            outcome,
            ServeOutcome::Stopped,
            "a LAN-only store has no outbox to drain, so a stop is just a stop"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_store_started_again_on_its_own_database_rebuilds_and_serves() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let bind = free_address();
        let (handle, stop) = start(config_in(dir.path(), bind));
        assert!(healthz(bind).await.starts_with("HTTP/1.1 200"));
        stop.send(()).expect("the edge is still running");
        assert_eq!(stopped(handle).await, ServeOutcome::Stopped);

        let shift_id = an_open_shift_on(&dir.path().join("store.sqlite")).await;

        // Started again, the shift is known only because the log was replayed before serving.
        let bind = free_address();
        let (handle, stop) = start(config_in(dir.path(), bind));
        let response = get(bind, "/api/shifts/current", Some(TOKEN)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "got {response}");
        assert!(
            response.contains(&shift_id),
            "the shift opened before the restart is the current one: {response}"
        );
        stop.send(()).expect("the edge is still running");
        assert_eq!(stopped(handle).await, ServeOutcome::Stopped);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_config_file_is_refused_before_anything_opens() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let error = refused(dir.path().join("absent.toml")).await;
        assert!(
            matches!(error, EdgeError::ConfigRead { .. }),
            "got {error:?}"
        );
        assert!(
            !dir.path().join("store.sqlite").exists(),
            "no database is created for a store that could not be configured"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_config_with_an_unknown_key_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!("store_id = \"{STORE_ID}\"\nlisten = \"127.0.0.1:0\"\n"),
        )
        .expect("write config.toml");
        let error = refused(path).await;
        assert!(matches!(error, EdgeError::ConfigParse(_)), "got {error:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_database_that_cannot_be_opened_is_a_store_error() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let bind = free_address();
        let path = config_in(dir.path(), bind);
        // A directory where the database file should be: SQLite cannot open it.
        std::fs::create_dir(dir.path().join("store.sqlite")).expect("make the directory");
        let error = refused(path).await;
        assert!(matches!(error, EdgeError::Store(_)), "got {error:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_address_already_in_use_is_a_bind_error() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let taken = TcpListener::bind("127.0.0.1:0").expect("hold a port");
        let bind = taken.local_addr().expect("its address");
        let error = refused(config_in(dir.path(), bind)).await;
        assert!(
            matches!(error, EdgeError::Bind { addr, .. } if addr == bind),
            "got {error:?}"
        );
    }
}
