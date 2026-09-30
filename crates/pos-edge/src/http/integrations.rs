// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The vendor connections this store was published
//! ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md) decision 5).
//!
//! `GET /api/integrations` lists what the `integrations` node says serves this store — the family,
//! the provider and the name the console gave it — so the Devices screen can say which e-invoice
//! provider or card terminal the store is set up for, and a store whose node never arrived says it
//! runs its offline paths. No setting is returned: a terminal's address is the driver's business,
//! and nothing a till draws needs it.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::WireEnum;

use crate::app::Edge;

/// One connection, as the Devices screen lists it.
#[derive(Debug, Serialize)]
pub(crate) struct IntegrationResponse {
    connection_id: String,
    /// The family's wire token; `INTEGRATION_FAMILY_UNSPECIFIED` for one this build does not know.
    family: &'static str,
    provider_id: String,
    display_name: String,
}

/// `GET /api/integrations` — the connections that serve this store, most specific first.
pub(crate) async fn list<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let session = edge.session();
    let integrations: Vec<IntegrationResponse> = session
        .integrations
        .integrations()
        .iter()
        .map(|integration| IntegrationResponse {
            connection_id: integration.connection_id.clone(),
            family: integration.family.known().as_wire(),
            provider_id: integration.provider_id.clone(),
            display_name: integration.display_name.as_str().to_owned(),
        })
        .collect();
    Json(integrations).into_response()
}
