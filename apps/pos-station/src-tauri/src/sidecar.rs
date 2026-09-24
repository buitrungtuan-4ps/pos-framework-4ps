// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The print agent as a sidecar on a Terminal (ADR-0147 over ADR-0112).
//!
//! The bundler ships `pos_print_agent` beside the app's own executable (`bundle.externalBin`), and
//! this module keeps one copy of it running for as long as the app runs and the device is paired:
//! started with this terminal's own token, restarted with a growing delay when it exits, and stopped
//! when the app quits.
//!
//! # How the agent is configured
//!
//! Exactly the way its installers configure it (ADR-0112's correction): a `print-agent.toml` naming
//! `edge_url` and `state_path`, found through `POS_PRINT_AGENT_CONFIG`, and the credential in
//! `POS_PRINT_AGENT_TOKEN` — never in the file, which sits beside the state and is read by whoever is
//! diagnosing a printer. `POS_PRINT_AGENT_LOG_FILE` gives it the durable log ADR-0117 describes,
//! because a sidecar has no console of its own on Windows.
//!
//! # Spawned from Rust, not through `tauri-plugin-shell`
//!
//! The plugin's value is a JavaScript API for spawning processes, and this app deliberately gives its
//! pages none. Resolving the binary the way the plugin does — the executable's own directory — costs a
//! few lines of `std`, and keeps the plugin's subtree out of the build.
//!
//! # Not handled here
//!
//! If the app itself is killed rather than quit, the agent outlives it until the next start finds a
//! second agent already running under the same token. That is harmless to printing — the edge leases
//! each job to one claimer — and fixing it needs a Job object on Windows and `PR_SET_PDEATHSIG` on
//! Linux, both `unsafe` FFI this crate denies.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;

/// The agent's executable name, without the platform suffix.
pub(crate) const PROGRAM: &str = "pos_print_agent";

/// How often the supervisor checks whether the agent is still running.
const TICK: Duration = Duration::from_millis(500);

/// The first restart delay.
const FIRST_DELAY: Duration = Duration::from_secs(1);

/// The longest restart delay. A kitchen waits at most this long for its printer after the fault that
/// kept killing the agent is fixed.
const MAX_DELAY: Duration = Duration::from_secs(60);

/// A run at least this long counts as healthy, and the next failure starts the delays again from the
/// first.
const STABLE_RUN: Duration = Duration::from_secs(60);

/// What the agent is doing, for the status page and the tray.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state")]
pub(crate) enum AgentStatus {
    /// About to spawn.
    #[serde(rename = "PRINT_AGENT_STARTING")]
    Starting,
    /// Running.
    #[serde(rename = "PRINT_AGENT_RUNNING")]
    Running {
        /// How many times it has been restarted since the app started.
        restarts: u32,
    },
    /// Exited, and waiting to be started again.
    #[serde(rename = "PRINT_AGENT_WAITING")]
    Waiting {
        /// How many times it has been restarted since the app started.
        restarts: u32,
        /// When the next start is.
        retry_in_seconds: u64,
    },
    /// The executable is not beside the app: a build that did not bundle it.
    #[serde(rename = "PRINT_AGENT_MISSING")]
    Missing,
    /// Stopped by the app.
    #[serde(rename = "PRINT_AGENT_STOPPED")]
    Stopped,
}

/// Everything one agent process needs.
#[derive(Clone)]
pub(crate) struct Launch {
    /// The agent executable.
    pub(crate) program: PathBuf,
    /// Where to write `print-agent.toml`.
    pub(crate) config_path: PathBuf,
    /// Where the agent keeps its one id per printer.
    pub(crate) state_path: PathBuf,
    /// The agent's durable log.
    pub(crate) log_path: PathBuf,
    /// The edge, as an origin.
    pub(crate) edge_url: String,
    /// This terminal's device token.
    pub(crate) token: String,
}

impl core::fmt::Debug for Launch {
    /// Redacts the token, which is a working bearer credential.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Launch")
            .field("program", &self.program)
            .field("config_path", &self.config_path)
            .field("state_path", &self.state_path)
            .field("log_path", &self.log_path)
            .field("edge_url", &self.edge_url)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// The agent executable beside the app's own, as the bundler places `externalBin`.
pub(crate) fn beside_current_exe() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "the executable has no directory")
    })?;
    Ok(dir.join(format!("{PROGRAM}{}", std::env::consts::EXE_SUFFIX)))
}

/// The running supervisor. Dropping it without [`Supervisor::stop`] leaves the thread to stop the
/// agent when the channel closes.
#[derive(Debug)]
pub(crate) struct Supervisor {
    stop: SyncSender<()>,
    thread: Option<JoinHandle<()>>,
    status: Arc<Mutex<AgentStatus>>,
}

