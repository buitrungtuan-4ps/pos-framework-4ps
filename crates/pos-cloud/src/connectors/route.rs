// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /connectors/{tenant_id}/{connection_id}/orders`: where a delivery vendor sends its own
//! webhook ([ADR-0083](../../../../docs/adr/0083-integration-doctrine.md) decision 3).
//!
//! The vendor holds no API key: its signature, under the secret sealed on the tenant's connection,
//! is its credential. In order: the connection, enabled, store-scoped and of a provider this cloud
//! has a connector for (one `404` for anything else, so the route maps nothing for a prober); the
//! connection's budget; the signature over the raw body (`401`); the vendor's order and the item
//! map (`400`, `422`); the recipient, when the order names one, kept apart as a personal record
//! ([`super::recipient`]; `400` for a bad field); then the same intake `/v1/orders` submits to.
//!
//! A refused request logs the connection's id and why, never the body.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::post;
use pos_cloud_api::response::{api_error, api_error_with_details, parse_ulid_fields};
use pos_ports::PortError;
use pos_ports::order_in::OrderIn;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::integrations::SettingValue;

use super::recipient::{RecipientStore, keep_recipient, recipient_subject_id};
use super::{MappingRefusal, OrderConnector, item_map, to_inbound};
use crate::auth::rate_limit::SlidingRateLimiter;
use crate::connections::{Connection, ConnectionStore, ScopeLevel, SealedFor};
use crate::http::too_many_requests;
use crate::keys::{Keys, TenantSealer};
use crate::orders::{intake_error, order_response};

/// The largest vendor webhook body accepted, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Webhooks a second one connection may send.
pub const PER_CONNECTION_PER_SECOND: usize = 20;

/// The connection setting each connector reads its item map from.
pub const ITEM_MAP: &str = super::demo::ITEM_MAP;
/// The connection secret each connector verifies its vendor's signature with.
pub const WEBHOOK_SECRET: &str = super::demo::WEBHOOK_SECRET;

/// What the route shares.
pub(crate) struct ConnectorState<S, X, C, R> {
    connections: S,
    intake: X,
    recipients: R,
    clock: C,
    connectors: Arc<[Arc<dyn OrderConnector>]>,
    keys: Option<Keys>,
    limiter: SlidingRateLimiter,
}

impl<S: Clone, X: Clone, C: Clone, R: Clone> Clone for ConnectorState<S, X, C, R> {
    fn clone(&self) -> Self {
        Self {
            connections: self.connections.clone(),
            intake: self.intake.clone(),
            recipients: self.recipients.clone(),
            clock: self.clock.clone(),
            connectors: Arc::clone(&self.connectors),
            keys: self.keys.clone(),
            limiter: self.limiter.clone(),
        }
    }
}

