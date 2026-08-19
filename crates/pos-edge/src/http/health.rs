// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The health probe.
//!
//! `GET /healthz` is what a service manager (the Windows service wrapper, a systemd unit) polls to
//! decide the process is up, and what the pairing flow hits to confirm it found the right machine.
//! It reports what the binary is and which protocol it speaks — a store id, never anything about a
//! guest or an employee.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::state::AppState;

/// The health payload. Every field is either compile-time constant or a store identifier — no PII.
#[derive(Debug, Serialize)]
pub(crate) struct Health {
    /// Always `"ok"` when the process can answer at all.
    status: &'static str,
    /// The binary's package name.
    service: &'static str,
    /// The binary's version.
    version: &'static str,
    /// The cloud–edge wire protocol this binary speaks.
    protocol_version: u32,
    /// Which store this machine serves.
    store_id: String,
}

/// Answers `GET /healthz`.
pub(crate) async fn healthz(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        service: state.build.service,
        version: state.build.version,
        protocol_version: state.build.protocol_version,
        store_id: state.config.store_id.to_string(),
    })
}
