// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /api/sync` — what the status bar says about the cloud
//! ([ADR-0137](../../../../docs/adr/0137-a-deep-outbox-warns-and-never-refuses.md)).
//!
//! `docs/ui-ux.md` §4: a store that lost the internet reads *"Offline — selling normally"*, counts
//! its pending events, and blocks nothing. This is the read that lets a device say so.
//!
//! It also carries what the box last measured of its own clock (roadmap-v3 **PF4**,
//! [`crate::sntp`]), which the Devices screen draws. A second route would have been a second poll
//! for one more fact about the same box, and the fields are additive: a device that predates them
//! reads the outbox exactly as before.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::time::Timestamp;

use crate::app::Edge;
use crate::http::error_response;
use crate::sntp::{ClockReading, drift_alarm_ms};
use crate::sync_status::{OUTBOX_PLANNED_DEPTH, SyncReport};

/// The cloud link and the outbox, as a device draws them.
#[derive(Debug, Serialize)]
pub(crate) struct SyncResponse {
    /// Events committed on this box and not yet acknowledged by the cloud.
    outbox_depth: u64,
    /// The depth the warnings are graded against. Not a limit: the store sells past it.
    outbox_planned_depth: u64,
    /// `OUTBOX_LEVEL_NORMAL`, `_ELEVATED` (half the plan), `_HIGH` (four fifths) or `_BEYOND`.
    outbox_level: &'static str,
    /// `CLOUD_LINK_ONLINE`, `CLOUD_LINK_OFFLINE`, or `CLOUD_LINK_UNSPECIFIED` when nothing has tried.
    cloud_link: &'static str,
    /// When the outbox drain last reached the cloud; absent if it has not since the edge started.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_sync_time: Option<Timestamp>,
    /// `CLOCK_DRIFT_OK`, `CLOCK_DRIFT_ALARM` past `clock_drift_alarm_ms`, or
    /// `CLOCK_DRIFT_UNSPECIFIED` when no probe has succeeded since the edge started.
    clock_drift: &'static str,
    /// The offset either way past which the clock alarms, in milliseconds.
    clock_drift_alarm_ms: u64,
    /// This box's clock minus the time server's, in milliseconds; absent until measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    clock_offset_ms: Option<i64>,
    /// When that was measured, on this box's own clock; absent until measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    clock_measure_time: Option<Timestamp>,
}

impl SyncResponse {
    fn new(report: SyncReport, clock: Option<ClockReading>) -> Self {
        Self {
            outbox_depth: report.outbox_depth,
            outbox_planned_depth: OUTBOX_PLANNED_DEPTH,
            outbox_level: report.outbox_level.as_wire(),
            cloud_link: report.cloud_link.as_wire(),
            last_sync_time: report.last_sync_time,
            clock_drift: clock.map_or("CLOCK_DRIFT_UNSPECIFIED", |reading| {
                reading.drift().as_wire()
            }),
            clock_drift_alarm_ms: drift_alarm_ms(),
            clock_offset_ms: clock.map(|reading| reading.offset_ms),
            clock_measure_time: clock.map(|reading| reading.measure_time),
        }
    }
}

/// `GET /api/sync` — the outbox depth, its level, the cloud link, and the box's clock.
pub(crate) async fn read<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    match edge.sync_report().await {
        Ok(report) => {
            Json(SyncResponse::new(report, edge.clock_status().reading())).into_response()
        }
        Err(error) => error_response(&error),
    }
}
