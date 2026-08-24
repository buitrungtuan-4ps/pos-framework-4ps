// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud [`MetricsSink`](pos_ports::metrics_sink::MetricsSink) over `VictoriaMetrics` (P7).
//!
//! Telemetry sits off the sales path ([ADR-0026](../../../docs/adr/0026-port-shapes.md)
//! `metrics_sink.rs` contract 1), so [`VmMetrics::record`] never waits on the network: a sample
//! goes into a bounded in-memory queue and a background task flushes batches to `VictoriaMetrics`.
//! When the queue is full the batch is **dropped** and `record` still returns `Ok` — a monitoring
//! backend being slow or down must never become a trading outage.
//!
//! The wire is one endpoint — `VictoriaMetrics`' JSON line import (`/api/v1/import`) — so the
//! transport is hand-rolled HTTP/1.1 over `tokio` rather than a client crate
//! ([ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)). The risk in this adapter is
//! the queueing, not the HTTP, and that is where the tests concentrate: the shared port contract
//! runs in process against a capturing transport, and a separate in-process HTTP mock pins the
//! exact import bytes. No live `VictoriaMetrics` is needed to verify the adapter.
//!
//! There is no floating point anywhere (`clippy.toml` bans it workspace-wide): a sample is an
//! `i64` and a [`MetricUnit`](pos_ports::metrics_sink::MetricUnit), and the unit rides across as a
//! label so a dashboard threshold is set against a known unit.

#![forbid(unsafe_code)]

mod sink;

pub use sink::{HttpTransport, MetricTransport, QUEUE_CAPACITY, VmMetrics};
