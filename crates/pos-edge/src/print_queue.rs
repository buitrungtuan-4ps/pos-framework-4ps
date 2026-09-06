// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The queue a print agent claims from
//! ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! # A trait here, implemented for the concrete store
//!
//! The same shape [`crate::print_agent::PrintAgents`], [`crate::queue::QueueNumberAuthority`] and
//! [`crate::lease_state::LeaseAuthority`] take, and for the same reason: [`crate::app::Edge`] is
//! generic over its store, so it cannot reach a concrete store's inherent table. In the field this
//! is [`store_sqlite::SqliteStore`], whose single writer thread serialises an enqueue against every
//! other allocation; the fakes-backed example and the route tests use [`InMemoryPrintQueue`].
//!
//! # The JSON is the storage format, not this seam's vocabulary
//!
//! `print_jobs.document` is a rendered [`PrintJob`] as JSON, and that encode lives **inside** the
//! SQLite implementation rather than in front of the seam. Putting it in front would make the
//! in-memory twin serialise for no reason and, worse, would let the two implementations disagree
//! about what a stored document is — the queue would then be a place where a document changes shape
//! depending on which store is underneath it. The seam speaks jobs; the table speaks text.
//!
//! # What this seam does not decide
//!
//! Not the clock, not the TTL, not the lease, not the cap. All four are the caller's, because they
//! are constants in one edge module ([`crate::print_agent`]) and a store adapter that held a policy
//! would be a second place to change one. And not the agent's liveness: [`Enqueued`] has no
//! `AgentUnavailable`, because that refusal is decided from the binding *before* this table is
//! touched. ADR-0112 orders the three enqueue outcomes deliberately — agent first, cap second,
//! accept third — and a queue must not start building behind a box that is not there.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

use pos_ports::printer::PrintJob;
use pos_ports::{PortError, PortName};
use pos_proto::ids::{DeviceId, EventId};
use store_sqlite::SqliteStore;

/// What an enqueue did — everything the table itself can decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    /// The row is written and the agent may be signalled.
    Queued,
    /// That printer already holds its full allowance of unexpired jobs. Nothing was written.
    ///
    /// Not a dead agent — that is refused earlier, from the binding. This is a *live* agent whose
    /// printer is not consuming: paper out, cover open, a write that errors and a job that returns
    /// to the queue at the claim lease, while the till keeps firing.
    QueueFull,
    /// A job with this id is already queued, so nothing was written and nothing was duplicated.
    ///
    /// `job_id` is the idempotency key everywhere else in printing, so a redelivered enqueue is the
    /// same ticket rather than a second one.
    AlreadyQueued,
}

/// A job an agent has just leased, with the lease it holds it under.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedJob {
    /// The printer to open. Not on the [`PrintJob`], which names a *station* — and a receipt has no
    /// station at all.
    pub printer: DeviceId,
    /// The finished document. The agent decides nothing about it.
    pub job: PrintJob,
    /// Unix milliseconds after which an unacknowledged claim lapses and the job is claimable again.
    pub claim_expires_ms: i64,
}

/// The durable queue between the edge that renders and the agent that writes the bytes.
pub trait PrintQueue: Send + Sync {
    /// Puts a rendered job on `agent`'s queue for `printer`, unless that printer is at `cap`.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the queue cannot be written.
    fn enqueue(
        &self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
        queued_at_ms: i64,
        expires_at_ms: i64,
        cap: u32,
    ) -> impl Future<Output = Result<Enqueued, PortError>> + Send;

    /// Leases the oldest claimable job for each printer this agent owns.
    ///
    /// At most one per printer: ESC/POS is a byte stream, and two jobs in flight to one print head
    /// interleave into garbage.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the queue cannot be read or the lease cannot be written.
    fn claim(
        &self,
        agent: DeviceId,
        now_ms: i64,
        claim_expires_ms: i64,
    ) -> impl Future<Output = Result<Vec<ClaimedJob>, PortError>> + Send;

    /// Deletes an acknowledged job, reporting whether it was still queued.
    ///
    /// Scoped to the agent holding it, so one paired device cannot delete another's job unprinted.
    /// `false` is not a failure: an acknowledgement can legitimately arrive after the job expired,
    /// or a second time after a lost reply.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the delete fails.
    fn acknowledge(
        &self,
        job: EventId,
        agent: DeviceId,
    ) -> impl Future<Output = Result<bool, PortError>> + Send;

