// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which paired device answers for which terminal's print agent
//! ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! # Two identity spaces, and the seam between them
//!
//! The `agent_device_id` is **cloud-approved**: the `TERMINAL` entry a console admin created, which
//! reaches the store in the published `devices` node. The paired device id is **locally minted** by a
//! pairing, and [`crate::pairing`] says so in as many words — *"unique per pairing; the cloud's
//! approved-device registry is a separate identity this local id does not claim to be."* Nothing else
//! in the tree joins them, and a printer's `agent_device_id` is inert until something does. This is
//! that something.
//!
//! # A trait here, implemented for the concrete store
//!
//! [`crate::app::Edge`] is generic over its store, so it cannot reach a concrete store's inherent
//! table — the same shape [`crate::queue::QueueNumberAuthority`] and
//! [`crate::lease_state::LeaseAuthority`] already take, and for the same reason. In the field this is
//! [`store_sqlite::SqliteStore`], whose single writer thread serialises the binding with every other
//! allocation; the fakes-backed example and the route tests use [`InMemoryPrintAgents`], which holds
//! the same rules without a database.
//!
//! # What the binding buys, and what it does not
//!
//! It is exclusive in both directions and refuses rather than promotes. Take-over-by-latest is the
//! tempting simplification and it is wrong: two devices holding one identity both claim from the same
//! queue, so each ticket prints exactly once — on whichever box grabbed it. If one of them is a phone
//! in an apron, half the kitchen's tickets are in a pocket and nobody finds out until service.
//! Refusing is visible; splitting is not.
//!
//! What it does **not** buy is any proof about which physical machine is on the other end. The
//! framework has no device attestation: a paired device is whatever holds a token, so a manager who
//! signs in on a phone and claims a terminal entry gets a phone as the agent. The two gates on the
//! route make that impossible to do casually — an approved entry a console admin created, a manager's
//! PIN at the box, and a deliberate exclusive claim — and the binding being readable is what keeps it
//! from happening invisibly.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use pos_ports::PortError;
use pos_proto::ids::DeviceId;
use store_sqlite::SqliteStore;

/// How many unexpired jobs one **printer** may hold before an enqueue refuses.
///
/// Per printer, not per agent, so one jammed kitchen printer cannot consume the receipt printer's
/// budget on the same terminal. The table's total bound follows and is finite by construction: this
/// many rows times the printers the published `devices` node lists, a list only the console grows.
pub const MAX_QUEUED_PER_PRINTER: u32 = 200;

/// How long a queued job stays deliverable.
///
/// **A ticket printed an hour late is worse than a ticket that visibly failed.** The late one is
/// cooked against a bill that settled, walks out to a table that left, and costs the food twice; the
/// failed one is a cashier reading a refusal while the guest is still standing there. That is the
/// whole argument for the TTL, and why it is ten minutes rather than a number chosen to look
/// generous.
pub const JOB_TTL: Duration = Duration::from_secs(600);

/// How long a claimed job stays leased before it returns to the queue.
///
/// An agent that dies holding a job does not hold it forever.
pub const CLAIM_LEASE: Duration = Duration::from_secs(30);

/// How long an agent may go without asking for work before the enqueue treats it as gone.
///
/// Read *before* the queue is touched: a queue must not start building behind a box that is not
/// there. Longer than [`AGENT_PARK`] by design — a healthy agent re-asks as soon as its park ends,
/// so one missed cycle must not read as silence.
pub const AGENT_SILENCE: Duration = Duration::from_secs(60);

/// How long `GET /api/print/jobs` holds a request open before answering empty.
///
/// Bounded on both axes. A held socket that is never answered is indistinguishable from a dead one,
/// and 4G NAT bindings are reaped; so the park ends, the agent asks again, and the fallback re-read
/// inside it is derived as `AGENT_PARK / 2` rather than declared — a separate constant could be set
/// to a value that computes to zero re-reads and silently removes the safety net.
pub const AGENT_PARK: Duration = Duration::from_secs(20);

/// What a claim did.
///
/// The two refusals are kept apart because they send an operator to different places: one says *that
/// terminal is already answered for*, the other says *this box already answers for something else*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentClaim {
    /// This device now holds the identity — including when it already did, so an agent that restarts
    /// and re-claims does not need a manager at the box a second time.
    Bound,
    /// Another paired device holds this terminal.
    HeldByAnotherDevice,
    /// This device already holds a different terminal. Release that one first.
    DeviceHoldsAnotherAgent,
}

