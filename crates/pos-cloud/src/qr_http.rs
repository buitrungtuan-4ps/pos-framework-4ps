// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The guest-facing QR ordering endpoint — `POST /v1/qr/orders`
//! ([ADR-0057](../../../docs/adr/0057-qr-ordering.md), [ADR-0012](../../../docs/adr/0012-qr-ordering-via-cloud.md)).
//!
//! A guest scanning a printed table code has no API key — the HMAC-signed table token *is* the
//! credential ([`crate::qr`]). This endpoint verifies that token, gathers the guardrail facts the
//! store's config owns, weighs them with the pure [`crate::qr::evaluate`], and on acceptance forwards
//! the submission into the very same [`OrderIn`] intake the keyed `POST /v1/orders` uses — the relay
//! in the binary, a fake in tests. Nothing here re-implements intake; it is a signed, guardrailed
//! on-ramp to it.
//!
//! # Where the facts come from
//!
//! - **token** — verified here, cryptographically binding tenant, store, and table.
//! - **store online, business hours, staff-confirmation default, per-table limit** — the store's
//!   effective config tree (a `qr` node, and `order_relay.enabled` for the store's participation),
//!   read the same forgiving way the relay reads its own config: an absent or malformed value falls
//!   back to a safe default rather than failing the order.
//! - **submissions in window** — an in-memory sliding-window counter keyed by `(store, table)`. The
//!   cloud is one process on one VPS (ADR-0003), so an in-process counter is the whole fleet's view;
//!   it resets on restart, which for an anti-abuse nuisance guard (never the security boundary — the
//!   signed token is) is an accepted trade rather than a durable-store slice.

use core::time::Duration;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde::Deserialize;

use pos_ports::order_in::{ExternalReference, InboundOrder, OrderIn};
use pos_proto::determinism::ClockSource;
use pos_proto::error::ErrorStatus;
use pos_proto::ids::{StoreId, TableId, TenantId};
use pos_proto::wire_enum::Open;
use pos_proto::{SalesChannel, Timestamp};

use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore};
use crate::http::api_error;
use crate::orders::{OrderLineRequest, intake_error, order_response, to_inbound_line};
use crate::qr::{QrDecision, QrFacts, QrRejection, TableTokenSecret, verify_table_token};

/// The default per-table submission ceiling in the rate-limit window, when config does not say.
const DEFAULT_PER_TABLE_LIMIT: u32 = 10;
/// The default rate-limit window, in seconds, when config does not say.
const DEFAULT_WINDOW_SECS: u64 = 60;

/// A guest's QR submission. The store, table, and tenant are **not** on the wire — they come from the
/// verified token, so a guest cannot name a table that is not theirs. The channel is always QR.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct QrOrderRequest {
    /// The HMAC-signed table token printed in the QR code.
    table_token: String,
    /// The guest session's idempotency reference (the app mints one per basket).
    external_reference: String,
    /// The requested lines, the same shape `/v1/orders` accepts.
    lines: Vec<OrderLineRequest>,
    /// When the guest placed it, epoch milliseconds.
    placed_at_ms: i64,
}

/// A store's optional business-hours window, in a fixed minute offset from UTC.
#[derive(Debug, Clone, Copy)]
struct BusinessHours {
    open_hour: u8,
    close_hour: u8,
    offset_minutes: i64,
}

/// The per-store QR guardrail settings, read from the store's effective config.
struct QrConfig {
    enabled: bool,
    store_online: bool,
    /// `None` means the store has configured no hours, i.e. always open.
    business_hours: Option<BusinessHours>,
    staff_confirmation_required: bool,
    per_table_limit: u32,
    window: Duration,
}

impl Default for QrConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            store_online: true,
            business_hours: None,
            // On by default — ADR-0057: a guest order waits for a member of staff unless the store
            // has explicitly turned that off.
            staff_confirmation_required: true,
            per_table_limit: DEFAULT_PER_TABLE_LIMIT,
            window: Duration::from_secs(DEFAULT_WINDOW_SECS),
        }
    }
}

impl QrConfig {
    /// Whether the store is open at `now_ms`. No configured window means always open.
    fn within_business_hours(&self, now_ms: i64) -> bool {
        self.business_hours.is_none_or(|hours| {
            is_open_at(
                current_hour(now_ms, hours.offset_minutes),
                hours.open_hour,
                hours.close_hour,
            )
        })
    }
}