    /// Deletes every job past its TTL, reporting how many.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the delete fails.
    fn expire(&self, now_ms: i64) -> impl Future<Output = Result<u64, PortError>> + Send;
}

/// Shared by delegation, so the dispatch that enqueues and the route that claims hold one queue.
impl<T: PrintQueue + ?Sized> PrintQueue for std::sync::Arc<T> {
    fn enqueue(
        &self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
        queued_at_ms: i64,
        expires_at_ms: i64,
        cap: u32,
    ) -> impl Future<Output = Result<Enqueued, PortError>> + Send {
        (**self).enqueue(agent, printer, job, queued_at_ms, expires_at_ms, cap)
    }

    fn claim(
        &self,
        agent: DeviceId,
        now_ms: i64,
        claim_expires_ms: i64,
    ) -> impl Future<Output = Result<Vec<ClaimedJob>, PortError>> + Send {
        (**self).claim(agent, now_ms, claim_expires_ms)
    }

    fn acknowledge(
        &self,
        job: EventId,
        agent: DeviceId,
    ) -> impl Future<Output = Result<bool, PortError>> + Send {
        (**self).acknowledge(job, agent)
    }

    fn expire(&self, now_ms: i64) -> impl Future<Output = Result<u64, PortError>> + Send {
        (**self).expire(now_ms)
    }
}

/// The `500` for a document that will not encode.
///
/// Unreachable in practice — every field of a [`PrintJob`] is `Serialize` — but reporting it beats
/// writing a row whose `document` is a placeholder nothing can print.
fn encode_failed(error: &serde_json::Error) -> PortError {
    PortError::internal(
        PortName::PrinterDriver,
        format!("a rendered job could not be encoded for the queue: {error}"),
    )
}

impl PrintQueue for SqliteStore {
    async fn enqueue(
        &self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
        queued_at_ms: i64,
        expires_at_ms: i64,
        cap: u32,
    ) -> Result<Enqueued, PortError> {
        let document = serde_json::to_string(&job).map_err(|error| encode_failed(&error))?;
        let queued = SqliteStore::enqueue_print_job(
            self,
            store_sqlite::QueuedPrintJob {
                job_id: job.job_id.to_string(),
                store_id: job.store_id.to_string(),
                printer_device_id: printer.to_string(),
                agent_device_id: agent.to_string(),
                document,
            },
            queued_at_ms,
            expires_at_ms,
            cap,
        )
        .await?;
        Ok(match queued {
            store_sqlite::PrintEnqueue::Queued => Enqueued::Queued,
            store_sqlite::PrintEnqueue::QueueFull => Enqueued::QueueFull,
            store_sqlite::PrintEnqueue::AlreadyQueued => Enqueued::AlreadyQueued,
        })
    }

    async fn claim(
        &self,
        agent: DeviceId,
        now_ms: i64,
        claim_expires_ms: i64,
    ) -> Result<Vec<ClaimedJob>, PortError> {
        let claimed =
            SqliteStore::claim_print_jobs(self, agent.to_string(), now_ms, claim_expires_ms)
                .await?;
        Ok(claimed
            .into_iter()
            .filter_map(|row| {
                // A row this edge cannot decode is skipped rather than failing the whole claim: the
                // agent's other printers still have work, and the undecodable row expires at its
                // TTL like any other. Unreachable through this module — every document here was
                // written by `enqueue` above — so it means the file was edited outside the edge.
                let printer = row.printer_device_id.parse().ok().map(DeviceId::new);
                let job = serde_json::from_str::<PrintJob>(&row.document).ok();
                if let (Some(printer), Some(job)) = (printer, job) {
                    return Some(ClaimedJob {
                        printer,
                        job,
                        claim_expires_ms: row.claim_expires_at,
                    });
                }
                tracing::error!(
                    job_id = %row.job_id,
                    "a queued print job could not be read back and was skipped"
                );
                None
            })
            .collect())
    }

    async fn acknowledge(&self, job: EventId, agent: DeviceId) -> Result<bool, PortError> {
        SqliteStore::acknowledge_print_job(self, job.to_string(), agent.to_string()).await
    }

