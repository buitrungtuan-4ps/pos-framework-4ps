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
