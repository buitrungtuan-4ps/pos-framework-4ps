// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The Windows service wrapper (roadmap v3 **E4**).
//!
//! Windows is a supported store operating system, and until this landed `deploy/edge/README.md` told
//! an operator to register the binary with `sc.exe create` and start it. **That does not work.** The
//! Service Control Manager gives a starting service about thirty seconds to connect back to it and
//! report `SERVICE_RUNNING`; a plain console program never does, so SCM kills it and reports
//! *"error 1053: the service did not respond to the start request in a timely fashion"*. Every
//! Windows store therefore either ran under a third-party shim such as NSSM or ran the binary by
//! hand in a console window, which is not a service at all — nothing starts it after a power cut.
//!
//! This module is the missing handshake. It is small on purpose: SCM is a state machine with four
//! messages in it, and the edge already had everything else.
//!
//! # The three things it does
//!
//! 1. **Connects to SCM and reports `Running`**, which is what makes `sc.exe start` succeed.
//! 2. **Turns a stop into the same drain a `SIGTERM` starts.** A stop arrives as a call on an SCM
//!    thread rather than as a signal, so it cannot reach [`tokio::signal`]; it flips a watch that
//!    [`serve_until`](pos_edge::serve_until) is waiting on, and an in-flight sale finishes before
//!    the process exits, exactly as it does on Linux.
//! 3. **Reports a non-zero exit code when the stop was an installed update.** This is the part with
//!    teeth, and the reason over-the-air updates could not have worked on Windows even with a shim.
//!
//! # Why the exit code carries the update
//!
//! An install writes the spare slot, retargets `bin/current` and *exits*; on Linux `Restart=always`
//! turns that exit into a start on the new binary ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md)
//! Amendment 1). SCM has no such setting. It has **failure actions**, and it applies them only when
//! a service looks like it *failed*: a service that reports `SERVICE_STOPPED` with exit code zero
//! has been stopped on purpose and stays stopped.
//!
//! So a store that installed a release at 11:40 on a Saturday would have gone dark until somebody
//! drove to the shop. Reporting [`RESTART_EXIT_CODE`] instead is what lets a configured failure
//! action ("restart after 5 seconds") bring the shop back — and an operator's own stop still reports
//! zero, so `sc.exe stop` stops the service rather than starting a restart loop. The registration
//! commands, including the failure action this depends on, are in
//! [`deploy/edge/README.md`](../../../deploy/edge/README.md).
//!
//! # What is still not exercised here
//!
//! The Windows CI job compiles this module (`cargo build --workspace` on `windows-2022`), so a
//! rename or a signature change fails a pull request. What no gate in this repository can check is
//! SCM's own behaviour: that a real service reaches `Running`, that a stop drains, and that a
//! failure action restarts on [`RESTART_EXIT_CODE`]. That needs a Windows box with a service
//! installed on it, and it is a row in [`docs/gate-register.md`](../../../docs/gate-register.md)
//! rather than a claim made here.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use pos_edge::{EdgeError, ServeOutcome};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

/// The service name SCM knows this binary by, and the name `sc.exe create` must use.
///
/// It matches the systemd unit's name so one set of instructions reads the same on both platforms.
pub const SERVICE_NAME: &str = "pos-edge";

/// What the wrapper reports to SCM when the stop was an installed update or a reverted boot, so a
/// configured failure action starts the binary that is now on disk.
///
/// One rather than zero is the whole point; the specific value does not matter to SCM, which only
/// asks whether the code is `ERROR_SUCCESS`.
const RESTART_EXIT_CODE: u32 = 1;

/// SCM's answer when a process that it did not start tries to connect to it as a service
/// (`ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`). The ordinary case: a console run.
const NOT_STARTED_BY_SCM: i32 = 1063;

/// How long SCM is told to wait for a state change it asked for. Only read while a service reports
/// a pending state, which this wrapper never does — it goes straight to `Running` and straight to
/// `Stopped` — so it is the documented zero rather than a guess at a timeout.
const NO_WAIT_HINT: Duration = Duration::ZERO;

/// The config path, handed from `main` to [`service_main`].
///
/// A static because SCM calls the service entry point through a C function pointer of its own
/// choosing: there is no argument to thread the path through. Written exactly once, before the
/// dispatcher is started, and read exactly once, on the thread SCM calls back on.
static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Where [`service_main`] leaves anything that went wrong, for `main` to return.
///
/// SCM's entry point cannot return a value, and a service that failed to start must not exit zero
/// and look healthy to whatever is watching the process.
static FAILURE: Mutex<Option<String>> = Mutex::new(None);

define_windows_service!(ffi_service_main, service_main);