/// Who holds a terminal, and when they last asked for work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentStanding {
    /// The paired device answering for this terminal.
    pub paired_device: DeviceId,
    /// Unix milliseconds of the agent's last claim against the queue.
    ///
    /// Written by the act that proves liveness — asking for work — rather than by a ping of its own,
    /// because a separate ping is a second thing that can be true while printing is broken.
    pub last_seen_ms: i64,
}

/// One binding and the job that has waited longest behind it — what the heartbeat reports so an
/// operator learns about a stalled terminal before the night ends
/// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
///
/// Two opaque identifiers and an instant. No document, no line, no table, no name: a stalled agent
/// is diagnosed from *which box*, *for which terminal*, and *how long*, and nothing a ticket says
/// helps with any of those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentBacklog {
    /// The terminal a console admin created and a manager bound.
    pub agent: DeviceId,
    /// The paired device answering for it.
    pub paired_device: DeviceId,
    /// Unix milliseconds at which the oldest still-unacknowledged job was queued, or `None` when
    /// nothing is waiting.
    ///
    /// An acknowledgement deletes its row, so a row still here is a ticket that has not printed.
    /// `None` is the healthy answer and is deliberately distinct from a zero age.
    pub oldest_queued_ms: Option<i64>,
}

/// The durable record of which device answers for which terminal.
pub trait PrintAgents: Send + Sync {
    /// Binds `agent` to `device`, exclusively.
    ///
    /// Recorded **before** the route answers, the ordering [`crate::pairing`] already uses: a crash
    /// between the write and the reply leaves an operator claiming again rather than holding a
    /// standing the box forgot.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the record cannot be written.
    fn claim(
        &self,
        agent: DeviceId,
        device: DeviceId,
        now_ms: i64,
    ) -> impl Future<Output = Result<AgentClaim, PortError>> + Send;

    /// Releases a binding, answering whether `device` actually held it.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the record cannot be written.
    fn revoke(
        &self,
        agent: DeviceId,
        device: DeviceId,
    ) -> impl Future<Output = Result<bool, PortError>> + Send;

    /// Records that a bound agent asked for work, reporting whether the binding was still there.
    ///
    /// Liveness is written by the act that proves it — asking the queue for work — rather than by a
    /// ping of its own, because a separate ping is a second thing that can be true while printing
    /// is broken.
    ///
    /// It never *creates* a binding. This runs on every claim and can race a manager revoking the
    /// binding at the till; a write that inserted would resurrect what the revoke just released,
    /// which is the one thing a revoke has to be able to guarantee.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the record cannot be written.
    fn heard_from(
        &self,
        agent: DeviceId,
        device: DeviceId,
        now_ms: i64,
    ) -> impl Future<Output = Result<bool, PortError>> + Send;

    /// The terminal a paired device answers for, if any — the first thing every agent route asks.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the record cannot be read.
    fn agent_for(
        &self,
        device: DeviceId,
    ) -> impl Future<Output = Result<Option<DeviceId>, PortError>> + Send;

    /// The standing of a terminal, or `None` if nobody holds it — the enqueue's first question.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the record cannot be read.
    fn standing(
        &self,
        agent: DeviceId,
    ) -> impl Future<Output = Result<Option<AgentStanding>, PortError>> + Send;

    /// Every binding in the store, each with the instant its oldest unacknowledged job was queued.
    ///
    /// The heartbeat's read, once a tick, and the only one that spans agents. `now_ms` excludes jobs
    /// the TTL has already given up on, so an expired ticket cannot keep an alert alive after the
    /// thing it was about stopped mattering.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the record cannot be read.
    fn backlogs(
        &self,
        now_ms: i64,
    ) -> impl Future<Output = Result<Vec<AgentBacklog>, PortError>> + Send;
}

