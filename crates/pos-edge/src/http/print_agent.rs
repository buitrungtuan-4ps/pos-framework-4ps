// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Claiming and releasing a terminal's print-agent identity, and the read the till's Devices screen
//! draws them from ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! Three routes, and they carry **both** gates: a paired device *and* an employee signed in on it
//! holding [`Permission::ManageDevices`]. Binding a terminal is a managerial act performed in front
//! of the machine, not something a process does on its own behalf — so unlike the agent's own
//! claim-and-acknowledge routes, which an unattended process calls and which therefore carry the
//! paired gate alone, these sit with the rest of the domain surface. The read carries the same two
//! because it is the binding screen's: who may bind is who needs to see what is bound.
//!
//! The permission is checked here rather than at a decide, because a binding produces no event: it
//! is durable edge-local state, like the pairing it records against, not a fact about the business.
//!
//! This module carries its own router and state for the reason [`crate::http::counter`] does:
//! [`PrintAgents`] returns `impl Future` and is not dyn-compatible, so it cannot be erased into the
//! shared `Arc<Edge<S>>` state the other domain routes use.

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;

use crate::app::Edge;
use crate::clock::SystemClock;
use crate::http::{ERROR_REASON_HEADER, bad_request, parse_ulid};
use crate::print_agent::{AgentClaim, PrintAgents};
use crate::printing::published_terminals;

/// The edge (for the roster the permission is read from) and the binding record.
pub(crate) struct PrintAgentDeps<S, A> {
    pub(crate) edge: Arc<Edge<S>>,
    pub(crate) agents: A,
}

/// The terminal a device is claiming or releasing, named by its cloud-approved device id.
#[derive(Debug, Deserialize)]
struct AgentRequest {
    /// The `TERMINAL` entry's id, as it appears in the published `devices` node.
    agent_device_id: String,
}

/// What the claim did, in the vocabulary the till renders.
#[derive(Debug, Serialize)]
struct AgentResponse {
    /// `BOUND`, `HELD_BY_ANOTHER_DEVICE` or `DEVICE_HOLDS_ANOTHER_AGENT`.
    outcome: &'static str,
}

/// The store's terminals, as the till's **This device** card lists them.
#[derive(Debug, Serialize)]
struct TerminalsResponse {
    /// The published `TERMINAL` entries, in the order the `devices` node lists them.
    terminals: Vec<TerminalResponse>,
}

/// One published terminal, and whether a device holds it.
#[derive(Debug, Serialize)]
struct TerminalResponse {
    /// The `TERMINAL` entry's id, which a claim and a release name.
    agent_device_id: String,
    /// The name the console gave the terminal.
    name: String,
    /// `THIS_DEVICE`, `ANOTHER_DEVICE` or `NONE`: never which other device.
    held: &'static str,
}

/// The three routes, over the edge and the binding record.
pub(crate) fn router<S, A>(edge: Arc<Edge<S>>, agents: A) -> Router
where
    S: EventStore + Send + Sync + 'static,
    A: PrintAgents + 'static,
{
    Router::new()
        .route(
            "/api/print/agent",
            get(terminals::<S, A>).post(claim::<S, A>),
        )
        .route("/api/print/agent/revoke", post(revoke::<S, A>))
        .with_state(Arc::new(PrintAgentDeps { edge, agents }))
}

/// Whether the signed-in person may manage devices on this box.
///
/// Read from the roster the cloud published, never from anything the request carried: the store
/// authorises against the set the console published and invents nothing (ADR-0070).
fn may_manage_devices<S>(edge: &Edge<S>, actor: Actor) -> bool {
    edge.session()
        .staff
        .grants(actor.employee_id, Permission::ManageDevices)
}

/// The refusal a person without the permission gets.
///
/// `403` rather than `401`: the device is paired and somebody *is* signed in, so sending the till
/// back to the sign-in screen would be the wrong instruction. What is missing is standing, and the
/// answer says whose, under the `PERMISSION_DENIED` token the till translates (ADR-0137).
fn needs_manage_devices() -> Response {
    (
        StatusCode::FORBIDDEN,
        [(ERROR_REASON_HEADER, "PERMISSION_DENIED")],
        "binding a print agent needs a manager signed in on this device",
    )
        .into_response()
}

