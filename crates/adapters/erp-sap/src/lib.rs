// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The SAP [`ErpSink`](pos_ports::erp::ErpSink) adapter, over HTTPS
//! ([ADR-0059](../../../docs/adr/0059-erp-adapter.md)).
//!
//! `docs/architecture.md` §6.1 puts ERP posting behind the `ErpSink` port: a nightly, whole-day
//! posting of revenue and consumption, keyed by the **trading** day. This crate maps the port's two
//! operations — [`post`](pos_ports::erp::ErpSink::post) and
//! [`posted`](pos_ports::erp::ErpSink::posted) — onto a REST ERP API.
//!
//! # A transport seam, and a pure core
//!
//! The socket lives behind [`ErpTransport`]; building the batch body and mapping the ERP's HTTP status
//! to the right [`PortError`](pos_ports::PortError) are pure. That is what lets the shared `ErpSink`
//! contract suite run in the fast pull-request gate against a stateful stub ERP, while the real TLS
//! path ([`TlsErpTransport`]) is exercised in the gated integration lane and the soak — the split
//! [ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md) drew for the webhook sender and
//! [ADR-0058](../../../docs/adr/0058-shipping-adapters.md) for the couriers.
//!
//! # Nightly, and off the sales path
//!
//! Posting is nightly by design ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)):
//! an ERP is a system of record for accounting periods, and putting a finance system on the sales path
//! is exactly what the store's offline-first autonomy forbids. A batch carries a `revision` so a late
//! void or a reprocessed day can supersede an earlier posting rather than double-count it.
//!
//! # The exact ERP wire is pinned in the gated lane
//!
//! The concrete endpoint paths, authentication, and account-code vocabulary here are this adapter's
//! own mapping (ADR-0059); the exact SAP strings are confirmed against the live system in the gated
//! integration lane. What the fast gate proves is the port *semantics*: idempotency by revision,
//! whole-or-nothing validation, revision supersession, and keying on the trading day.

#![forbid(unsafe_code)]

mod client;
mod wire;

pub use client::HttpSapErp;
pub use wire::{ErpTransport, HttpResponse, Method, TlsErpTransport, TransportError};