    async fn expire(&self, now_ms: i64) -> Result<u64, PortError> {
        SqliteStore::expire_print_jobs(self, now_ms).await
    }
}

/// One queued job, as the in-memory twin holds it.
#[derive(Debug, Clone)]
struct Held {
    printer: DeviceId,
    agent: DeviceId,
    job: PrintJob,
    queued_at_ms: i64,
    expires_at_ms: i64,
    claim_expires_ms: Option<i64>,
}

/// The same rules without a database, for the fakes-backed example and the route tests.
#[derive(Debug, Default)]
pub struct InMemoryPrintQueue {
    jobs: Mutex<HashMap<EventId, Held>>,
}

impl InMemoryPrintQueue {
    /// A queue nobody has enqueued anything on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<EventId, Held>> {
        // A poisoned lock means another thread panicked holding it. The map is a plain record with
        // no invariant a panic could have half-broken, so continuing beats failing every enqueue.
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PrintQueue for InMemoryPrintQueue {
    async fn enqueue(
        &self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
        queued_at_ms: i64,
        expires_at_ms: i64,
        cap: u32,
    ) -> Result<Enqueued, PortError> {
        let mut jobs = self.locked();
        if jobs.contains_key(&job.job_id) {
            return Ok(Enqueued::AlreadyQueued);
        }
        let queued = jobs
            .values()
            .filter(|held| held.printer == printer && held.expires_at_ms > queued_at_ms)
            .count();
        if u64::try_from(queued).unwrap_or(u64::MAX) >= u64::from(cap) {
            return Ok(Enqueued::QueueFull);
        }
        jobs.insert(
            job.job_id,
            Held {
                printer,
                agent,
                job,
                queued_at_ms,
                expires_at_ms,
                claim_expires_ms: None,
            },
        );
        Ok(Enqueued::Queued)
    }

    async fn claim(
        &self,
        agent: DeviceId,
        now_ms: i64,
        claim_expires_ms: i64,
    ) -> Result<Vec<ClaimedJob>, PortError> {
        let mut jobs = self.locked();
        // A printer holding any live claim is skipped whole, not merely at the claimed row: filter
        // the row alone and the printer yields its *next* job, which is two writers to one print
        // head. The SQL says the same thing with a `NOT IN`.
        let busy: Vec<DeviceId> = jobs
            .values()
            .filter(|held| {
                held.agent == agent
                    && held.expires_at_ms > now_ms
                    && held.claim_expires_ms.is_some_and(|until| until > now_ms)
            })
            .map(|held| held.printer)
            .collect();
        let mut claimable: Vec<(EventId, i64)> = jobs
            .iter()
            .filter(|(_, held)| {
                held.agent == agent
                    && held.expires_at_ms > now_ms
                    && held.claim_expires_ms.is_none_or(|until| until <= now_ms)
                    && !busy.contains(&held.printer)
            })
            .map(|(id, held)| (*id, held.queued_at_ms))
            .collect();
        // Oldest first, then by id, so one printer's oldest job wins deterministically.
        claimable.sort_by_key(|(id, queued_at)| (*queued_at, *id));

        let mut taken: Vec<DeviceId> = Vec::new();
        let mut claimed = Vec::new();
        for (id, _) in claimable {
            let Some(held) = jobs.get_mut(&id) else {
                continue;
            };
            if taken.contains(&held.printer) {
                continue;
            }
            taken.push(held.printer);
            held.claim_expires_ms = Some(claim_expires_ms);
            claimed.push(ClaimedJob {
                printer: held.printer,
                job: held.job.clone(),
                claim_expires_ms,
            });
        }
        Ok(claimed)
    }

    async fn acknowledge(&self, job: EventId, agent: DeviceId) -> Result<bool, PortError> {
        let mut jobs = self.locked();
        if jobs.get(&job).is_some_and(|held| held.agent == agent) {
            jobs.remove(&job);
            return Ok(true);
        }
        Ok(false)
    }

    async fn expire(&self, now_ms: i64) -> Result<u64, PortError> {
        let mut jobs = self.locked();
        let before = jobs.len();
        jobs.retain(|_, held| held.expires_at_ms > now_ms);
        Ok(u64::try_from(before.saturating_sub(jobs.len())).unwrap_or(u64::MAX))
    }
}