/// An in-memory, per-`(store, table)` sliding-window counter of QR submissions.
#[derive(Debug, Default)]
pub(crate) struct TableRateLimiter {
    hits: Mutex<HashMap<(StoreId, TableId), Vec<i64>>>,
}

impl TableRateLimiter {
    /// A fresh, empty limiter.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// How many submissions this table has made within `window` ending at `now_ms`, pruning older
    /// entries as it counts.
    fn count_in_window(
        &self,
        store: StoreId,
        table: TableId,
        now_ms: i64,
        window: Duration,
    ) -> u32 {
        let cutoff = now_ms.saturating_sub(window.as_millis().try_into().unwrap_or(i64::MAX));
        let mut hits = self.hits.lock().expect("rate-limiter lock");
        let entry = hits.entry((store, table)).or_default();
        entry.retain(|&at| at >= cutoff);
        u32::try_from(entry.len()).unwrap_or(u32::MAX)
    }

    /// Records that this table submitted at `now_ms`.
    fn record(&self, store: StoreId, table: TableId, now_ms: i64) {
        let mut hits = self.hits.lock().expect("rate-limiter lock");
        hits.entry((store, table)).or_default().push(now_ms);
    }
}

/// The collaborators the QR endpoint composes: the signing secret, the [`OrderIn`] it forwards
/// accepted orders to, the config tree the guardrails read, a clock, and the in-memory rate limiter.
struct QrState<X, T, C> {
    secret: TableTokenSecret,
    intake: X,
    config_trees: T,
    clock: C,
    limiter: Arc<TableRateLimiter>,
}

impl<X: Clone, T: Clone, C: Clone> Clone for QrState<X, T, C> {
    fn clone(&self) -> Self {
        Self {
            secret: self.secret.clone(),
            intake: self.intake.clone(),
            config_trees: self.config_trees.clone(),
            clock: self.clock.clone(),
            limiter: self.limiter.clone(),
        }
    }
}

/// Builds the guest QR sub-router: `POST /v1/qr/orders`. Carries its own state and is merged into the
/// app router (the same shape the other sub-routers take), so the `CloudApp` generics do not grow.
pub fn qr_router<X, T, C>(secret: TableTokenSecret, intake: X, config_trees: T, clock: C) -> Router
where
    X: OrderIn + Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/v1/qr/orders", post(submit_qr_order::<X, T, C>))
        .with_state(QrState {
            secret,
            intake,
            config_trees,
            clock,
            limiter: Arc::new(TableRateLimiter::new()),
        })
}

/// `POST /v1/qr/orders` — verify the table token, weigh the guardrails, and on acceptance forward to
/// the intake as a QR-channel order for the token's table.
async fn submit_qr_order<X, T, C>(
    State(state): State<QrState<X, T, C>>,
    Json(request): Json<QrOrderRequest>,
) -> Response
where
    X: OrderIn + Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    // The token is the credential: an unverifiable one is refused before anything else is read, and
    // the reason is the same generic "untrusted table" the guardrail reports — no oracle for which of
    // malformed/forged/wrong-store it was.
    let Ok(table) = verify_table_token(&state.secret, &request.table_token) else {
        return rejection_response(QrRejection::UntrustedTable);
    };

    let config = qr_config_for(&state.config_trees, table.tenant_id, table.store_id).await;
    if !config.enabled {
        // A `404`, not a `403`: a guest scanning a code for a store that has QR ordering switched
        // off learns only that there is nothing here, which is what the store wants them to see.
        return api_error(
            ErrorStatus::NotFound,
            "QR ordering is not enabled for this store",
        );
    }

    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    let submissions_in_window =
        state
            .limiter
            .count_in_window(table.store_id, table.table_id, now_ms, config.window);

    let facts = QrFacts {
        token_valid: true,
        store_online: config.store_online,
        within_business_hours: config.within_business_hours(now_ms),
        submissions_in_window,
        per_table_limit: config.per_table_limit,
        staff_confirmation_required: config.staff_confirmation_required,
    };

    match crate::qr::evaluate(&facts) {
        QrDecision::Reject(reason) => rejection_response(reason),
        QrDecision::Accept { .. } => {
            let order = match to_inbound_qr_order(&request, table.store_id, table.table_id) {
                Ok(order) => order,
                Err(reason) => return (StatusCode::BAD_REQUEST, reason).into_response(),
            };
            // Count this submission only once it has passed every guardrail, so a rejected attempt
            // does not consume the table's budget.
            state.limiter.record(table.store_id, table.table_id, now_ms);
            match state.intake.submit(&order).await {
                Ok(acceptance) => order_response(&acceptance),
                Err(error) => intake_error(&error),
            }
        }
    }
}

