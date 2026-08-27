// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Background-task health seam ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 4).
//!
//! The cloud runs several background loops off the request path — the rollup projector, the
//! retention/PII sweep, the webhook dispatcher — and until now nothing recorded whether they were
//! still alive and keeping up. This seam is the smallest durable record that answers it: each loop
//! upserts one row at the end of every tick with the instant and a small self-describing detail, and
//! the `/admin` health route reads them. Staleness ("has this loop gone quiet?") is derived at read
//! time from `now − last_tick_at` against the interval the tick itself records — exactly as fleet
//! liveness derives online/offline — so nothing writes a "stalled" flag.
//!
//! A trait so it runs against an in-memory fake in tests and a `store-postgres` table in the cloud
//! (the impl lives in [`crate::persistence`], the SQL in `store-postgres`).

use core::future::Future;

use pos_proto::time::Timestamp;

/// The canonical name of the rollup projector loop.
pub const ROLLUP_PROJECTOR: &str = "rollup_projector";
/// The canonical name of the retention / PII-masking sweep loop.
pub const RETENTION: &str = "retention";
/// The canonical name of the webhook dispatcher loop.
pub const WEBHOOK_DISPATCHER: &str = "webhook_dispatcher";

/// Builds a tick's self-describing detail. Every loop records at least `ok` (did this tick's work
/// succeed) and `interval_secs` (its configured cadence, which the reader compares `now − last_tick`
/// against to judge staleness); `extra` carries any per-loop counts, merged in at the top level.
///
/// # Panics
///
/// Never in practice: `extra` is always constructed as a JSON object by the caller. A non-object
/// `extra` is ignored rather than panicking.
#[must_use]
pub fn tick_detail(ok: bool, interval_secs: u64, extra: serde_json::Value) -> serde_json::Value {
    let mut detail = serde_json::Map::new();
    detail.insert("ok".to_owned(), serde_json::Value::Bool(ok));
    detail.insert(
        "interval_secs".to_owned(),
        serde_json::Value::from(interval_secs),
    );
    if let serde_json::Value::Object(fields) = extra {
        for (key, value) in fields {
            detail.insert(key, value);
        }
    }
    serde_json::Value::Object(detail)
}

/// One background loop's recorded health: its name, the instant of its most recent tick, and the
/// tick's self-describing detail.
#[derive(Debug, Clone)]
pub struct TaskHealth {
    /// The stable loop name (one of the `*` consts in this module).
    pub task: String,
    /// The loop's most recent completed tick.
    pub last_tick_at: Timestamp,
    /// The tick's detail (`{"ok":bool,"interval_secs":N,…}`).
    pub detail: serde_json::Value,
}

/// Records and reads background-loop health.
pub trait TaskHealthStore {
    /// Records that `task` completed a tick at `at`, with `detail` describing it. Upserts, so the
    /// latest tick replaces the loop's prior row.
    ///
    /// # Errors
    ///
    /// [`TaskHealthError`] if the row could not be written.
    fn record_tick(
        &self,
        task: &str,
        at: Timestamp,
        detail: &serde_json::Value,
    ) -> impl Future<Output = Result<(), TaskHealthError>> + Send;

    /// Every recorded loop's health, most-recently-ticked first.
    ///
    /// # Errors
    ///
    /// [`TaskHealthError`] if the rows could not be read.
    fn list_health(&self) -> impl Future<Output = Result<Vec<TaskHealth>, TaskHealthError>> + Send;
}

/// A failure of the task-health store itself — the database is unreachable, or a stored detail could
/// not be decoded.
#[derive(Debug, thiserror::Error)]
#[error("the task-health store failed: {0}")]
pub struct TaskHealthError(String);

impl TaskHealthError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