impl Supervisor {
    /// Starts supervising. Returns an error only if the thread could not be spawned.
    pub(crate) fn start(launch: Launch) -> io::Result<Self> {
        let (stop, stopped) = sync_channel(1);
        let status = Arc::new(Mutex::new(AgentStatus::Starting));
        let shared = Arc::clone(&status);
        let thread = std::thread::Builder::new()
            .name("print-agent".to_owned())
            .spawn(move || supervise(&launch, &stopped, &shared))?;
        Ok(Self {
            stop,
            thread: Some(thread),
            status,
        })
    }

    /// What the agent is doing now.
    pub(crate) fn status(&self) -> AgentStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Stops the agent and waits for the supervisor to finish, which is quick: the agent is killed,
    /// not drained. A job it was printing is unacknowledged, so the edge's lease hands it back
    /// (ADR-0112) — there is nothing to drain.
    pub(crate) fn stop(mut self) {
        let _sent = self.stop.try_send(());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("the print agent supervisor thread panicked");
        }
    }
}

fn set(status: &Mutex<AgentStatus>, next: AgentStatus) {
    *status.lock().unwrap_or_else(PoisonError::into_inner) = next;
}

/// The supervisor loop: spawn, watch, and on exit wait and spawn again, until told to stop.
fn supervise(launch: &Launch, stop: &Receiver<()>, status: &Mutex<AgentStatus>) {
    if let Err(error) = write_config(launch) {
        log::error!("could not write {}: {error}", launch.config_path.display());
    }
    let mut delay = Duration::ZERO;
    let mut restarts: u32 = 0;
    loop {
        let ran_for = match spawn(launch) {
            Ok(mut child) => {
                log::info!("print agent started (pid {})", child.id());
                set(status, AgentStatus::Running { restarts });
                match watch(&mut child, stop) {
                    Watched::Stopped => {
                        set(status, AgentStatus::Stopped);
                        return;
                    }
                    Watched::Exited { ran_for } => ran_for,
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                log::warn!(
                    "the print agent is not installed at {}",
                    launch.program.display()
                );
                set(status, AgentStatus::Missing);
                if wait(stop, MAX_DELAY) {
                    set(status, AgentStatus::Stopped);
                    return;
                }
                continue;
            }
            Err(error) => {
                log::warn!("the print agent could not be started: {error}");
                Duration::ZERO
            }
        };
        delay = next_delay(delay, ran_for);
        restarts = restarts.saturating_add(1);
        set(
            status,
            AgentStatus::Waiting {
                restarts,
                retry_in_seconds: delay.as_secs(),
            },
        );
        log::info!("print agent restarts in {} s", delay.as_secs());
        if wait(stop, delay) {
            set(status, AgentStatus::Stopped);
            return;
        }
    }
}

/// How a watched process ended.
enum Watched {
    /// The app asked it to stop; it has been killed and reaped.
    Stopped,
    /// It exited by itself after roughly `ran_for`.
    Exited { ran_for: Duration },
}

/// Watches `child` until it exits or the app asks for a stop.
fn watch(child: &mut Child, stop: &Receiver<()>) -> Watched {
    let mut ticks: u32 = 0;
    loop {
        if wait(stop, TICK) {
            if let Err(error) = child.kill() {
                log::warn!("could not stop the print agent: {error}");
            }
            let _reaped = child.wait();
            log::info!("print agent stopped");
            return Watched::Stopped;
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                log::warn!("print agent exited: {exit}");
                return Watched::Exited {
                    ran_for: TICK.saturating_mul(ticks),
                };
            }
            Ok(None) => ticks = ticks.saturating_add(1),
            Err(error) => {
                log::warn!("could not read the print agent's state: {error}");
                return Watched::Exited {
                    ran_for: TICK.saturating_mul(ticks),
                };
            }
        }
    }
}

/// Waits up to `duration`, returning `true` if a stop arrived (or the app went away) meanwhile.
fn wait(stop: &Receiver<()>, duration: Duration) -> bool {
    match stop.recv_timeout(duration) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
        Err(RecvTimeoutError::Timeout) => false,
    }
}

/// The delay before the next start: doubling from [`FIRST_DELAY`] to [`MAX_DELAY`], and back to the
/// first after a run of at least [`STABLE_RUN`].
pub(crate) fn next_delay(previous: Duration, ran_for: Duration) -> Duration {
    if ran_for >= STABLE_RUN {
        return FIRST_DELAY;
    }
    previous.saturating_mul(2).clamp(FIRST_DELAY, MAX_DELAY)
}

