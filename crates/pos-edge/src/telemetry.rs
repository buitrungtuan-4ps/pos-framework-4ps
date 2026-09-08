// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `tracing` subscriber.
//!
//! # The one rule
//!
//! Store logs travel to the cloud for the remote tail
//! ([`docs/architecture.md`](../../../docs/architecture.md)), so a log line is data that leaves the
//! store. It therefore records **identifiers and counts, never PII** — no guest name, phone, address
//! or card detail ever enters a span or an event. [`pos_proto::pii`] marks the values this rule is
//! about; the discipline here is to log the id, not the person. The workspace already forbids
//! `println!`/`eprintln!` in this crate, so `tracing` is the only way out and this is the only place
//! it is configured.
//!
//! # The durable sink, and why it is opt-in
//!
//! A Windows service started by the Service Control Manager has no console, so stdout is discarded
//! and every diagnostic with it — including the pairing code, which is the *only* way a device joins
//! the store. That made a headless store unbootable rather than merely undiagnosable
//! ([ADR-0117](../../../docs/adr/0117-a-headless-store-keeps-a-log.md)).
//!
//! Setting [`LOG_FILE_VARIABLE`] tees the stream into that file as well. Unset — or when this
//! process is an over-the-air `--self-test` child — the builder is byte-for-byte what it was before
//! that record, so journald stays the only sink on Linux, `just run-edge` is untouched, and nothing
//! double-logs.
//!
//! # What the writer refuses to make durable
//!
//! Two guards, both on the writer rather than on a reviewer's memory:
//!
//! * [`MakeWriterExt::with_max_level`] pins the *file* at `INFO` while stdout keeps whatever
//!   `RUST_LOG` admits. An operator raising `RUST_LOG=debug` to diagnose pairing — which is exactly
//!   what an operator does — therefore cannot make a `DEBUG` span durable.
//! * [`MakeWriterExt::with_filter`] excludes [`EXCLUDED_TARGETS`]: the pairing announcement, and
//!   both halves of employee authentication. The pairing code stays out because ADR-0030 says a
//!   pairing code is never logged and this is what finally makes that true. The auth targets stay
//!   out because a durable per-employee sign-in and wrong-PIN stream is an attendance-and-failed-auth
//!   record outside `SubjectStore` — invisible to ADR-0035's masking and ADR-0076's erasure, and an
//!   employee-monitoring surface that would need a lawful basis, staff notification and a DPIA. This
//!   process creates none of that, and that is a decision rather than an accident.
//!
//! # Bounds, not retention
//!
//! The file holds the current run plus one previous run: [`init`] renames the old file aside before
//! opening a new one. Within a run it is capped at [`RUN_BYTE_CAP`], which is a **disk-full guard**
//! and not a retention period — a store issued an under-scoped API key logs a `403` every few
//! seconds on a box nobody restarts, and the disk it would fill is the one holding `store.sqlite`.
//! ADR-0035's periods are about personal data in `SubjectStore`; this file carries identifiers,
//! counts and outcomes.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::{Level, Metadata};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt as _;

/// The environment variable that names the durable log file, and by being set turns it on.
///
/// The generated installers set it to a path under the service's own state directory; nothing under
/// `deploy/` sets it on Linux, where journald already captures the stream.
pub const LOG_FILE_VARIABLE: &str = "POS_EDGE_LOG_FILE";

/// How many bytes one run may write to the durable file before it stops writing to it.
///
/// A guard against filling the volume that holds `store.sqlite`, not a retention rule. A run that
/// reaches it keeps logging to stdout; it does not rotate mid-run, because a per-event bound would
/// need a lock on the path every till request crosses.
const RUN_BYTE_CAP: u64 = 8 * 1024 * 1024;

/// The suffix the previous run's file is renamed to at start-up.
const PREVIOUS_RUN_SUFFIX: &str = ".1";

/// Targets whose events reach stdout and journald but never the durable file.
///
/// [`crate::pairing::ANNOUNCE_TARGET`] carries the pairing code (ADR-0030); the two auth targets
/// carry which employee signed in, mistyped a PIN or was locked out (ADR-0117 decision 6). A target
/// matches when it is equal to an entry or is a `::`-separated descendant of one.
///
/// Every entry is the module's own declaration of its target rather than a string spelled out here,
/// so moving or renaming one of those modules cannot silently make its stream durable.
pub const EXCLUDED_TARGETS: [&str; 3] = [
    crate::pairing::ANNOUNCE_TARGET,
    crate::http::auth::LOG_TARGET,
    crate::auth::LOG_TARGET,
];

