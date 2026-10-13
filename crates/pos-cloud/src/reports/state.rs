// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The part of the cloud's state the reporting routes read.

use core::fmt;
use std::sync::Arc;

use crate::audit::AuditRecorder;

/// What a reporting route reads: the rollup store, the API keys and the console sessions that
/// authorise it, the clock, the audit trail an export writes to, and the till names the X/Z report
/// shows.
///
/// The router's state is the whole application, which composes every store the cloud runs. A
/// report handler takes this instead, built from that state by axum's `FromRef` (implemented beside
/// the application state), so a handler's signature names only what it reads.
#[derive(Clone)]
pub(crate) struct ReportsState<R, K, C, A, T> {
    /// The materialised rollups.
    pub(crate) rollups: R,
    /// The API keys a `/v1` read authenticates with.
    pub(crate) keys: K,
    /// The clock sessions and audit entries are timed by.
    pub(crate) clock: C,
    /// The console's admins and sessions.
    pub(crate) admin: A,
    /// Where an export is recorded.
    pub(crate) audit: Arc<dyn AuditRecorder>,
    /// The names the stores' configuration gives their tills.
    pub(crate) till_names: T,
}

impl<R, K, C, A, T> fmt::Debug for ReportsState<R, K, C, A, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReportsState")
            .finish_non_exhaustive()
    }
}
