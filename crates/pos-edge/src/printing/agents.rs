// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Printing through a print agent ([ADR-0112](../../../../docs/adr/0112-print-agents.md)): the lane a
//! finished job takes to the device that writes its bytes, and the drawer kick it waits on.

use std::collections::HashMap;
use std::fmt;
use std::pin::Pin;
use std::sync::Mutex;

use pos_ports::PortError;
use pos_ports::printer::PrintJob;
use pos_proto::ClockSource;
use pos_proto::ids::{DeviceId, EventId};
use tokio::sync::oneshot;

use crate::print_agent::{AGENT_SILENCE, JOB_TTL, KICK_WAIT, MAX_QUEUED_PER_PRINTER};
use crate::printing::PrintOutcome;

/// Where a job goes when its printer names an agent.
///
/// **Dyn-compatible on purpose, unlike the three seams underneath it.** `PrintAgents`, `PrintQueue`
/// and `PrintWake` all return `impl Future`, so none of them can be erased — and [`Printers`] is
/// held as one `Arc<Printers>` in an axum extension by every composition and every route test. Making
/// it generic over three more parameters would push those three into the signature of every caller
/// that only ever wanted to print a receipt. So the erasure happens once, here, at the point where
/// the three become one question: *can this agent take this job, and does it now have it?*
///
/// The binding answers one more question, and this is where a route can ask it: which terminal a
/// paired device is, which decides where that till's guests' paper prints
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 4).
pub trait AgentDispatch: Send + Sync {
    /// Offers `job` to `agent` for `printer`, answering with what the till should say.
    ///
    /// Never an error: a printer that is down must not roll back a bill the guest has already paid,
    /// which is the same rule the direct path follows.
    fn enqueue<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>>;

    /// Offers a kick, a job that is [`PrintDocument::kick`] alone, to `agent` for `printer`, under
    /// the same rules as [`Self::enqueue`] but deliverable only for [`KICK_WAIT`]
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
    ///
    /// A dispatch that cannot carry one refuses it, which is what the default does.
    fn kick<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        let _ = (agent, printer, job);
        Box::pin(async { PrintOutcome::AgentUnavailable })
    }

    /// The terminal paired `device` answers for, if any: the till it is.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the binding cannot be read.
    fn terminal_of<'a>(
        &'a self,
        device: DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<DeviceId>, PortError>> + Send + 'a>>;
}

/// The shipped [`AgentDispatch`]: the binding, the queue and the wake, in ADR-0112's order.
pub struct AgentLane<A, Q, W> {
    agents: A,
    queue: Q,
    wake: W,
}

impl<A, Q, W> fmt::Debug for AgentLane<A, Q, W> {
    /// The name and nothing else. Hand-written rather than derived because the three seams are not
    /// required to be `Debug` — and because the queue holds rendered documents, which
    /// `pos_ports::printer` says are never written to a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AgentLane")
    }
}

impl<A, Q, W> AgentLane<A, Q, W> {
    /// A lane over one binding record, one queue and one wake.
    ///
    /// All three must be the *same* values the agent's routes hold. Two queues over one store would
    /// be two answers to "what is waiting", and a wake nobody is parked on is a wake that never
    /// arrives.
    pub const fn new(agents: A, queue: Q, wake: W) -> Self {
        Self {
            agents,
            queue,
            wake,
        }
    }
}

