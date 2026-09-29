// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The Ahamove [`ShippingDispatch`](pos_ports::shipping::ShippingDispatch) adapter, over HTTPS
//! ([ADR-0058](../../../docs/adr/0058-shipping-adapters.md)).
//!
//! Ahamove is one of the two couriers `docs/architecture.md` §6.1 names (Grab Express is the other).
//! The port gives it three operations — [`create_delivery`](pos_ports::shipping::ShippingDispatch::create_delivery),
//! [`cancel`](pos_ports::shipping::ShippingDispatch::cancel), and
//! [`track`](pos_ports::shipping::ShippingDispatch::track) — and the rule that a courier's status
//! becomes a domain event. This crate maps those three onto a REST courier API.
//!
//! # A transport seam, and a pure core
//!
//! The socket lives behind [`CourierTransport`]; everything else — building the request body, mapping
//! the courier's status vocabulary to a [`ShipmentStatus`](pos_proto::ShipmentStatus), and mapping the
//! courier's HTTP status to the right [`PortError`](pos_ports::PortError) — is pure. That is what lets
//! the shared `ShippingDispatch` contract suite run in the fast pull-request gate against a stateful
//! stub courier, while the real TLS path ([`TlsCourierTransport`]) is exercised in the gated
//! integration lane and the soak — the split [ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md)
//! drew for the webhook sender and [ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md) for the
//! edge→cloud client.
//!
//! # Callbacks do not come back through this crate
//!
//! A courier's webhook lands on `pos_cloud`'s HTTP surface and becomes a domain event there; the port
//! documents that this trait is not a server. [`track`](pos_ports::shipping::ShippingDispatch::track)
//! is the polling path that reconciles a *missed* callback, and it returns the same
//! [`Shipment`](pos_ports::shipping::Shipment) shape either way.
//!
//! # The exact courier wire is pinned in the gated lane
//!
//! The concrete endpoint paths, authentication headers, and status vocabulary here are this adapter's
//! own mapping (ADR-0058); the exact Ahamove strings are confirmed against the live API in the gated
//! integration lane, the same way [ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md) split the
//! provable request-shaping from the real socket. What the fast gate proves is the port's *semantics*:
//! idempotent booking, cancel-after-completion refused, an unknown job not-found, and a finished job
//! still trackable.

#![forbid(unsafe_code)]

mod client;
mod wire;

pub use client::HttpAhamove;
pub use wire::{CourierTransport, HttpResponse, Method, TlsCourierTransport, TransportError};

use pos_proto::locale::CountryCode;
use pos_providers::{Family, ProviderDescriptor, fields};

/// This adapter as the provider catalogue lists it
/// ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)): the
/// Ahamove courier a tenant connects to from the console, and what they fill in to do it.
///
/// Only the settings [`HttpAhamove`] and its transport read today — the base URL and a request timeout.
/// Authentication is pinned in the gated lane with the rest of the exact wire, and gains its secret
/// field when it is: a field declared before anything reads it would be a password the console
/// asks for and nothing sends.
pub static PROVIDER: ProviderDescriptor = ProviderDescriptor {
    provider_id: "courier.ahamove",
    family: Family::Courier,
    name_key: "provider.courier.ahamove",
    countries: &[CountryCode::VN],
    fields: &[fields::BASE_URL, fields::TIMEOUT_SECONDS],
    capabilities: &[],
    sandbox: false,
};

#[cfg(test)]
mod provider_tests {
    use pos_providers::Registry;

    #[test]
    fn the_catalogue_accepts_this_adapters_descriptor() {
        let registry = Registry::new([&super::PROVIDER]).expect("a well-formed descriptor");
        assert!(registry.get("courier.ahamove").is_some());
    }
}
