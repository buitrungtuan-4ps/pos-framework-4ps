// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Operational alerting ([ADR-0073](../../../docs/adr/0073-alerting.md), Track O2).
//!
//! The cloud watches the read models O1 built — fleet liveness, background-task health, JetStream
//! capacity, webhook state — and raises an alert when a condition fires, storing it with an
//! open→resolved lifecycle and delivering newly-opened ones to the console and any configured
//! channels. This module holds the domain model and the pure evaluator; the durable store, the
//! background loop, delivery, and the `/admin` surface land in the following slices.

pub mod eval;
pub mod model;

pub use eval::{AlertThresholds, TenantAlertInput, WebhookRef, evaluate};
pub use model::{AlertKind, AlertSeverity, FiringAlert};
