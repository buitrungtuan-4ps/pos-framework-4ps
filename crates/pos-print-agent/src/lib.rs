// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The print agent: a device driver that happens to be a process
//! ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! A printer may name the device whose transport reaches it. This is that device's side: claim a
//! rendered job from the store's edge, open the address the edge named, write the bytes, say the id
//! back. **It decides nothing.** No rendering, no domain, no configuration of its own beyond where
//! the edge is and how to prove it is paired.
//!
//! # Why this is a third binary, when ADR-0002 says there are two
//!
//! ADR-0112 confronts that rather than routing around it, and ADR-0113 amends ADR-0002 to *three
//! tiers and one binary per tier, plus named device-level artifacts that decide nothing* — with this
//! as one of the two named exceptions. Three things bound the cost ADR-0002 was protecting against:
//! it exists only on stores that chose a hosted edge placement, it holds nothing that matters (one
//! id per printer), and it decides nothing.
//!
//! # What it persists, and what losing it costs
//!
//! **One value per printer: the id of the last job it wrote successfully.** Not the document, not a
//! queue, not a history. When the edge redelivers a job after a lost acknowledgement, the agent
//! compares the id against that value and, on a match, acknowledges without writing.
//!
//! One value is sufficient because the queue leases one job per printer at a time — ESC/POS is a
//! byte stream and two concurrent writers interleave garbage — so at any moment there is exactly one
//! job whose outcome is in doubt. An agent that loses the file reprints one job, which is the
//! correct direction: a duplicate ticket costs a strip of paper and a missing one costs a dish.
//!
//! # The order of the three writes, and why it is this one
//!
//! Write the bytes, **record the id, then acknowledge.** A crash between the record and the
//! acknowledgement is the case this ordering exists for: the job comes back at the claim lease, the
//! record answers "already written", and the agent acknowledges without printing a second ticket.
//! The reverse order would leave a job deleted with no record of it, which is survivable, and the
//! order chosen is strictly better. A write that *fails* is never acknowledged at all: the lease
//! lapses and the job returns to the queue, which is what a jammed printer needs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use pos_ports::PortError;
use pos_ports::printer::{PrintJob, PrinterCapabilities, PrinterConnection};
use pos_proto::ids::EventId;

pub mod printers;
pub mod wire;

/// How long to wait before asking again after the edge could not be reached.
///
/// Shorter than the queue's TTL by two orders of magnitude, so an edge that comes back inside a
/// blip loses nothing; long enough that a store whose edge is down does not spend a night dialling
/// it. The happy path has no sleep at all — `GET /api/print/jobs` parks server-side, so a healthy
/// agent is always either waiting on that request or printing.
pub const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

/// The version this binary reports in its user agent, for a log an operator reads.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Anything that stops a cycle.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The edge could not be reached, or answered something this build cannot read.
    #[error("the edge could not be reached: {0}")]
    Edge(String),
    /// The configuration file is missing, unreadable, or not what this build expects.
    #[error("the agent's configuration could not be read: {0}")]
    Config(String),
    /// The record of what was last written could not be read or written.
    #[error("the record of written jobs could not be kept: {0}")]
    State(String),
}

/// Where the edge is and how to prove this device is paired to it.
///
/// Three fields and nothing else. An agent that could be configured would be an agent that decides
/// something, and every decision about printing belongs on the edge.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// The store's edge, as `http://host:port` on a shop LAN or `https://host` for a hosted edge
    /// placement. Both are supported because the edge is in a different place in each
    /// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    pub edge_url: String,
    /// The bearer token this device was issued when it was paired
    /// ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md)). The only gate the agent's
    /// two routes carry, and the thing that resolves which terminal it answers for.
    pub device_token: String,
    /// Where to keep the one id per printer. Defaults beside the configuration file.
    #[serde(default = "default_state_path")]
    pub state_path: PathBuf,
}

/// The default for [`Config::state_path`]: a file beside the binary's working directory.
fn default_state_path() -> PathBuf {
    PathBuf::from("print-agent-state.json")
}

impl Config {
    /// Reads a configuration from a TOML file.
    ///
    /// # Errors
    ///
    /// [`AgentError::Config`] if the file cannot be read or does not parse. Refusing to start beats
    /// starting with a token that is not there: an agent that silently never claims looks exactly
    /// like a printer nobody plugged in.
    pub fn load(path: &Path) -> Result<Self, AgentError> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            AgentError::Config(format!("{} could not be read: {error}", path.display()))
        })?;
        let config: Self = toml::from_str(&text).map_err(|error| {
            AgentError::Config(format!("{} is not valid: {error}", path.display()))
        })?;
        if config.device_token.trim().is_empty() {
            return Err(AgentError::Config(
                "device_token is empty; pair this device with the store first".to_owned(),
            ));
        }
        Ok(config)
    }
}