/// Shared by delegation, so one record can be held in two places.
///
/// The routes that bind an agent and the dispatch that reads the binding must agree — two records
/// over one store would be two answers to "who holds this terminal" — and the trait returns
/// `impl Future`, so it cannot be erased behind `dyn`. `Arc<T>` is what lets the same value be
/// cloned into both, exactly as [`crate::queue::QueueNumberAuthority`] is shared.
impl<T: PrintAgents + ?Sized> PrintAgents for std::sync::Arc<T> {
    fn claim(
        &self,
        agent: DeviceId,
        device: DeviceId,
        now_ms: i64,
    ) -> impl Future<Output = Result<AgentClaim, PortError>> + Send {
        (**self).claim(agent, device, now_ms)
    }

    fn revoke(
        &self,
        agent: DeviceId,
        device: DeviceId,
    ) -> impl Future<Output = Result<bool, PortError>> + Send {
        (**self).revoke(agent, device)
    }

    fn heard_from(
        &self,
        agent: DeviceId,
        device: DeviceId,
        now_ms: i64,
    ) -> impl Future<Output = Result<bool, PortError>> + Send {
        (**self).heard_from(agent, device, now_ms)
    }

    fn agent_for(
        &self,
        device: DeviceId,
    ) -> impl Future<Output = Result<Option<DeviceId>, PortError>> + Send {
        (**self).agent_for(device)
    }

    fn standing(
        &self,
        agent: DeviceId,
    ) -> impl Future<Output = Result<Option<AgentStanding>, PortError>> + Send {
        (**self).standing(agent)
    }

    fn backlogs(
        &self,
        now_ms: i64,
    ) -> impl Future<Output = Result<Vec<AgentBacklog>, PortError>> + Send {
        (**self).backlogs(now_ms)
    }
}

/// Turns the adapter's string-keyed answer back into an id, dropping one it cannot parse.
///
/// Unparseable is unreachable through this module — every id written here was rendered from a
/// [`DeviceId`] — so it means the file was edited outside the edge. Answering "nobody holds it" is
/// the safe direction: printing refuses rather than routing a ticket at a name nobody can resolve.
fn parse_device(id: &str) -> Option<DeviceId> {
    id.parse().ok().map(DeviceId::new)
}

impl PrintAgents for SqliteStore {
    async fn claim(
        &self,
        agent: DeviceId,
        device: DeviceId,
        now_ms: i64,
    ) -> Result<AgentClaim, PortError> {
        let claimed =
            SqliteStore::claim_print_agent(self, agent.to_string(), device.to_string(), now_ms)
                .await?;
        Ok(match claimed {
            store_sqlite::PrintAgentClaim::Bound => AgentClaim::Bound,
            store_sqlite::PrintAgentClaim::HeldByAnotherDevice => AgentClaim::HeldByAnotherDevice,
            store_sqlite::PrintAgentClaim::DeviceHoldsAnotherAgent => {
                AgentClaim::DeviceHoldsAnotherAgent
            }
        })
    }

    async fn revoke(&self, agent: DeviceId, device: DeviceId) -> Result<bool, PortError> {
        SqliteStore::revoke_print_agent(self, agent.to_string(), device.to_string()).await
    }

    async fn heard_from(
        &self,
        agent: DeviceId,
        device: DeviceId,
        now_ms: i64,
    ) -> Result<bool, PortError> {
        SqliteStore::touch_print_agent(self, agent.to_string(), device.to_string(), now_ms).await
    }

    async fn agent_for(&self, device: DeviceId) -> Result<Option<DeviceId>, PortError> {
        Ok(
            SqliteStore::print_agent_for_device(self, device.to_string())
                .await?
                .as_deref()
                .and_then(parse_device),
        )
    }

    async fn standing(&self, agent: DeviceId) -> Result<Option<AgentStanding>, PortError> {
        Ok(SqliteStore::print_agent_standing(self, agent.to_string())
            .await?
            .and_then(|standing| {
                Some(AgentStanding {
                    paired_device: parse_device(&standing.paired_device_id)?,
                    last_seen_ms: standing.last_seen_at,
                })
            }))
    }

    async fn backlogs(&self, now_ms: i64) -> Result<Vec<AgentBacklog>, PortError> {
        Ok(SqliteStore::print_agent_backlogs(self, now_ms)
            .await?
            .into_iter()
            // A row whose ids will not parse is dropped rather than reported, the same rule
            // `agent_for` and `standing` already follow: this adapter cannot write such a row, so one
            // means the file was tampered with, and a heartbeat is not where that is diagnosed.
            .filter_map(|row| {
                Some(AgentBacklog {
                    agent: parse_device(&row.agent_device_id)?,
                    paired_device: parse_device(&row.paired_device_id)?,
                    oldest_queued_ms: row.oldest_queued_at,
                })
            })
            .collect())
    }
}

