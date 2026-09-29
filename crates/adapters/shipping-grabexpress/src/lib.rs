// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The Grab Express [`ShippingDispatch`](pos_ports::shipping::ShippingDispatch) adapter, over HTTPS
//! ([ADR-0058](../../../docs/adr/0058-shipping-adapters.md)).
//!
//! Grab Express is the second of the two couriers `docs/architecture.md` §6.1 names (Ahamove is the
//! other). It is the `templates/adapter-template` extraction's first consumer: the same transport-seam
//! shape, the same pure status mapping, and the same stub-driven contract suite as `shipping-ahamove`,
//! differing only in the courier's own wire — Grab Express keys a booking by `merchant_order_id`,
//! posts to a `deliveries` collection, and reports its own status vocabulary.
//!
//! # A transport seam, and a pure core
//!
//! The socket lives behind [`CourierTransport`]; building the request, mapping the courier's status
//! vocabulary to a [`ShipmentStatus`](pos_proto::ShipmentStatus), and mapping HTTP status to the right
//! [`PortError`](pos_ports::PortError) are pure. The shared `ShippingDispatch` contract suite runs in
//! the fast gate against a stateful stub courier; the real TLS path ([`TlsCourierTransport`]) is
//! exercised in the gated integration lane and the soak.
//!
//! # The exact courier wire is pinned in the gated lane
//!
//! The concrete endpoint paths, authentication headers, and status vocabulary here are this adapter's
//! own mapping (ADR-0058); the exact Grab Express strings are confirmed against the live API in the
//! gated integration lane. What the fast gate proves is the port *semantics*: idempotent booking,
//! cancel-after-completion refused, an unknown job not-found, and a finished job still trackable.

#![forbid(unsafe_code)]

mod client;
mod wire;

pub use client::HttpGrabExpress;
pub use wire::{CourierTransport, HttpResponse, Method, TlsCourierTransport, TransportError};

use pos_proto::locale::CountryCode;
use pos_providers::{Family, ProviderDescriptor, fields};

/// This adapter as the provider catalogue lists it
/// ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)): the
/// Grab Express courier a tenant connects to from the console, and what they fill in to do it.
///
/// Only the settings [`HttpGrabExpress`] and its transport read today — the base URL and a request timeout.
/// Authentication is pinned in the gated lane with the rest of the exact wire, and gains its secret
/// field when it is: a field declared before anything reads it would be a password the console
/// asks for and nothing sends.
pub static PROVIDER: ProviderDescriptor = ProviderDescriptor {
    provider_id: "courier.grabexpress",
    family: Family::Courier,
    name_key: "provider.courier.grabexpress",
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
        assert!(registry.get("courier.grabexpress").is_some());
    }
}