/// One job the edge has leased to this agent, as `GET /api/print/jobs` returns it.
///
/// A client's view of the edge's response, the way [`ui/src/api/types.ts`] is a client's view of the
/// same surface. Everything on it was decided by the edge: the agent opens `address` with the
/// transport `connection` names, hands `capabilities` and `job` to the encoder, and reports
/// `job.job_id` back.
#[derive(Debug, Clone, Deserialize)]
pub struct LeasedJob {
    /// Which printer, as the published `devices` node names it.
    pub printer_device_id: String,
    /// Where to open it: a host and port, or a device path.
    pub address: String,
    /// Which of those it is.
    pub connection: PrinterConnection,
    /// What the edge assumed this printer can do when it prepared the document.
    pub capabilities: PrinterCapabilities,
    /// Unix milliseconds after which the lease lapses and the job returns to the queue.
    pub claim_expires_at: i64,
    /// The finished document, and the id the acknowledgement carries.
    pub job: PrintJob,
}

/// What a claim returned. Empty is the ordinary answer at the end of a park.
#[derive(Debug, Clone, Deserialize)]
pub struct LeasedJobs {
    /// At most one per printer this agent owns.
    pub jobs: Vec<LeasedJob>,
}

/// The two calls the agent makes against its store's edge.
///
/// A seam so the cycle below can be driven without a socket: the ordering rules this module is about
/// — record before acknowledge, never acknowledge a failed write — are the part worth testing, and
/// they are not about HTTP.
pub trait EdgeTransport: Send + Sync {
    /// `GET /api/print/jobs`. The edge holds this open for up to twenty seconds when there is
    /// nothing to hand out, so a healthy agent spends its life inside this call.
    ///
    /// # Errors
    ///
    /// [`AgentError::Edge`] if the edge could not be reached or answered unreadably.
    fn claim(&self) -> impl Future<Output = Result<Vec<LeasedJob>, AgentError>> + Send;

    /// `POST /api/print/jobs/{job_id}/ack`.
    ///
    /// # Errors
    ///
    /// [`AgentError::Edge`] as above.
    fn acknowledge(&self, job: EventId) -> impl Future<Output = Result<(), AgentError>> + Send;
}

/// Putting bytes on a print head.
///
/// Blocking, because writing to a device path is: a printer that has been unplugged blocks on a
/// socket or a device timeout, and the caller puts this on a blocking thread.
pub trait Printing: Send + Sync {
    /// Opens `job.address` and writes the document.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the printer could not be reached or refused the job.
    fn write(&self, job: &LeasedJob) -> Result<(), PortError>;
}

/// The id of the last job written successfully, per printer.
///
/// Kept as JSON in one small file, rewritten whole through a temporary file and a rename, so a
/// crash mid-write leaves the previous record rather than half of the new one.
#[derive(Debug)]
pub struct LastWritten {
    path: PathBuf,
    /// Ordered so the file's bytes are stable, which makes a diff of it readable to a human
    /// diagnosing a duplicate ticket.
    by_printer: Mutex<BTreeMap<String, String>>,
}

/// The on-disk shape. A struct rather than a bare map so a later field has somewhere to go.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    by_printer: BTreeMap<String, String>,
}

impl LastWritten {
    /// Reads the record at `path`, or starts empty when there is none.
    ///
    /// A missing file is the ordinary first run. A file that will not parse is **also** treated as
    /// empty, deliberately, and logged: the cost of ignoring it is one duplicate ticket, and the
    /// cost of refusing to start is a kitchen with no tickets at all.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let by_printer = match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<StateFile>(&text) {
                Ok(state) => state.by_printer,
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "the record of written jobs could not be read and was started empty; at \
                         most one job may print twice"
                    );
                    BTreeMap::new()
                }
            },
            Err(_missing) => BTreeMap::new(),
        };
        Self {
            path: path.to_path_buf(),
            by_printer: Mutex::new(by_printer),
        }
    }

    /// Whether this exact job was already written to this printer.
    #[must_use]
    pub fn already_written(&self, printer: &str, job: EventId) -> bool {
        self.locked()
            .get(printer)
            .is_some_and(|written| written == &job.to_string())
    }

    /// Records that `job` was written to `printer`, replacing whatever was there.
    ///
    /// # Errors
    ///
    /// [`AgentError::State`] if the file could not be written. The caller must **not** acknowledge
    /// after this fails: the job returns at the lease and prints once more, which is the bounded
    /// failure this design accepts.
    pub fn record(&self, printer: &str, job: EventId) -> Result<(), AgentError> {
        let snapshot = {
            let mut written = self.locked();
            written.insert(printer.to_owned(), job.to_string());
            StateFile {
                by_printer: written.clone(),
            }
        };
        let text = serde_json::to_string_pretty(&snapshot)
            .map_err(|error| AgentError::State(format!("the record would not encode: {error}")))?;
        // Temporary file then rename, so a crash leaves the previous record whole.
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, text)
            .map_err(|error| AgentError::State(format!("writing the record failed: {error}")))?;
        std::fs::rename(&temporary, &self.path)
            .map_err(|error| AgentError::State(format!("replacing the record failed: {error}")))?;
        Ok(())
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        // A poisoned lock means another thread panicked holding it. The map is a plain record with
        // no invariant a panic could have half-broken, and losing it costs one duplicate ticket.
        self.by_printer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One claim-write-acknowledge cycle, returning how many jobs were handled.
