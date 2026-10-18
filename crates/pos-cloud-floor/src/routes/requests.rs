// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The queries and request bodies the floor and kitchen routes take, and the checks on a station's
//! fields.

use axum::response::Response;
use pos_cloud_api::response::api_error_with_details;
use pos_proto::ErrorStatus;
use pos_proto::display::GridPosition;
use pos_proto::printing::ReceiptLanguage;
use serde::Deserialize;

use pos_cloud_api::refusal::optional_choice;

/// The (tenant, store) a floor/kitchen list is scoped to — a floor is per-store, so both are required.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct FloorListQuery {
    pub(super) tenant_id: String,
    pub(super) store_id: String,
}

/// The tenant a single-record floor/kitchen read is scoped to (the id is globally unique).
#[derive(Debug, Clone, Deserialize)]
pub(super) struct FloorTenantQuery {
    pub(super) tenant_id: String,
}

/// Create a floor area.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateAreaRequest {
    pub(super) tenant_id: String,
    pub(super) store_id: String,
    pub(super) name: String,
}

/// Rename an area and/or set its status.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateAreaRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    pub(super) status: String,
}

/// Create a floor table.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateTableRequest {
    pub(super) tenant_id: String,
    pub(super) store_id: String,
    pub(super) area_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) seats: u16,
    #[serde(default)]
    pub(super) grid_column: Option<u16>,
    #[serde(default)]
    pub(super) grid_row: Option<u16>,
}

/// Update a table's area, label, seats, position, and status.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateTableRequest {
    pub(super) tenant_id: String,
    pub(super) area_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) seats: u16,
    #[serde(default)]
    pub(super) grid_column: Option<u16>,
    #[serde(default)]
    pub(super) grid_row: Option<u16>,
    pub(super) status: String,
}

/// Create a kitchen station.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateStationRequest {
    pub(super) tenant_id: String,
    pub(super) store_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) backup_station_id: Option<String>,
    #[serde(default)]
    pub(super) is_default: bool,
    /// When the station's tickets are late, in seconds; absent or `null` for the default. Read as
    /// any integer so that one out of bounds is refused naming the field, as one too large is.
    #[serde(default)]
    pub(super) late_after_seconds: Option<i64>,
    /// The language its kitchen tickets print in, as a `RECEIPT_LANGUAGE_…` token; absent or `null`
    /// for the store's display language.
    #[serde(default)]
    pub(super) ticket_language: Option<String>,
}

/// Update a station's name, backup, default flag, late threshold, ticket language, and status. Every
/// field is the station's whole new state, so an absent threshold or language is the default again.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateStationRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) backup_station_id: Option<String>,
    #[serde(default)]
    pub(super) is_default: bool,
    #[serde(default)]
    pub(super) late_after_seconds: Option<i64>,
    #[serde(default)]
    pub(super) ticket_language: Option<String>,
    pub(super) status: String,
}

/// A station's late threshold as a request names it: `None` for the default, or a number of seconds
/// within [`LATE_AFTER_SECONDS`](pos_proto::floor::LATE_AFTER_SECONDS).
///
/// # Errors
///
/// A `400` naming `late_after_seconds` for a threshold outside the bounds, which the edge would read
/// as ten minutes rather than as what the operator typed.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
pub(super) fn station_late_after(seconds: Option<i64>) -> Result<Option<u32>, Response> {
    let Some(seconds) = seconds else {
        return Ok(None);
    };
    u32::try_from(seconds)
        .ok()
        .filter(|seconds| pos_proto::floor::LATE_AFTER_SECONDS.contains(seconds))
        .map(Some)
        .ok_or_else(|| {
            api_error_with_details(
                ErrorStatus::InvalidArgument,
                "late_after_seconds must be from 60 to 3600",
                &[("late_after_seconds", "OUT_OF_RANGE")],
            )
        })
}

/// A station's ticket language as a request names it: `None` for the store's display language, or
/// one of the [`ReceiptLanguage`] tokens a receipt's language takes.
///
/// # Errors
///
/// A `400 INVALID_ENUM_VALUE` naming `ticket_language` for a token this release does not know, and
/// for `RECEIPT_LANGUAGE_UNSPECIFIED`, which is what an older reader takes a newer language to be
/// and never a choice.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
pub(super) fn station_ticket_language(
    token: Option<&str>,
) -> Result<Option<ReceiptLanguage>, Response> {
    optional_choice("ticket_language", token)
}

/// Create an item→station routing rule.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateRoutingRuleRequest {
    pub(super) tenant_id: String,
    pub(super) store_id: String,
    pub(super) station_id: String,
    #[serde(default)]
    pub(super) menu_item_id: Option<String>,
    #[serde(default)]
    pub(super) course_id: Option<String>,
    #[serde(default)]
    pub(super) sort: u16,
}

/// Folds two optional grid coordinates into a [`GridPosition`] — a table is placed only when both are
/// given; either alone is treated as unplaced.
pub(super) fn grid_position(column: Option<u16>, row: Option<u16>) -> Option<GridPosition> {
    match (column, row) {
        (Some(column), Some(row)) => Some(GridPosition { column, row }),
        _ => None,
    }
}