fn spawn(launch: &Launch) -> io::Result<Child> {
    let mut command = Command::new(&launch.program);
    command
        .env("POS_PRINT_AGENT_CONFIG", &launch.config_path)
        .env("POS_PRINT_AGENT_TOKEN", &launch.token)
        .env("POS_PRINT_AGENT_LOG_FILE", &launch.log_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = launch.config_path.parent() {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// `CREATE_NO_WINDOW`: a console program started by a GUI app must not open a console.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()
}

fn write_config(launch: &Launch) -> io::Result<()> {
    if let Some(dir) = launch.config_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(dir) = launch.log_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &launch.config_path,
        config_text(&launch.edge_url, &launch.state_path),
    )
}

/// The agent's `print-agent.toml`. No token: that travels in the environment.
pub(crate) fn config_text(edge_url: &str, state_path: &Path) -> String {
    format!(
        "# Written by POS Station at every start; edits are overwritten (ADR-0147).\n\
         # The device token is not here: the app passes it in POS_PRINT_AGENT_TOKEN.\n\
         edge_url = {}\n\
         state_path = {}\n",
        toml_string(edge_url),
        toml_string(&state_path.to_string_lossy())
    )
}

/// A TOML basic string. A JSON string literal is one, except that TOML also forbids a raw DEL.
fn toml_string(value: &str) -> String {
    serde_json::Value::String(value.to_owned())
        .to_string()
        .replace('\u{7f}', "\\u007F")
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::{AgentStatus, FIRST_DELAY, Launch, MAX_DELAY, config_text, next_delay};

    #[test]
    fn restarts_back_off_from_one_second_to_a_minute() {
        let mut delay = Duration::ZERO;
        let mut seen = Vec::new();
        for _ in 0..8 {
            delay = next_delay(delay, Duration::ZERO);
            seen.push(delay.as_secs());
        }
        assert_eq!(seen, vec![1, 2, 4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn a_healthy_run_resets_the_back_off() {
        assert_eq!(next_delay(MAX_DELAY, Duration::from_secs(61)), FIRST_DELAY);
        assert_eq!(next_delay(MAX_DELAY, Duration::from_secs(60)), FIRST_DELAY);
        assert_eq!(
            next_delay(Duration::from_secs(8), Duration::from_secs(59)),
            Duration::from_secs(16)
        );
    }

    #[test]
    fn the_config_names_the_edge_and_the_state_and_never_the_token() {
        let text = config_text(
            "http://192.168.1.10:8080",
            Path::new("/var/lib/pos-station/print-agent-state.json"),
        );
        assert!(text.contains("edge_url = \"http://192.168.1.10:8080\"\n"));
        assert!(text.contains("state_path = \"/var/lib/pos-station/print-agent-state.json\"\n"));
        assert!(!text.contains("device_token"));
    }

    #[test]
    fn a_windows_path_survives_as_a_toml_string() {
        let text = config_text(
            "http://10.0.0.2:8080",
            Path::new(r#"C:\Users\Thu "Ngân"\AppData\Roaming\pos-station\print-agent-state.json"#),
        );
        assert!(text.contains(
            r#"state_path = "C:\\Users\\Thu \"Ngân\"\\AppData\\Roaming\\pos-station\\print-agent-state.json""#
        ));
        let tricky = config_text("http://h\u{7f}", Path::new("/tmp/x"));
        assert!(
            tricky.contains("edge_url = \"http://h\\u007F\""),
            "{tricky}"
        );
    }

    #[test]
    fn the_launch_never_prints_its_token() {
        let launch = Launch {
            program: "pos_print_agent".into(),
            config_path: "print-agent.toml".into(),
            state_path: "state.json".into(),
            log_path: "agent.log".into(),
            edge_url: "http://10.0.0.2:8080".to_owned(),
            token: "not-a-real-token".to_owned(),
        };
        let debug = format!("{launch:?}");
        assert!(!debug.contains("not-a-real-token"), "{debug}");
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn the_status_is_serialised_in_upper_snake_case() {
        let json = serde_json::to_value(AgentStatus::Waiting {
            restarts: 3,
            retry_in_seconds: 8,
        })
        .unwrap();
        assert_eq!(
            json.pointer("/state").and_then(serde_json::Value::as_str),
            Some("PRINT_AGENT_WAITING")
        );
        assert_eq!(
            json.pointer("/retry_in_seconds")
                .and_then(serde_json::Value::as_u64),
            Some(8)
        );
    }
}
