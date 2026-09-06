// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The two routes a print agent lives on
//! ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! `GET /api/print/jobs` hands out at most one job per printer the agent owns;
//! `POST /api/print/jobs/{job_id}/ack` says the job is written. There is no third route and no
//! frame on `/ws` — the queue is the only delivery path.
//!
//! # The paired gate, and no second one
//!
//! Both routes carry the paired-device gate **alone**. An agent is an unattended process: nobody is
//! signed in on it, and requiring a sign-in would mean a manager's PIN before every kitchen ticket.
//! That is weaker than the domain routes, and it is not a departure from a uniform norm, because
//! there is not one — five of the edge's `/api/*` routes already carry the paired gate only, and
//! ADR-0112 lists these two joining them. The two *human* acts on a binding, in
//! [`crate::http::print_agent`], carry both gates.
//!
//! What the paired gate buys here is the only thing that matters: the caller is a device this store
//! paired, and the binding decides which terminal it answers for. Nothing in the request names the
//! agent — a request cannot claim to be a terminal it is not bound to, because it does not get to
//! say.
//!
//! # Subscribe, then read, then wait
//!
//! [ADR-0062](../../../docs/adr/0062-the-relay-wake.md)'s rule, and this module is where it is
//! obeyed: the loop takes a wake subscription, *then* claims, *then* parks. A job enqueued between
//! the read and the park is already accounted for, and a missed signal here is a ticket sitting in
//! the queue while a guest waits at a table.
//!
//! The park is bounded on both axes. `AGENT_PARK` is the deadline; inside it the loop re-reads at
//! `AGENT_PARK / 2` whether or not anything signalled, which is ADR-0062's *"a wake is an
//! optimisation, never the correctness argument"* made mechanical. And one agent parks once: a
//! second concurrent request on the same binding reads and answers rather than parking, so an agent
//! cannot accumulate held connections against the edge.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use pos_ports::printer::PrintJob;
use pos_proto::ClockSource;
use pos_proto::ids::{DeviceId, EventId};

use crate::clock::SystemClock;
use crate::http::{bad_request, parse_ulid};
use crate::print_agent::{AGENT_PARK, CLAIM_LEASE, PrintAgents};
use crate::print_queue::PrintQueue;
use crate::print_wake::PrintWake;

/// The binding, the queue and the wake, plus who is currently parked.
pub(crate) struct PrintJobDeps<A, Q, W> {
    pub(crate) agents: A,
    pub(crate) queue: Q,
    pub(crate) wake: W,
    /// The agents holding a parked request right now, so a second one is answered instead.
    ///
    /// In-process and not durable, deliberately: it bounds held sockets on *this* edge, and a
    /// restart that forgets it is a restart that dropped the sockets it was counting.
    parked: Mutex<HashSet<DeviceId>>,
}

/// One leased job, as the agent receives it.
#[derive(Debug, Serialize)]
struct LeasedJob {
    /// The printer to open. Not on the job — a [`PrintJob`] names a *station*, and a receipt has
    /// none at all.
    printer_device_id: String,
    /// Unix milliseconds after which this lease lapses and the job returns to the queue. The agent
    /// has until then to write the bytes and acknowledge.
    claim_expires_at: i64,
    /// The finished document. Its `job_id` is what the acknowledgement carries — deliberately not
    /// repeated at this level, because two copies of an id are two things that can disagree.
    job: PrintJob,
}

/// What a claim returned. Always present, possibly empty: an empty park is the ordinary answer.
#[derive(Debug, Serialize)]
struct LeasedJobs {
    jobs: Vec<LeasedJob>,
}

/// The two routes, over the binding, the queue and the wake.
pub(crate) fn router<A, Q, W>(agents: A, queue: Q, wake: W) -> Router
where
    A: PrintAgents + 'static,
    Q: PrintQueue + 'static,
    W: PrintWake + 'static,
{
    Router::new()
        .route("/api/print/jobs", get(claim::<A, Q, W>))
        .route("/api/print/jobs/{job_id}/ack", post(acknowledge::<A, Q, W>))
        .with_state(Arc::new(PrintJobDeps {
            agents,
            queue,
            wake,
            parked: Mutex::new(HashSet::new()),
        }))
}

/// The refusal for a paired device that answers for no terminal.
///
/// `409` rather than `403`: the device's credential is fine and nothing about the request is
/// malformed. What is missing is a binding, which a manager makes at the till — so the answer says
/// that, because the reader is a log an engineer is looking at while a kitchen has no tickets.
fn no_binding() -> Response {
    (
        StatusCode::CONFLICT,
        "this device answers for no print agent; a manager binds one at the till",
    )
        .into_response()
}

/// The `503` for a queue that could not be reached.
fn store_unavailable(what: &'static str) -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, what).into_response()
}

/// Holds an agent's place in the parked set and gives it back on drop.
///
/// A guard rather than a bare insert-and-remove: the handler has several early returns and a
/// `.await` that can be cancelled when the agent hangs up, and a leaked entry would mean that agent
/// never parks again until the edge restarts.
struct ParkedOnce<'a> {
    agent: DeviceId,
    parked: &'a Mutex<HashSet<DeviceId>>,
}

impl<'a> ParkedOnce<'a> {
    /// Takes the place, or `None` when this agent already holds it.
    fn take(parked: &'a Mutex<HashSet<DeviceId>>, agent: DeviceId) -> Option<Self> {
        let mut held = parked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.insert(agent).then(|| Self { agent, parked })
    }
}

impl Drop for ParkedOnce<'_> {
    fn drop(&mut self) {
        let mut held = self
            .parked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.remove(&self.agent);
    }
}