/// The same rules without a database, for the fakes-backed example and the route tests.
///
/// It holds **bindings only**: the queue lives in [`crate::print_queue::InMemoryPrintQueue`], a
/// separate value, so this twin cannot compute the join the shipped adapter does in one statement.
/// It therefore holds the *answer* instead of deriving it — see [`Self::stage_oldest_queued`] — and
/// the join itself is proven against a real database in store-sqlite's own `print_queue` tests. A
/// twin that silently reported "nothing is ever waiting" would be worse than one that says plainly
/// it is being told.
#[derive(Debug, Default)]
pub struct InMemoryPrintAgents {
    bindings: Mutex<HashMap<DeviceId, AgentStanding>>,
    staged: Mutex<HashMap<DeviceId, i64>>,
}

impl InMemoryPrintAgents {
    /// A store nobody has claimed anything in.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stages the instant [`PrintAgents::backlogs`] should report for `agent`'s oldest waiting job.
    ///
    /// The twin holds no queue, so a caller that wants to exercise the reporting above it says what
    /// the answer is. Unstaged agents report `None`, which is the healthy state.
    pub fn stage_oldest_queued(&self, agent: DeviceId, queued_ms: i64) {
        self.staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(agent, queued_ms);
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<DeviceId, AgentStanding>> {
        // A poisoned lock means another thread panicked holding it. The map is a plain record with
        // no invariant a panic could have half-broken, so continuing beats failing every claim.
        self.bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PrintAgents for InMemoryPrintAgents {
    async fn claim(
        &self,
        agent: DeviceId,
        device: DeviceId,
        now_ms: i64,
    ) -> Result<AgentClaim, PortError> {
        let mut bindings = self.locked();
        if let Some(standing) = bindings.get(&agent)
            && standing.paired_device != device
        {
            return Ok(AgentClaim::HeldByAnotherDevice);
        }
        if bindings
            .iter()
            .any(|(held, standing)| *held != agent && standing.paired_device == device)
        {
            return Ok(AgentClaim::DeviceHoldsAnotherAgent);
        }
        bindings.insert(
            agent,
            AgentStanding {
                paired_device: device,
                last_seen_ms: now_ms,
            },
        );
        Ok(AgentClaim::Bound)
    }

    async fn revoke(&self, agent: DeviceId, device: DeviceId) -> Result<bool, PortError> {
        let mut bindings = self.locked();
        if bindings
            .get(&agent)
            .is_some_and(|standing| standing.paired_device == device)
        {
            bindings.remove(&agent);
            return Ok(true);
        }
        Ok(false)
    }

    async fn heard_from(
        &self,
        agent: DeviceId,
        device: DeviceId,
        now_ms: i64,
    ) -> Result<bool, PortError> {
        let mut bindings = self.locked();
        match bindings.get_mut(&agent) {
            Some(standing) if standing.paired_device == device => {
                standing.last_seen_ms = now_ms;
                Ok(true)
            }
            // Never an insert: see the trait's doc comment.
            _ => Ok(false),
        }
    }

    async fn agent_for(&self, device: DeviceId) -> Result<Option<DeviceId>, PortError> {
        Ok(self
            .locked()
            .iter()
            .find(|(_, standing)| standing.paired_device == device)
            .map(|(agent, _)| *agent))
    }

    async fn standing(&self, agent: DeviceId) -> Result<Option<AgentStanding>, PortError> {
        Ok(self.locked().get(&agent).copied())
    }

    async fn backlogs(&self, _now_ms: i64) -> Result<Vec<AgentBacklog>, PortError> {
        let staged = self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut backlogs: Vec<AgentBacklog> = self
            .locked()
            .iter()
            .map(|(agent, standing)| AgentBacklog {
                agent: *agent,
                paired_device: standing.paired_device,
                oldest_queued_ms: staged.get(agent).copied(),
            })
            .collect();
        // The adapter orders by agent id; a caller comparing the two must not have to care which it
        // is talking to.
        backlogs.sort_by_key(|backlog| backlog.agent);
        Ok(backlogs)
    }
}