impl<A, Q, W> AgentLane<A, Q, W>
where
    A: crate::print_agent::PrintAgents,
    Q: crate::print_queue::PrintQueue,
    W: crate::print_wake::PrintWake,
{
    /// Offers `job` to `agent` for `printer`, deliverable for `ttl`: a print job's [`JOB_TTL`], a
    /// kick's [`KICK_WAIT`].
    fn offer<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
        ttl: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        Box::pin(async move {
            let now_ms = crate::clock::SystemClock
                .now()
                .as_milliseconds_since_epoch();

            // 1. The agent, before the table is touched. A queue must not start building behind a
            //    box that is not there, and the ordering is what makes the cap's refusal below mean
            //    something specific.
            let standing = match self.agents.standing(agent).await {
                Ok(standing) => standing,
                Err(error) => {
                    // A store that will not say whether the agent is there is treated as *not*
                    // there: nothing is written, and the till reads a named refusal.
                    tracing::warn!(%agent, %error, "a print agent's standing could not be read");
                    return PrintOutcome::AgentUnavailable;
                }
            };
            let silence_ms = i64::try_from(AGENT_SILENCE.as_millis()).unwrap_or(i64::MAX);
            let heard_from = standing.map(|standing| standing.last_seen_ms);
            if heard_from.is_none_or(|last| now_ms.saturating_sub(last) > silence_ms) {
                tracing::warn!(
                    %agent,
                    %printer,
                    bound = heard_from.is_some(),
                    "the printer's agent is unbound or silent; nothing was queued"
                );
                return PrintOutcome::AgentUnavailable;
            }

            // 2. The cap. The instants are computed here rather than in the adapter, because the TTL
            //    is a constant in one edge module and a store that held it would be a second place
            //    to change it.
            let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
            let job_id = job.job_id;
            let queued = self
                .queue
                .enqueue(
                    agent,
                    printer,
                    job,
                    now_ms,
                    now_ms.saturating_add(ttl_ms),
                    MAX_QUEUED_PER_PRINTER,
                )
                .await;

            // 3. And the row, or the refusal the table decided.
            match queued {
                Ok(
                    crate::print_queue::Enqueued::Queued
                    | crate::print_queue::Enqueued::AlreadyQueued,
                ) => {
                    // Signalled on both. `AlreadyQueued` is a redelivery of the *same* ticket, so
                    // nothing is duplicated — and if the first enqueue signalled while nobody was
                    // parked, this one may be the signal that finally lands. The wake is an
                    // optimisation either way; the row is the truth (ADR-0062).
                    self.wake.queued(agent);
                    // The job's identifier and its destination, never its content
                    // (`pos_ports::printer`).
                    tracing::info!(%job_id, %agent, %printer, "queued to a print agent");
                    PrintOutcome::QueuedToAgent
                }
                Ok(crate::print_queue::Enqueued::QueueFull) => {
                    tracing::warn!(
                        %job_id,
                        %agent,
                        %printer,
                        "that printer holds its full allowance of unexpired jobs; nothing was queued"
                    );
                    PrintOutcome::QueueFull
                }
                Err(error) => {
                    tracing::warn!(%job_id, %agent, %printer, %error, "the print queue refused the job");
                    PrintOutcome::Unavailable
                }
            }
        })
    }
}

impl<A, Q, W> AgentDispatch for AgentLane<A, Q, W>
where
    A: crate::print_agent::PrintAgents,
    Q: crate::print_queue::PrintQueue,
    W: crate::print_wake::PrintWake,
{
    fn enqueue<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        self.offer(agent, printer, job, JOB_TTL)
    }

    fn kick<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        self.offer(agent, printer, job, KICK_WAIT)
    }

    fn terminal_of<'a>(
        &'a self,
        device: DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<DeviceId>, PortError>> + Send + 'a>> {
        Box::pin(self.agents.agent_for(device))
    }
}

/// A kick a till is waiting on, which nobody waits on once this is dropped.
pub(super) struct AwaitedKick<'a> {
    pub(super) awaited: &'a Mutex<HashMap<EventId, oneshot::Sender<()>>>,
    pub(super) job: EventId,
    pub(super) receiver: oneshot::Receiver<()>,
}

impl AwaitedKick<'_> {
    /// Whether the agent acknowledged the kick within `wait`.
    pub(super) async fn within(mut self, wait: std::time::Duration) -> bool {
        matches!(
            tokio::time::timeout(wait, &mut self.receiver).await,
            Ok(Ok(()))
        )
    }
}

impl Drop for AwaitedKick<'_> {
    fn drop(&mut self) {
        self.awaited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.job);
    }
}
