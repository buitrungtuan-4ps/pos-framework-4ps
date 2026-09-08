// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `tracing` subscriber.
//!
//! # Why this is a second copy of a subscriber that already exists
//!
//! `pos_edge::telemetry` does the same thing, and this crate cannot use it: ADR-0112 fixes this
//! binary's dependencies at three workspace crates and `cargo run -p xtask -- print-agent-deps`
//! holds that list, so a `pos-edge` dependency here fails CI on purpose. Nor can the sink move down
//! into `pos-proto` or `pos-ports`, which are the vendor-neutral backbone and carry no
//! `tracing-subscriber`. Two small copies is the price of the boundary, and the boundary is the
//! reason one ESC/POS encoder exists in this tree rather than two.
//!
//! # The rule
//!
//! A print document may carry a buyer's name and tax code ([`pos_ports::printer`]), so this process
//! writes a document nowhere but the print head — the crate's own `print_stdout`/`print_stderr`
//! denials say so, and a log line here records identifiers, counts and outcomes only.
//!
//! # The durable sink
//!
//! Registered with `sc.exe create`, this process has no console, so its stdout goes nowhere and
//! every diagnostic with it — the *"silence reported twice"* signal ADR-0112 leans on included.
//! Setting [`LOG_FILE_VARIABLE`] tees the stream into that file as well; unset, or on a
//! `--self-test`, the builder is what it was before
//! [ADR-0117](../../../docs/adr/0117-a-headless-store-keeps-a-log.md).
//!
//! The file is pinned at `INFO` by the writer, so raising `RUST_LOG` to diagnose a printer cannot
//! make a `DEBUG` line durable. It holds the current run plus one previous run — [`init`] renames the
//! old file aside — and within a run is capped at [`RUN_BYTE_CAP`], a disk-full guard rather than a
//! retention period.
//!
//! **This process still has no Service Control Manager handshake** — no `service.rs` and no
//! `windows-service` dependency, which is the error-1053 failure `pos_edge::service` documents. Until
//! that is fixed the file this module opens will be empty on Windows, because the process never gets
//! far enough to write to it. The mechanism lands here so that the fix has somewhere to report to.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::Level;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt as _;

/// The environment variable that names the durable log file, and by being set turns it on.
///
/// Set by the generated Windows installer to a path under the agent's own state directory. Nothing
/// under `deploy/` sets it on Linux, where journald already captures the stream.
pub const LOG_FILE_VARIABLE: &str = "POS_PRINT_AGENT_LOG_FILE";

/// The flag the installer runs a staged binary with, and which declines a durable log file.
///
/// A self-test is a second process against the same state directory as the live one, and two of them
/// rotating and truncating one file would cost the running agent its log.
pub const SELF_TEST_FLAG: &str = "--self-test";

/// How many bytes one run may write to the durable file before it stops writing to it.
///
/// Smaller than the edge's, because this process logs one line per job and the machine it sits on is
/// usually a shop-floor PC with a small disk.
const RUN_BYTE_CAP: u64 = 4 * 1024 * 1024;

/// The suffix the previous run's file is renamed to at start-up.
const PREVIOUS_RUN_SUFFIX: &str = ".1";

/// Installs the process-wide log subscriber, once.
///
/// Reads the filter from `RUST_LOG`, defaulting to `info`. `try_init` rather than `init`: a second
/// install is the idempotent case, and a `panic` in the first three lines of `main` gives an
/// operator a process that dies with no explanation at all — the worst possible failure for a
/// module whose whole job is explaining failures.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_unset| EnvFilter::new("info"));

    let Some(path) = durable_log_path() else {
        let _ignored = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        return;
    };

    match open_durable(&path) {
        Ok(durable) => {
            // stdout first: `Tee` writes both halves before it propagates either error, so the file
            // still receives the whole line when stdout is the null handle a Windows service gets.
            let sink = io::stdout.and(Arc::new(durable).with_max_level(Level::INFO));
            let _ignored = tracing_subscriber::fmt()
                .with_env_filter(filter)
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
            let _ignored = tracing_subscriber::fmt().with_env_filter(filter).try_init();
            tracing::warn!(
                log_file = %path.display(),
                %error,
                variable = LOG_FILE_VARIABLE,
                "the log file could not be opened; logging to standard output only — on a machine \
                 running this as a service that means no diagnostics at all"
            );
        }
    }
}

/// The durable file's path, or `None` when there is not to be one.
fn durable_log_path() -> Option<PathBuf> {
    if std::env::args_os().any(|argument| argument == SELF_TEST_FLAG) {
        return None;
    }
    let raw = std::env::var_os(LOG_FILE_VARIABLE)?;
    (!raw.is_empty()).then(|| PathBuf::from(raw))
}

/// Rotates the previous run's file aside and opens a fresh one, creating the parent directory.
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
    // Owner-only where there is a mode. On Windows the state directory's inherited ACL applies,
    // which is deliberately what the installer's own stated intent asks for: a log readable by
    // whoever is diagnosing a printer.
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
/// Deliberately silent: a fresh install has nothing to rename, and a rename that fails for any other
/// reason is not a reason to refuse to log. The truncating open above is what guarantees this run
/// does not append to the last one either way.
fn rotate(path: &Path) {
    let mut previous = path.as_os_str().to_owned();
    previous.push(PREVIOUS_RUN_SUFFIX);
    let _ignored = fs::rename(path, PathBuf::from(previous));
}

/// The durable file, and how many bytes this run has put in it.
///
/// `Relaxed` on purpose: two threads logging at the same instant may between them cross the boundary
/// by one line, which is the right amount of precision for a disk-full guard.
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

    #[test]
    fn the_cap_stops_the_file_growing_and_does_not_report_an_error() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("print-agent.log");
        let capped = open_durable(&path).expect("the log file opens");

        let line = vec![b'x'; 64 * 1024];
        let mut sink = &capped;
        for _ in 0..((RUN_BYTE_CAP / line.len() as u64) * 2) {
            sink.write_all(&line).expect("a capped write never errors");
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
    fn opening_rotates_the_previous_run_aside_and_creates_the_state_directory() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("state").join("print-agent.log");

        drop(open_durable(&path).expect("the parents are created"));
        fs::write(&path, b"the previous run\n").expect("the first run writes");
        drop(open_durable(&path).expect("the second run opens"));

        assert_eq!(
            fs::read_to_string(path.with_file_name("print-agent.log.1"))
                .expect("the previous run was kept"),
            "the previous run\n"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("this run's file"),
            "",
            "the current run appended to the previous one"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only_where_there_is_a_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("print-agent.log");
        drop(open_durable(&path).expect("the log file opens"));
        let mode = fs::metadata(&path)
            .expect("the file exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }

    #[test]
    fn a_self_test_child_never_takes_a_durable_file() {
        // Asserted through the flag rather than the environment, which is process-global and would
        // race every other test in this binary.
        assert!(
            std::env::args_os().all(|argument| argument != SELF_TEST_FLAG),
            "the test binary was invoked with the flag, which would void this assertion"
        );
        assert!(
            durable_log_path().is_none(),
            "an unset variable produced a path"
        );
    }
}
