// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store → cloud [`MessageLink`](pos_ports::message_link::MessageLink) over NATS JetStream (P7).
//!
//! Outbound only, at-least-once. The store publishes each event to a JetStream stream and the cloud
//! consumes idempotently by ULID ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §4). There is no
//! transaction across NATS and the edge database — the outbox is what makes a crash between commit
//! and publish safe — so this port has no `begin`, only `publish`, `capacity`, and a `handshake`.
//!
//! The handshake is **local**, which is what keeps the link one-directional
//! ([ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)): it confirms the broker is
//! reachable, ensures this store's stream exists, and returns the outcome of
//! [`pos_proto::protocol::negotiate`] — the same function the in-memory fake uses, so the
//! version-overlap rule is [ADR-0024](../../../docs/adr/0024-protocol-version-negotiation.md)'s.
//! There is no responder to wait on; the cloud validates versions when it reads.
//!
//! Back-pressure is the subtle obligation: a full stream must halt synchronisation *visibly* —
//! [`NatsLink::publish`] returns `resource_exhausted` (retryable, so the events stay in the outbox)
//! and [`NatsLink::capacity`] reports the fill level the 80% alert reads. The stream is configured
//! `discard: new`, so a full stream refuses new messages rather than silently dropping the oldest.

#![forbid(unsafe_code)]

mod link;

pub use link::{NatsConfig, NatsLink};