///
/// Never fails on a printer: a jammed device is a job left unacknowledged, which the queue's lease
/// returns for the next cycle. It fails only when the *edge* cannot be reached, because there is
/// nothing else to do then but wait.
///
/// # Errors
///
/// [`AgentError::Edge`] if the claim itself failed.
pub async fn one_cycle<T, P>(
    edge: &T,
    printers: &P,
    written: &LastWritten,
) -> Result<usize, AgentError>
where
    T: EdgeTransport,
    P: Printing,
{
    let leased = edge.claim().await?;
    let mut handled = 0_usize;
    for job in leased {
        let job_id = job.job.job_id;
        if written.already_written(&job.printer_device_id, job_id) {
            // A redelivery after a lost acknowledgement. The bytes are already on paper; saying so
            // is the whole reason the record exists.
            tracing::info!(
                %job_id,
                printer = %job.printer_device_id,
                "this job was already written; acknowledging without printing it again"
            );
            acknowledge(edge, job_id).await;
            handled = handled.saturating_add(1);
            continue;
        }
        match printers.write(&job) {
            Ok(()) => {
                // Recorded *before* the acknowledgement: a crash in between costs nothing, a crash
                // the other way round would let a redelivery print twice.
                if let Err(error) = written.record(&job.printer_device_id, job_id) {
                    tracing::error!(
                        %job_id,
                        printer = %job.printer_device_id,
                        %error,
                        "the job printed but could not be recorded; it is not acknowledged, so it \
                         will return at the lease and print once more"
                    );
                    continue;
                }
                // The job's identifier and its printer, never its content (`pos_ports::printer`).
                tracing::info!(%job_id, printer = %job.printer_device_id, "printed");
                acknowledge(edge, job_id).await;
                handled = handled.saturating_add(1);
            }
            Err(error) => {
                // Deliberately not acknowledged. The lease lapses and the queue hands it back,
                // which is exactly what a printer out of paper needs.
                tracing::warn!(
                    %job_id,
                    printer = %job.printer_device_id,
                    %error,
                    "the printer refused the job; it stays on the queue until the lease lapses"
                );
            }
        }
    }
    Ok(handled)
}

/// Acknowledges, logging rather than failing.
///
/// A lost acknowledgement is the case the whole record above exists to survive, so it must not stop
/// the agent from printing the next job.
async fn acknowledge<T: EdgeTransport>(edge: &T, job: EventId) {
    if let Err(error) = edge.acknowledge(job).await {
        tracing::warn!(
            %job,
            %error,
            "the acknowledgement did not reach the edge; the job will be redelivered and skipped"
        );
    }
}

/// Claims and prints until `stop` resolves.
///
/// There is no poll interval in the healthy path: the edge parks the claim for up to twenty seconds
/// when it has nothing, so this loop is either inside that request or writing bytes.
pub async fn run<T, P, F>(edge: T, printers: P, written: LastWritten, stop: F)
where
    T: EdgeTransport,
    P: Printing,
    F: Future<Output = ()> + Send,
{
    let mut stop = std::pin::pin!(stop);
    loop {
        let cycle = std::pin::pin!(one_cycle(&edge, &printers, &written));
        tokio::select! {
            () = &mut stop => {
                tracing::info!("stopping; any job not acknowledged returns to the queue");
                return;
            }
            outcome = cycle => {
                if let Err(error) = outcome {
                    tracing::warn!(%error, "could not claim from the edge; waiting before asking again");
                    tokio::select! {
                        () = &mut stop => return,
                        () = tokio::time::sleep(RECONNECT_BACKOFF) => {}
                    }
                }
            }
        }
    }
}