/// `GET /api/print/agent`: the store's published terminals, each held by this device, another
/// device or nobody.
///
/// What the till's **This device** card is drawn from. It answers *another device* and never which
/// one, and nothing about when an agent last asked for work: a paired device's id would enumerate
/// the store's pairings, and the card needs neither. It tells the manager no more than a refused
/// claim (`HELD_BY_ANOTHER_DEVICE`) already would.
///
/// Read from the published node and the binding record as they stand, so a terminal the console
/// has not published is not listed, whatever the record holds.
async fn terminals<S, A>(
    State(deps): State<Arc<PrintAgentDeps<S, A>>>,
    Extension(actor): Extension<Actor>,
) -> Response
where
    S: EventStore + Send + Sync,
    A: PrintAgents,
{
    if !may_manage_devices(&deps.edge, actor) {
        return needs_manage_devices();
    }
    let session = deps.edge.session();
    let mut terminals = Vec::new();
    for terminal in published_terminals(&session.devices) {
        let held = match deps.agents.standing(terminal.device_id).await {
            Ok(Some(standing)) if standing.paired_device == actor.device_id => "THIS_DEVICE",
            Ok(Some(_)) => "ANOTHER_DEVICE",
            Ok(None) => "NONE",
            Err(error) => {
                tracing::warn!(%error, "could not read a print-agent binding");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the store could not read the binding",
                )
                    .into_response();
            }
        };
        terminals.push(TerminalResponse {
            agent_device_id: terminal.device_id.to_string(),
            name: terminal.name.as_str().to_owned(),
            held,
        });
    }
    Json(TerminalsResponse { terminals }).into_response()
}

/// A paired device claims a terminal's agent identity, exclusively.
async fn claim<S, A>(
    State(deps): State<Arc<PrintAgentDeps<S, A>>>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<AgentRequest>,
) -> Response
where
    S: EventStore + Send + Sync,
    A: PrintAgents,
{
    if !may_manage_devices(&deps.edge, actor) {
        return needs_manage_devices();
    }
    let Some(agent) = parse_ulid(&request.agent_device_id).map(pos_proto::ids::DeviceId::new)
    else {
        return bad_request("agent_device_id must be a ULID");
    };
    let now = SystemClock.now().as_milliseconds_since_epoch();
    match deps.agents.claim(agent, actor.device_id, now).await {
        Ok(outcome) => {
            // The identities and the outcome, never a document or a person: this log line is read by
            // whoever is working out why a terminal is not printing.
            tracing::info!(
                %agent,
                device = %actor.device_id,
                outcome = ?outcome,
                "a device claimed a print agent"
            );
            let outcome = match outcome {
                AgentClaim::Bound => "BOUND",
                AgentClaim::HeldByAnotherDevice => "HELD_BY_ANOTHER_DEVICE",
                AgentClaim::DeviceHoldsAnotherAgent => "DEVICE_HOLDS_ANOTHER_AGENT",
            };
            // `200` for all three: every one of them is a decided answer about the binding, and the
            // two refusals are states of the store rather than faults in the request. The till reads
            // `outcome` and says which.
            (StatusCode::OK, Json(AgentResponse { outcome })).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "could not record a print-agent claim");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the store could not record the binding",
            )
                .into_response()
        }
    }
}

/// A paired device releases the identity it holds — how a dead terminal is replaced.
async fn revoke<S, A>(
    State(deps): State<Arc<PrintAgentDeps<S, A>>>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<AgentRequest>,
) -> Response
where
    S: EventStore + Send + Sync,
    A: PrintAgents,
{
    if !may_manage_devices(&deps.edge, actor) {
        return needs_manage_devices();
    }
    let Some(agent) = parse_ulid(&request.agent_device_id).map(pos_proto::ids::DeviceId::new)
    else {
        return bad_request("agent_device_id must be a ULID");
    };
    match deps.agents.revoke(agent, actor.device_id).await {
        Ok(released) => {
            tracing::info!(%agent, device = %actor.device_id, released, "a device released a print agent");
            // `204` whether or not this device held it. A release is idempotent, and telling a caller
            // which case it was enumerates what other devices hold.
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "could not release a print-agent binding");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "the store could not release the binding",
            )
                .into_response()
        }
    }
}