/// Hands the main thread to SCM if SCM started this process.
///
/// Returns `Ok(true)` when this ran as a service and has now finished, and `Ok(false)` when the
/// process was started from a console — the caller then serves normally.
///
/// # Errors
///
/// [`EdgeError::Service`] if the dispatcher could not be started for any reason other than "not
/// started by SCM", or if the service itself failed after SCM called back.
pub fn dispatch(config_path: PathBuf) -> Result<bool, EdgeError> {
    // Set before the dispatcher starts, so the callback thread cannot observe it unset.
    let _ignored = CONFIG_PATH.set(config_path);
    match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        Ok(()) => match FAILURE.lock() {
            Ok(mut failure) => match failure.take() {
                Some(message) => Err(EdgeError::Service(message)),
                None => Ok(true),
            },
            // A poisoned lock means `service_main` panicked while holding it, which is itself the
            // report: the service did not run.
            Err(_poisoned) => Err(EdgeError::Service(
                "the service thread panicked while reporting a failure".to_owned(),
            )),
        },
        Err(windows_service::Error::Winapi(error))
            if error.raw_os_error() == Some(NOT_STARTED_BY_SCM) =>
        {
            Ok(false)
        }
        Err(error) => Err(EdgeError::Service(error.to_string())),
    }
}

/// SCM's entry point. Runs on a thread SCM owns and cannot return a value, so a failure is left in
/// [`FAILURE`] for [`dispatch`] to return.
fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        let message = error.to_string();
        tracing::error!(%error, "the store did not run as a service");
        if let Ok(mut failure) = FAILURE.lock() {
            *failure = Some(message);
        }
    }
}

/// The service's life: register a control handler, report `Running`, serve, report `Stopped` with an
/// exit code that says whether the stop was a restart.
fn run_service() -> Result<(), EdgeError> {
    // The channel a stop travels down. SCM calls the handler on its own thread and the handler must
    // return promptly, so it does nothing but flip this; the drain happens where every other
    // shutdown's drain happens.
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let status_handle =
        service_control_handler::register(SERVICE_NAME, move |control| match control {
            // `Shutdown` is the machine going down — a power button, or a UPS with minutes left. It is
            // the same request as `Stop` and gets the same drain, which is what keeps a settled sale.
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ignored = stop_tx.send(true);
                ServiceControlHandlerResult::NoError
            }
            // "Are you still there?" — answering at all is the answer.
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _other => ServiceControlHandlerResult::NotImplemented,
        })
        .map_err(|error| EdgeError::Service(format!("SCM refused the control handler: {error}")))?;

    status_handle
        .set_service_status(status(ServiceState::Running, ServiceExitCode::Win32(0)))
        .map_err(|error| EdgeError::Service(format!("could not report Running to SCM: {error}")))?;

    let path = CONFIG_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("config.toml"));
    let served = crate::runtime()?.block_on(crate::run(path, async move {
        // `changed()` only errors once the sender is gone, and the sender lives in the control
        // handler for as long as the service is registered. Either way the answer is the same: stop.
        let _ignored = stop_rx.changed().await;
    }));

    // Report `Stopped` whatever happened, and report it *before* returning the error: a service that
    // exits without reporting its state leaves SCM waiting, and the operator staring at a console
    // that says "stopping".
    let exit = match served {
        Ok(ServeOutcome::RestartWanted) => {
            tracing::info!(
                "the binary on disk changed; exiting {RESTART_EXIT_CODE} so the service's failure \
                 action starts it"
            );
            ServiceExitCode::Win32(RESTART_EXIT_CODE)
        }
        Ok(ServeOutcome::Stopped) => ServiceExitCode::Win32(0),
        // A start that failed — an unreadable config, a bound port, a store that will not open — is
        // also a non-zero exit, and for the same reason: the failure action is what retries it.
        Err(_) => ServiceExitCode::Win32(RESTART_EXIT_CODE),
    };
    let reported = status_handle.set_service_status(status(ServiceState::Stopped, exit));
    served.map(|_outcome| ())?;
    reported
        .map_err(|error| EdgeError::Service(format!("could not report Stopped to SCM: {error}")))
}

/// One service status, in the shape SCM expects.
fn status(state: ServiceState, exit_code: ServiceExitCode) -> ServiceStatus {
    ServiceStatus {
        // The store is the only thing in this process, which is what `OWN_PROCESS` says.
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        // What the operator and the machine are allowed to ask for. Nothing else is accepted: the
        // edge has no meaningful pause, and pretending to pause a till is worse than refusing to.
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code,
        checkpoint: 0,
        wait_hint: NO_WAIT_HINT,
        process_id: None,
    }
}