/// Maps a QR request to an [`InboundOrder`] for the token's store and table, forced to the QR channel.
fn to_inbound_qr_order(
    request: &QrOrderRequest,
    store_id: StoreId,
    table_id: TableId,
) -> Result<InboundOrder, &'static str> {
    let external_reference = ExternalReference::parse(&request.external_reference)
        .map_err(|_ignored| "external_reference must be non-empty and at most 128 bytes")?;
    let placed_at = Timestamp::from_milliseconds_since_epoch(request.placed_at_ms)
        .map_err(|_ignored| "placed_at_ms is out of range")?;
    let mut lines = Vec::with_capacity(request.lines.len());
    for line in &request.lines {
        lines.push(to_inbound_line(line)?);
    }
    Ok(InboundOrder {
        external_reference,
        sales_channel: Open::from_known(SalesChannel::Qr),
        store_id,
        table_id: Some(table_id),
        subject_id: None,
        lines,
        placed_at,
    })
}

/// Reads the `qr` guardrail node (and `order_relay.enabled` for the store's participation) from a
/// store's effective config, tolerating any shape: an absent tree or field falls back to the default.
async fn qr_config_for<T: ConfigTreeStore>(
    config_trees: &T,
    tenant: TenantId,
    store_id: StoreId,
) -> QrConfig {
    let Ok(Some(state)) = config_trees
        .load(tenant, store_id)
        .await
        .map(crate::http::strip_tree_version)
    else {
        return QrConfig::default();
    };
    let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
    let Some(effective) = tree.current_effective() else {
        return QrConfig::default();
    };
    let default = QrConfig::default();
    let qr = effective.get("qr");
    let store_online = effective
        .get("order_relay")
        .and_then(|relay| relay.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(default.store_online);
    QrConfig {
        enabled: qr
            .and_then(|node| node.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default.enabled),
        store_online,
        business_hours: parse_business_hours(qr),
        staff_confirmation_required: qr
            .and_then(|node| node.get("staff_confirmation_required"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default.staff_confirmation_required),
        per_table_limit: qr
            .and_then(|node| node.get("per_table_limit"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(default.per_table_limit),
        window: qr
            .and_then(|node| node.get("rate_window_secs"))
            .and_then(serde_json::Value::as_u64)
            .map_or(default.window, Duration::from_secs),
    }
}

/// Parses an optional `qr.business_hours` object of `{ open_hour, close_hour, tz_offset_minutes }`.
/// Any missing or malformed hour yields `None` (always open) rather than refusing every order.
fn parse_business_hours(qr: Option<&serde_json::Value>) -> Option<BusinessHours> {
    let hours = qr?.get("business_hours")?;
    let open_hour = u8::try_from(hours.get("open_hour")?.as_u64()?).ok()?;
    let close_hour = u8::try_from(hours.get("close_hour")?.as_u64()?).ok()?;
    if open_hour > 23 || close_hour > 23 {
        return None;
    }
    let offset_minutes = hours
        .get("tz_offset_minutes")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    Some(BusinessHours {
        open_hour,
        close_hour,
        offset_minutes,
    })
}

/// The hour-of-day (0–23) at `now_ms` in a fixed minute offset from UTC.
fn current_hour(now_ms: i64, offset_minutes: i64) -> u8 {
    let local_ms = now_ms.saturating_add(offset_minutes.saturating_mul(60_000));
    let day_ms = local_ms.rem_euclid(86_400_000);
    u8::try_from(day_ms / 3_600_000).unwrap_or(0)
}

/// Whether `hour` falls in the `[open, close)` window, handling the overnight wrap. `open == close`
/// reads as always open.
fn is_open_at(hour: u8, open: u8, close: u8) -> bool {
    if open == close {
        return true;
    }
    if open < close {
        hour >= open && hour < close
    } else {
        hour >= open || hour < close
    }
}

/// Maps a [`QrRejection`] to the guest-facing envelope and reason.
///
/// One `match` for all four rejections, so the guest page has one shape to render whatever went
/// wrong. The messages stay guest-facing prose — a diner reads these, not an integrator — and the
/// `status` beside them is what the page branches on, which is the split the envelope exists to
/// make: the wording can be softened or translated without breaking the page.
///
/// No `details`: none of the four is about a field the guest filled in. An untrusted table code
/// carries no field-level reason on purpose — it is the one arm where naming what was wrong with
/// the code would help someone guessing at codes.
fn rejection_response(reason: QrRejection) -> Response {
    let (status, message) = match reason {
        QrRejection::UntrustedTable => (
            ErrorStatus::PermissionDenied,
            "the table code is not recognised",
        ),
        QrRejection::StoreOffline => (
            ErrorStatus::Unavailable,
            "the store is offline; please ask a member of staff",
        ),
        QrRejection::OutsideBusinessHours => (
            ErrorStatus::FailedPrecondition,
            "the store is closed right now",
        ),
        QrRejection::RateLimited => (
            ErrorStatus::ResourceExhausted,
            "too many orders from this table; please wait a moment",
        ),
    };
    api_error(status, message)
}

#[cfg(test)]
mod tests {
    use super::{TableRateLimiter, current_hour, is_open_at};
    use core::time::Duration;
    use pos_proto::ids::{StoreId, TableId};
    use pos_proto::ulid::Ulid;

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(1))
    }
    fn table() -> TableId {
        TableId::new(Ulid::from_u128(2))
    }

    #[test]
    fn a_same_day_window_is_open_only_within_it() {
        // 09:00–17:00
        assert!(!is_open_at(8, 9, 17));
        assert!(is_open_at(9, 9, 17));
        assert!(is_open_at(16, 9, 17));
        assert!(!is_open_at(17, 9, 17), "close is exclusive");
    }

    #[test]
    fn an_overnight_window_wraps_past_midnight() {
        // 18:00–02:00
        assert!(is_open_at(18, 18, 2));
        assert!(is_open_at(23, 18, 2));
        assert!(is_open_at(1, 18, 2));
        assert!(!is_open_at(2, 18, 2), "close is exclusive");
        assert!(!is_open_at(12, 18, 2));
    }

    #[test]
    fn equal_open_and_close_is_always_open() {
        for hour in 0..24 {
            assert!(is_open_at(hour, 0, 0));
        }
    }

    #[test]
    fn the_hour_is_computed_in_the_offset() {
        // 1970-01-01T00:00:00Z is hour 0 UTC; +7h offset makes it hour 7.
        assert_eq!(current_hour(0, 0), 0);
        assert_eq!(current_hour(0, 7 * 60), 7);
        // A negative offset wraps into the previous day.
        assert_eq!(current_hour(0, -60), 23);
    }

    #[test]
    fn the_rate_limiter_counts_within_the_window_and_prunes() {
        let limiter = TableRateLimiter::new();
        let window = Duration::from_secs(60);
        // Two hits at t=0 and t=30s.
        limiter.record(store(), table(), 0);
        limiter.record(store(), table(), 30_000);
        assert_eq!(
            limiter.count_in_window(store(), table(), 30_000, window),
            2,
            "both hits are within the 60s window"
        );
        // At t=90s the first hit (t=0) has aged out of the 60s window.
        assert_eq!(
            limiter.count_in_window(store(), table(), 90_000, window),
            1,
            "the first hit aged out"
        );
    }

    #[test]
    fn the_rate_limiter_is_per_table() {
        let limiter = TableRateLimiter::new();
        let window = Duration::from_secs(60);
        let other_table = TableId::new(Ulid::from_u128(3));
        limiter.record(store(), table(), 0);
        assert_eq!(limiter.count_in_window(store(), other_table, 0, window), 0);
        assert_eq!(limiter.count_in_window(store(), table(), 0, window), 1);
    }
}
