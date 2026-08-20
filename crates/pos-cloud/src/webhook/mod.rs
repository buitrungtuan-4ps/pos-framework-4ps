// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Webhooks: a cursor over the event log, delivered safely to tenant-controlled URLs (P7,
//! [ADR-0032](../../../docs/adr/0032-webhooks.md)).
//!
//! A tenant registers a URL and receives every new event for a store as a signed HTTPS POST. The
//! design is deliberately boring where it can be and paranoid where it must be:
//!
//!  * **A cursor, not a queue** ([`dispatch`]). Each endpoint stores only its position in the
//!    durable log, so a dead endpoint falls behind without the cloud buffering anything for it.
//!  * **Signed and replay-bounded** ([`sign`]). Every delivery carries an HMAC-SHA256 signature over
//!    a timestamped payload, and the timestamp closes a ±5-minute replay window.
//!  * **SSRF-guarded** ([`ssrf`]). A destination is vetted — https only, no credentials, and every
//!    resolved address must be public unicast — before it is registered and before the transport
//!    connects, so a webhook URL cannot become a probe into the cloud's own network.
//!  * **Circuit-broken and auto-disabled** ([`breaker`]). A failing endpoint is backed off, and one
//!    that fails for a day is disabled until a human intervenes.
//!
//! Endpoints are isolated: one cursor and one breaker each, so no receiver can affect another's
//! delivery. [`store`] persists the durable facts of a subscription — the URL, the signing secret,
//! the cursor, the disabled flag — behind a `store-postgres` table, so registrations survive a
//! restart. [`transport`] is the concrete TLS sender that turns a signed body into bytes on the wire
//! ([`TlsWebhookSender`], ADR-0038): one HTTPS `POST` per delivery, over the rustls/hyper stack
//! already in the tree, dialing only the pre-vetted address. [`runner`] is the background task that
//! ties it all together — load the enabled fleet, re-vet, deliver after the cursor, persist progress.

pub mod breaker;
pub mod dispatch;
pub mod runner;
pub mod sign;
pub mod ssrf;
pub mod store;
pub mod transport;

pub use dispatch::{
    DeliveryError, DeliveryOutcome, WebhookEndpoint, WebhookError, WebhookTransport, deliver_next,
};
pub use sign::{Signature, SigningSecret};
pub use ssrf::{SsrfRejection, VettedUrl, vet};
pub use store::{
    PersistedWebhook, WebhookEndpointId, WebhookEndpointStore, WebhookStoreError, WebhookSummary,
};
pub use transport::TlsWebhookSender;