/// Renders a claim's result, or the `503` a failed claim gets.
async fn claim_now<Q: PrintQueue>(
    queue: &Q,
    agent: DeviceId,
    now_ms: i64,
) -> Result<Vec<LeasedJob>, Response> {
    let lease_ms = i64::try_from(CLAIM_LEASE.as_millis()).unwrap_or(i64::MAX);
    match queue
        .claim(agent, now_ms, now_ms.saturating_add(lease_ms))
        .await
    {
        Ok(claimed) => Ok(claimed
            .into_iter()
            .map(|job| LeasedJob {
                printer_device_id: job.printer.to_string(),
                claim_expires_at: job.claim_expires_ms,
                job: job.job,
            })
            .collect()),
        Err(error) => {
            tracing::warn!(%agent, %error, "a print agent could not claim from the queue");
            Err(store_unavailable("the print queue is unavailable"))
        }
    }
}

/// An agent asks for work, parking for up to [`AGENT_PARK`] if there is none.
async fn claim<A, Q, W>(
    State(deps): State<Arc<PrintJobDeps<A, Q, W>>>,
    Extension(device): Extension<DeviceId>,
) -> Response
where
    A: PrintAgents,
    Q: PrintQueue,
    W: PrintWake,
{
    let agent = match resolve(&deps.agents, device).await {
        Some(Ok(agent)) => agent,
        Some(Err(response)) => return response,
        None => return no_binding(),
    };
    let now_ms = SystemClock.now().as_milliseconds_since_epoch();

    // Housekeeping before the read, so a claim never leases a job past its TTL. Cheap, and it is
    // the only thing that runs on a schedule the queue can rely on: the agent asks constantly, and
    // an edge with no agent has no queue to sweep.
    if let Err(error) = deps.queue.expire(now_ms).await {
        tracing::warn!(%error, "expired print jobs could not be swept");
    }
    // Asking for work is what proves liveness, so it is what writes it — and it never *creates* a
    // binding, so a manager's revoke that lands mid-claim wins. `false` means exactly that.
    match deps.agents.heard_from(agent, device, now_ms).await {
        Ok(true) => {}
        Ok(false) => return no_binding(),
        Err(error) => {
            tracing::warn!(%agent, %error, "a print agent's liveness could not be recorded");
            return store_unavailable("the store could not record the agent");
        }
    }

    // One park per agent. A second concurrent request reads once and answers, held socket or not.
    let Some(_parked) = ParkedOnce::take(&deps.parked, agent) else {
        return match claim_now(&deps.queue, agent, now_ms).await {
            Ok(jobs) => (StatusCode::OK, Json(LeasedJobs { jobs })).into_response(),
            Err(response) => response,
        };
    };

    // The re-read interval is derived, never declared: a separate constant could be set to a value
    // that computes to zero re-reads and silently removes the fallback.
    let step = AGENT_PARK / 2;
    let mut remaining = AGENT_PARK;
    loop {
        // Subscribe, *then* read, never the other way round (ADR-0062).
        let subscription = deps.wake.subscribe(agent);
        let jobs = match claim_now(
            &deps.queue,
            agent,
            SystemClock.now().as_milliseconds_since_epoch(),
        )
        .await
        {
            Ok(jobs) => jobs,
            Err(response) => return response,
        };
        if !jobs.is_empty() || remaining.is_zero() {
            return (StatusCode::OK, Json(LeasedJobs { jobs })).into_response();
        }
        let slice = step.min(remaining);
        // Either outcome leads to the same re-read; which one it was is what tells an operator
        // whether the wake or the fallback timer is doing the work.
        let woke = deps.wake.wait(subscription, slice).await;
        tracing::debug!(%agent, ?woke, "a parked print agent woke");
        remaining = remaining.saturating_sub(slice);
    }
}

/// An agent reports a job written.
async fn acknowledge<A, Q, W>(
    State(deps): State<Arc<PrintJobDeps<A, Q, W>>>,
    Extension(device): Extension<DeviceId>,
    Path(job_id): Path<String>,
) -> Response
where
    A: PrintAgents,
    Q: PrintQueue,
    W: PrintWake,
{
    let agent = match resolve(&deps.agents, device).await {
        Some(Ok(agent)) => agent,
        Some(Err(response)) => return response,
        None => return no_binding(),
    };
    let Some(job) = parse_ulid(&job_id).map(EventId::new) else {
        return bad_request("job_id must be a ULID");
    };
    match deps.queue.acknowledge(job, agent).await {
        Ok(deleted) => {
            // The job's identifier and its fate, never its content: a document may carry a buyer's
            // name and tax code (`pos_ports::printer`).
            tracing::info!(%job, %agent, deleted, "a print agent acknowledged a job");
            // `204` either way. `false` is an acknowledgement that arrived after the job expired,
            // or a second time after a lost reply, and the agent's next move is the same in all
            // three cases — telling it which would only enumerate what the queue holds.
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            tracing::warn!(%job, %agent, %error, "a print job could not be acknowledged");
            store_unavailable("the print queue is unavailable")
        }
    }
}

/// The terminal this device answers for: `None` for no binding, `Err` for a store that would not say.
///
/// Shared by both routes because both start with the same question, and because answering "no
/// binding" when the store is merely unreachable would tell an agent to stop asking.
async fn resolve<A: PrintAgents>(
    agents: &A,
    device: DeviceId,
) -> Option<Result<DeviceId, Response>> {
    match agents.agent_for(device).await {
        Ok(agent) => agent.map(Ok),
        Err(error) => {
            tracing::warn!(%device, %error, "a print agent binding could not be read");
            Some(Err(store_unavailable(
                "the store could not read the binding",
            )))
        }
    }
}