/// Builds the connector router over `connectors`, each serving one provider. A vendor order's
/// recipient is kept in `recipients` (ADR-0215 decision 3).
pub fn connector_router<S, X, C, R>(
    (connections, intake, recipients): (S, X, R),
    clock: C,
    connectors: Vec<Arc<dyn OrderConnector>>,
    keys: Option<Keys>,
) -> Router
where
    S: ConnectionStore + Clone + Send + Sync + 'static,
    X: OrderIn + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    R: RecipientStore + Clone + 'static,
{
    Router::new()
        .route(
            "/connectors/{tenant_id}/{connection_id}/orders",
            post(vendor_order::<S, X, C, R>),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(ConnectorState {
            connections,
            intake,
            recipients,
            clock,
            connectors: connectors.into(),
            keys,
            limiter: SlidingRateLimiter::new(PER_CONNECTION_PER_SECOND, 1),
        })
}

/// The one answer for a connection that is not there to serve a vendor.
fn no_connector() -> Response {
    api_error(ErrorStatus::NotFound, "no such connector")
}

/// `POST /connectors/{tenant_id}/{connection_id}/orders` — a vendor's order, verified, mapped and
/// submitted to the store.
async fn vendor_order<S, X, C, R>(
    State(state): State<ConnectorState<S, X, C, R>>,
    Path((tenant_id, connection_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response
where
    S: ConnectionStore + Clone + Send + Sync + 'static,
    X: OrderIn + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    R: RecipientStore + Clone + 'static,
{
    let Ok([tenant_id, connection_id]) =
        parse_ulid_fields([("tenant_id", &tenant_id), ("connection_id", &connection_id)])
    else {
        return no_connector();
    };
    let tenant_id = TenantId::new(tenant_id);
    let connection_id = connection_id.to_string();
    if let Err(retry) = state
        .limiter
        .check_and_record(core::slice::from_ref(&connection_id), state.clock.now())
    {
        return too_many_requests("too many webhooks for this connection", retry);
    }
    let (connection, store_id, connector) = match serving(&state, tenant_id, &connection_id).await {
        Ok(serving) => serving,
        Err(refusal) => return refusal,
    };
    let tenant_keys = match &state.keys {
        Some(keys) => keys.for_tenant(tenant_id).await.ok(),
        None => None,
    };
    let Some(secret) = webhook_secret(&connection, tenant_id, tenant_keys.as_ref()) else {
        tracing::error!(%connection_id, "a connector's webhook secret cannot be opened");
        return api_error(ErrorStatus::Unavailable, "the connector is unavailable");
    };
    if !connector.verify(&headers, &body, secret.as_str()) {
        tracing::warn!(%connection_id, "a vendor webhook's signature did not verify");
        return api_error(
            ErrorStatus::Unauthenticated,
            "the signature does not verify",
        );
    }
    let order = match connector.read(&body) {
        Ok(order) => order,
        Err(reason) => return api_error(ErrorStatus::InvalidArgument, reason),
    };
    let map = match connection.settings.get(ITEM_MAP) {
        Some(SettingValue::Text(text)) => item_map(text),
        _ => Err(MappingRefusal::BadItemMap),
    };
    let mut inbound = match map.and_then(|map| to_inbound(&connection_id, store_id, &order, &map)) {
        Ok(inbound) => inbound,
        Err(refusal) => return mapping_refused(&connection_id, &refusal),
    };
    if let Some(recipient) = &order.recipient {
        let subject_id =
            recipient_subject_id(tenant_id, &inbound.external_reference, inbound.placed_at);
        let kept = keep_recipient(
            &state.recipients,
            (tenant_id, store_id),
            subject_id,
            recipient,
            state.clock.now(),
        )
        .await;
        if let Err(error) = kept {
            return recipient_refused(&connection_id, &error);
        }
        inbound.subject_id = Some(subject_id);
    }
    match state.intake.submit(&inbound).await {
        Ok(acceptance) => order_response(&acceptance),
        Err(error) => intake_error(&error),
    }
}

/// The connection, the store it serves and its connector, or the one `404`.
async fn serving<S, X, C, R>(
    state: &ConnectorState<S, X, C, R>,
    tenant_id: TenantId,
    connection_id: &str,
) -> Result<(Connection, StoreId, Arc<dyn OrderConnector>), Response>
where
    S: ConnectionStore + Clone + Send + Sync + 'static,
{
    let connection = match state.connections.get(tenant_id, connection_id).await {
        Ok(Some(row)) => row.record,
        Ok(None) => return Err(no_connector()),
        Err(error) => {
            tracing::error!(%error, "the connection store failed");
            return Err(api_error(
                ErrorStatus::Unavailable,
                "the connector is unavailable",
            ));
        }
    };
    let store_id = match (
        connection.enabled,
        connection.scope_level,
        &connection.scope_id,
    ) {
        (true, ScopeLevel::Store, Some(store)) => store.parse::<StoreId>().ok(),
        _ => None,
    };
    let connector = state
        .connectors
        .iter()
        .find(|connector| connector.provider_id() == connection.provider_id)
        .cloned();
    match (store_id, connector) {
        (Some(store_id), Some(connector)) => Ok((connection, store_id, connector)),
        _ => Err(no_connector()),
    }
}

/// The connection's opened webhook secret, or `None` when it has none or it cannot be opened.
fn webhook_secret(
    connection: &Connection,
    tenant_id: TenantId,
    tenant_keys: Option<&TenantSealer>,
) -> Option<zeroize::Zeroizing<String>> {
    let sealed = connection.sealed_secrets.get(WEBHOOK_SECRET)?;
    tenant_keys?
        .open(
            SealedFor {
                tenant_id,
                connection_id: &connection.connection_id,
                field_key: WEBHOOK_SECRET,
            },
            sealed,
        )
        .ok()
}

/// The answer a recipient that could not be kept becomes: its field's reason for a bad one, never
/// its value, else unavailable. The log names the connection only.
fn recipient_refused(connection_id: &str, error: &PortError) -> Response {
    if error.status() == ErrorStatus::InvalidArgument {
        return api_error(ErrorStatus::InvalidArgument, error.message());
    }
    tracing::error!(%connection_id, "a delivery recipient could not be kept");
    api_error(ErrorStatus::Unavailable, "the connector is unavailable")
}

/// The answer a mapping refusal becomes: the vendor's own item code named back, which is the
/// vendor's data, so it can fix its menu or the tenant its map.
fn mapping_refused(connection_id: &str, refusal: &MappingRefusal) -> Response {
    match refusal {
        MappingRefusal::BadItemMap => {
            tracing::warn!(%connection_id, "a connection's item map is malformed");
            api_error(
                ErrorStatus::Unprocessable,
                "the connection's item map is malformed",
            )
        }
        MappingRefusal::UnmappedItem(code) => api_error_with_details(
            ErrorStatus::Unprocessable,
            "an item is not in the connection's item map: nothing was ordered",
            &[("items", code.as_str())],
        ),
        MappingRefusal::Invalid(reason) => api_error(ErrorStatus::InvalidArgument, *reason),
    }
}