/// Whether an event on `target` may be written to the durable file.
fn is_durable(target: &str) -> bool {
    !EXCLUDED_TARGETS.iter().any(|excluded| {
        target.starts_with(excluded)
            && matches!(target.as_bytes().get(excluded.len()), None | Some(b':'))
    })
}

/// Installs the process-wide log subscriber, once.
///
/// Reads the filter from `RUST_LOG`, defaulting to `info`. Calling it more than once (as a test that
/// spins up several servers might) is harmless: the second install fails quietly and the first
/// subscriber stands.
///
/// With [`LOG_FILE_VARIABLE`] set the stream is also written to that file, under the two guards this
/// module documents. Every failure on that path — an unwritable directory, a read-only volume, a
/// path that is a directory — falls back to stdout only and says so *after* the subscriber is
/// installed, so the warning is itself logged. Nothing here panics: a store that cannot open a log
/// file must still be able to sell.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_unset| EnvFilter::new("info"));

    let Some(path) = durable_log_path() else {
        // `try_init` returns `Err` if a global subscriber is already set. That is the idempotent
        // case, not a failure worth surfacing, so it is discarded deliberately — here and below.
        let _ignored = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .try_init();
        return;
    };

    match open_durable(&path) {
        Ok(durable) => {
            // stdout first: `Tee` writes both halves before it propagates either error, so the file
            // still receives the whole line when stdout is the null handle a Windows service gets.
            let sink = io::stdout.and(
                Arc::new(durable)
                    .with_max_level(Level::INFO)
                    .with_filter(|meta: &Metadata<'_>| is_durable(meta.target())),
            );
            // ANSI is a property of the fmt layer and not of one sink, so a file that would
            // otherwise fill with escape sequences costs the console its colour. Per-sink ANSI needs
            // the layered form; this is the trade ADR-0117 records.
            let _ignored = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(sink)
                .try_init();
            tracing::info!(
                log_file = %path.display(),
                cap_bytes = RUN_BYTE_CAP,
                "logging to this file as well as to standard output"
            );
        }
        Err(error) => {
            let _ignored = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .try_init();
            tracing::warn!(
                log_file = %path.display(),
                %error,
                variable = LOG_FILE_VARIABLE,
                "the log file could not be opened; logging to standard output only — on a headless \
                 box that means no pairing code and no diagnostics"
            );
        }
    }
}

/// The durable file's path, or `None` when there is not to be one.
///
/// `None` for an over-the-air `--self-test` child as well as for an unset or empty variable: that
/// process is a second `pos-edge` running against the same state directory as the live one, and two
/// processes rotating and truncating one file would cost the running store its log.
fn durable_log_path() -> Option<PathBuf> {
    if std::env::args_os().any(|argument| argument == crate::installer::SELF_TEST_FLAG) {
        return None;
    }
    let raw = std::env::var_os(LOG_FILE_VARIABLE)?;
    (!raw.is_empty()).then(|| PathBuf::from(raw))
}

/// Rotates the previous run's file aside and opens a fresh one.
///
/// Creates the parent directory, because the installer names a path under a state directory the
/// service may reach before anything else has created it.
///
/// # Errors
///
/// Whatever creating the directory or opening the file reports. The caller falls back to stdout.
fn open_durable(path: &Path) -> io::Result<CappedFile> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    rotate(path);

    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    // Owner-only, on the platform that has a mode. A pairing *URL* never reaches this file, but the
    // installer's own stated intent is a log readable by whoever is diagnosing the box, so this is
    // the narrowest mode that keeps that true for the service account.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    Ok(CappedFile {
        file: options.open(path)?,
        written: AtomicU64::new(0),
    })
}

/// Moves the previous run's file to `<path>.1`, best effort.
///
/// Deliberately silent: the first run of a fresh install has nothing to rename, and a rename that
/// fails for any other reason is not a reason to refuse to log. The truncating open below is what
/// guarantees this run does not append to the last one either way.
fn rotate(path: &Path) {
    let mut previous = path.as_os_str().to_owned();
    previous.push(PREVIOUS_RUN_SUFFIX);
    let _ignored = fs::rename(path, PathBuf::from(previous));
}

