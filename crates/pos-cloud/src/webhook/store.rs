// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The seam that persists webhook endpoints ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
//!
//! The delivery engine ([`super::dispatch`]) holds an endpoint as in-memory runtime state; this is
//! where the durable facts of a subscription live — which store it follows, where to `POST`, the
//! signing secret, how far its cursor has advanced, and whether it has been auto-disabled. A table in
//! `store-postgres`; a fake in tests.
//!
//! The signing secret **is** stored (unlike an API-key secret, which the cloud stores only as a hash,
//! [ADR-0037](../../../docs/adr/0037-api-keys.md)): the cloud signs each outgoing delivery with it, so
//! it must be recoverable. It is shown to the tenant once at registration and kept server-side
//! thereafter — the same posture a webhook signing secret has anywhere.

use core::fmt;
use core::future::Future;

use pos_proto::ids::{EventId, StoreId, TenantId};
use pos_proto::ulid::Ulid;

use super::sign::SigningSecret;

/// A webhook endpoint's public identifier — a ULID minted at registration, used to address the
/// endpoint in the admin routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WebhookEndpointId(Ulid);

impl WebhookEndpointId {
    /// Wraps a ULID as an endpoint id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for WebhookEndpointId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A persisted webhook subscription: everything durable about one endpoint.
///
/// [`fmt::Debug`] is derived on the parts, but [`SigningSecret`] redacts itself, so a logged record
/// never leaks the secret.
#[derive(Debug, Clone)]
pub struct PersistedWebhook {
    /// The endpoint's id.
    pub id: WebhookEndpointId,
    /// The tenant that owns the subscription.
    pub tenant_id: TenantId,
    /// The store whose event log this endpoint follows.
    pub store_id: StoreId,
    /// The vetted destination URL (re-vetted before each delivery, [ADR-0032](../../../docs/adr/0032-webhooks.md)).
    pub url: String,
    /// The per-endpoint HMAC signing secret the cloud signs deliveries with.
    pub secret: SigningSecret,
    /// How far the endpoint's cursor has advanced, or `None` if it has delivered nothing.
    pub cursor: Option<EventId>,
    /// Whether the endpoint has been auto-disabled (a day of continuous failure) and awaits a human.
    pub disabled: bool,
}

/// An endpoint's metadata for a listing — everything but the secret, which never leaves the store.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WebhookSummary {
    /// The endpoint id (a ULID string).
    pub id: String,
    /// The store the endpoint follows (a ULID string).
    pub store_id: String,
    /// The destination URL.
    pub url: String,
    /// The cursor position (an event-id ULID string), or `null` if nothing has been delivered.
    pub cursor: Option<String>,
    /// Whether the endpoint is auto-disabled.
    pub disabled: bool,
}

/// Persists and reads webhook endpoints.
///
/// The admin routes drive [`insert`](WebhookEndpointStore::insert),
/// [`list_for_tenant`](WebhookEndpointStore::list_for_tenant) and
/// [`delete`](WebhookEndpointStore::delete); the delivery task drives
/// [`load_enabled`](WebhookEndpointStore::load_enabled),
/// [`save_cursor`](WebhookEndpointStore::save_cursor) and
/// [`set_disabled`](WebhookEndpointStore::set_disabled).
pub trait WebhookEndpointStore {
    /// Persists a newly registered endpoint.
    ///
    /// # Errors
    ///
    /// [`WebhookStoreError`] if the store could not be written.
    fn insert(
        &self,
        endpoint: &PersistedWebhook,
    ) -> impl Future<Output = Result<(), WebhookStoreError>> + Send;

    /// Lists a tenant's endpoints as metadata only — never a secret.
    ///
    /// # Errors
    ///
    /// [`WebhookStoreError`] if the store could not be read.
    fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<WebhookSummary>, WebhookStoreError>> + Send;

    /// Deletes the endpoint `id` within `tenant_id`, returning whether a row was removed. The tenant
    /// scope stops one tenant deleting another's endpoint.
    ///
    /// # Errors
    ///
    /// [`WebhookStoreError`] if the store could not be written.
    fn delete(
        &self,
        tenant_id: TenantId,
        id: WebhookEndpointId,
    ) -> impl Future<Output = Result<bool, WebhookStoreError>> + Send;

    /// Loads every enabled endpoint across the fleet, for the delivery task to advance. Disabled
    /// endpoints are excluded — they are not retried until a human re-enables them.
    ///
    /// # Errors
    ///
    /// [`WebhookStoreError`] if the store could not be read.
    fn load_enabled(
        &self,
    ) -> impl Future<Output = Result<Vec<PersistedWebhook>, WebhookStoreError>> + Send;

    /// Advances an endpoint's persisted cursor after a successful delivery.
    ///
    /// # Errors
    ///
    /// [`WebhookStoreError`] if the store could not be written.
    fn save_cursor(
        &self,
        id: WebhookEndpointId,
        cursor: EventId,
    ) -> impl Future<Output = Result<(), WebhookStoreError>> + Send;

    /// Sets an endpoint's disabled flag — the delivery task disables one that has failed for a day,
    /// and an operator clears it to re-enable.
    ///
    /// # Errors
    ///
    /// [`WebhookStoreError`] if the store could not be written.
    fn set_disabled(
        &self,
        id: WebhookEndpointId,
        disabled: bool,
    ) -> impl Future<Output = Result<(), WebhookStoreError>> + Send;
}

/// A failure of the webhook-endpoint store itself — the database is unreachable.
#[derive(Debug, thiserror::Error)]
#[error("the webhook-endpoint store failed: {0}")]
pub struct WebhookStoreError(String);

impl WebhookStoreError {
    /// A store failure carrying a human-readable reason (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
