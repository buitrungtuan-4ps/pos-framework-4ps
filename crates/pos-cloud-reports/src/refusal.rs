// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The refusals a rollup read answers with.

use axum::response::Response;

use pos_cloud_api::response::{api_error, api_error_with_details};
use pos_proto::ErrorStatus;

use crate::dashboard::RollupError;
use crate::dashboard::rollup::WindowError;

/// The `400` a malformed rollup window earns, naming the query parameter at fault.
///
/// The mapping lives here rather than in `dashboard::rollup` because only the route knows the wire
/// names. `Inverted` carries **no** `details`: neither `from` nor `to` is wrong on its own, and the
/// standing rule is to name only the field that actually was — attributing it to `from` would send
/// a caller who mistyped `to` to the wrong input. The message already names both.
pub fn window_refusal(error: WindowError) -> Response {
    match error {
        WindowError::MalformedFrom => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "from must be a YYYY-MM-DD business date",
            &[("from", "INVALID_FORMAT")],
        ),
        WindowError::MalformedTo => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "to must be a YYYY-MM-DD business date",
            &[("to", "INVALID_FORMAT")],
        ),
        WindowError::Inverted => {
            api_error(ErrorStatus::InvalidArgument, "from must not be after to")
        }
        WindowError::LimitTooSmall => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "limit must be at least 1",
            &[("limit", "OUT_OF_RANGE")],
        ),
    }
}

/// Maps a rollup-read failure to a `503`, logging the detail rather than returning it — a dashboard
/// read only fails when the store itself is unreachable, which is transient and the caller's cue to
/// retry, and the internal reason is not the client's business.
pub fn rollup_error_response(error: &RollupError) -> Response {
    tracing::error!(%error, "a dashboard rollup read failed");
    api_error(
        ErrorStatus::Unavailable,
        "the dashboard is temporarily unavailable",
    )
}