/// The durable file, and how many bytes this run has put in it.
///
/// The counter is what makes the cap a cap. It is `Relaxed` on purpose: two threads logging at the
/// same instant may between them cross the boundary by one line, which is the right amount of
/// precision for a disk-full guard and costs nothing on the path a till request crosses.
#[derive(Debug)]
struct CappedFile {
    /// The open file. Written through `&File`, so the writer needs no lock of its own.
    file: File,
    /// Bytes this run has written, compared against [`RUN_BYTE_CAP`].
    written: AtomicU64,
}

impl io::Write for &CappedFile {
    /// Writes `buf` to the file until the run's cap is reached, and reports success afterwards.
    ///
    /// Reporting success past the cap is deliberate: an `Err` would propagate out of the `Tee` and
    /// be indistinguishable from a genuinely broken sink, and the line has already gone to stdout.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written.load(Ordering::Relaxed) >= RUN_BYTE_CAP {
            return Ok(buf.len());
        }
        let mut file = &self.file;
        let wrote = file.write(buf)?;
        self.written.fetch_add(wrote as u64, Ordering::Relaxed);
        Ok(wrote)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut file = &self.file;
        file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write as _;
    use std::sync::Mutex;

    use tracing_subscriber::fmt::MakeWriter;

    /// A sink the assertions can read back.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        fn text(&self) -> String {
            let bytes = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }

    impl io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for Captured {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    /// Every write fails, which is what a Windows service's stdout handle does.
    #[derive(Clone, Copy, Default)]
    struct AlwaysFails;

    impl io::Write for AlwaysFails {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    impl<'writer> MakeWriter<'writer> for AlwaysFails {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            Self
        }
    }

    /// Runs `body` against a subscriber shaped exactly like the durable half of [`init`]: pinned at
    /// info, filtered by target, over `sink`.
    fn with_durable_shape<W, F>(sink: W, body: F)
    where
        W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
        F: FnOnce(),
    {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("trace"))
            .with_ansi(false)
            .with_target(true)
            .with_writer(
                sink.with_max_level(Level::INFO)
                    .with_filter(|meta: &Metadata<'_>| is_durable(meta.target())),
            )
            .finish();
        tracing::subscriber::with_default(subscriber, body);
    }

    #[test]
    fn the_three_excluded_targets_and_their_descendants_are_not_durable() {
        assert!(!is_durable("pos_edge::pairing_announce"));
        assert!(!is_durable("pos_edge::auth"));
        assert!(!is_durable("pos_edge::auth::lockout"));
        assert!(!is_durable("pos_edge::http::auth"));
        assert!(!is_durable("pos_edge::http::auth::device"));
    }

    #[test]
    fn every_other_target_is_durable_including_near_misses() {
        assert!(is_durable("pos_edge::server"));
        assert!(is_durable("pos_edge::http"));
        assert!(is_durable("pos_edge::config_client"));
        // A prefix that is not a `::` boundary is a different target, not a descendant.
        assert!(is_durable("pos_edge::authority"));
        assert!(is_durable("pos_edge::pairing_announcements"));
        assert!(is_durable("pos_edge::pairing"));
    }

    #[test]
    fn a_pairing_code_never_reaches_the_durable_file() {
        let captured = Captured::default();
        with_durable_shape(captured.clone(), || {
            tracing::info!(
                target: "pos_edge::pairing_announce",
                pairing_url = "http://10.0.0.4:8080/pair?code=428913",
                "scan or type this to pair a device",
            );
            tracing::info!(target: "pos_edge::server", "pos_edge listening");
        });
        let text = captured.text();
        assert!(
            !text.contains("428913"),
            "the code reached the file: {text}"
        );
        assert!(
            !text.contains("pair?code"),
            "the URL reached the file: {text}"
        );
        assert!(
            text.contains("pos_edge listening"),
            "the ordinary line did not: {text}"
        );
    }

    #[test]
    fn a_wrong_pin_refusal_never_reaches_the_durable_file() {
        let captured = Captured::default();
        with_durable_shape(captured.clone(), || {
            tracing::warn!(
                target: "pos_edge::http::auth",
                employee_id = "01J000000000000000000000EM",
                remaining = 2_u32,
                "staff sign-in refused: wrong pin",
            );
            tracing::info!(
                target: "pos_edge::http::auth",
                employee_id = "01J000000000000000000000EM",
                "staff signed in",
            );
        });
        let text = captured.text();
        assert!(text.is_empty(), "an auth line reached the file: {text}");
    }

    #[test]
    fn raising_the_env_filter_cannot_make_a_debug_line_durable() {
        let captured = Captured::default();
        with_durable_shape(captured.clone(), || {
            tracing::debug!(target: "pos_edge::config_client", "polling the config rail");
            tracing::trace!(target: "pos_edge::config_client", "decoded a node");
            tracing::info!(target: "pos_edge::config_client", "config applied");
        });
        let text = captured.text();
        assert!(!text.contains("polling the config rail"), "{text}");
        assert!(!text.contains("decoded a node"), "{text}");
        assert!(text.contains("config applied"), "{text}");
    }

    #[test]
    fn a_line_reaches_the_second_sink_when_the_first_one_errors() {
        // The case a naive test misses, and the one every write under the Service Control Manager
        // takes: stdout is a handle that refuses every byte, and the file must still get the line.
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("info"))
            .with_ansi(false)
            .with_writer(AlwaysFails.and(captured.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("a line nobody on the console will ever see");
        });
        assert!(
            captured
                .text()
                .contains("a line nobody on the console will ever see"),
            "the surviving sink lost the line: {}",
            captured.text()
        );
    }

    #[test]
    fn the_cap_stops_the_file_growing_and_does_not_report_an_error() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("pos-edge.log");
        let capped = open_durable(&path).expect("the log file opens");

        let line = vec![b'x'; 64 * 1024];
        let mut written_calls = 0_u64;
        let mut sink = &capped;
        // Twice the cap, so the second half is entirely past it.
        while written_calls < (RUN_BYTE_CAP / line.len() as u64) * 2 {
            sink.write_all(&line).expect("a capped write never errors");
            written_calls += 1;
        }
        sink.flush().expect("a capped flush never errors");

        let size = fs::metadata(&path).expect("the file exists").len();
        assert!(size >= RUN_BYTE_CAP, "it stopped early at {size} bytes");
        assert!(
            size < RUN_BYTE_CAP + line.len() as u64,
            "it overshot the cap by more than one write: {size} bytes"
        );
    }

    #[test]
    fn opening_rotates_the_previous_run_aside_and_keeps_only_one() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("nested").join("pos-edge.log");

        fs::create_dir_all(path.parent().expect("a parent")).expect("the directory is created");
        fs::write(&path, b"the run before last\n").expect("a first file");
        drop(open_durable(&path).expect("the second run opens"));
        fs::write(&path, b"the previous run\n").expect("the second run writes");
        drop(open_durable(&path).expect("the third run opens"));

        let previous = fs::read_to_string(path.with_file_name("pos-edge.log.1"))
            .expect("the previous run was kept");
        assert_eq!(previous, "the previous run\n");
        assert_eq!(
            fs::read_to_string(&path).expect("this run's file"),
            "",
            "the current run appended to the previous one"
        );
        assert!(
            !path.with_file_name("pos-edge.log.1.1").exists(),
            "rotation kept more than one previous run"
        );
    }

    #[test]
    fn opening_creates_the_state_directory_the_installer_names() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("state").join("logs").join("edge.log");
        drop(open_durable(&path).expect("the parents are created"));
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only_where_there_is_a_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("pos-edge.log");
        drop(open_durable(&path).expect("the log file opens"));
        let mode = fs::metadata(&path)
            .expect("the file exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }

    #[test]
    fn a_self_test_child_never_takes_a_durable_file() {
        // Asserted through the flag rather than through the environment, which is process-global and
        // would race every other test in this binary.
        assert_eq!(crate::installer::SELF_TEST_FLAG, "--self-test");
        assert!(
            std::env::args_os().all(|argument| argument != crate::installer::SELF_TEST_FLAG),
            "the test binary was invoked with the flag, which would void this assertion"
        );
        assert!(
            durable_log_path().is_none(),
            "an unset variable produced a path"
        );
    }
}
