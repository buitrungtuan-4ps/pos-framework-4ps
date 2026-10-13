// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The responses every cloud route sends when it refuses: the one AIP-193 error envelope, with its
//! status derived from [`ErrorStatus`], and the CSV download a report or export answers with.

use axum::Json;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};

use pos_proto::ulid::Ulid;
use pos_proto::{ErrorResponse, ErrorStatus};

/// A CSV download response ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md), Track M5): the
/// bytes with `text/csv` and a `content-disposition` naming the file so the browser saves it. `filename`
/// is a fixed, server-chosen literal per domain — never tenant-supplied — so it needs no escaping.
pub fn csv_download_response(filename: &str, body: Vec<u8>) -> Response {
    (
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

/// Every error this surface sends, in the one AIP-193 shape
/// [`pos_proto::error`](../../../pos-proto/src/error.rs) defines
/// ([ADR-0026](../../../docs/adr/0026-cloud-http-surface.md) §27, `docs/naming-and-api.md` §4).
///
/// The HTTP status is **not** a parameter: it is derived from `status` through
/// [`ErrorStatus::http_code`], which is where the mapping is stated once. A caller that could pick
/// its own code could pick one that disagrees with the body it is sending, and a client branching on
/// either would then be reading a different error from the one the server meant.
///
/// Field-level detail has no caller yet — the inline validation sites that will carry it are the
/// next slice — so it is not a parameter either. When it arrives, `ErrorResponse::with_detail` is
/// already fluent and the door is a second constructor, not a widened signature.
pub fn api_error(status: ErrorStatus, message: impl Into<String>) -> Response {
    (
        http_status(status),
        Json(ErrorResponse::new(status, message)),
    )
        .into_response()
}

/// The same envelope, naming **which** field was wrong and why
/// (`docs/naming-and-api.md` §4).
///
/// This is the door [`api_error`] deliberately left open rather than widening its own signature: a
/// refusal that is not about a particular field cannot grow an empty `details` array by accident,
/// and a caller cannot pass one where none belongs.
///
/// `reason` is a stable `SCREAMING_SNAKE` token; `message` is prose for a person. They are separate
/// because they change at different rates — the message can be reworded or translated freely, while
/// the reason is what a client branches on, and wording a client depends on is wording that can no
/// longer be improved.
pub fn api_error_with_details(
    status: ErrorStatus,
    message: impl Into<String>,
    details: &[(&str, &str)],
) -> Response {
    let body = details.iter().fold(
        ErrorResponse::new(status, message),
        |body, (field, reason)| body.with_detail(*field, *reason),
    );
    (http_status(status), Json(body)).into_response()
}

/// Parses `N` named ULID fields at once, refusing with a detail per field that **actually** failed.
///
/// Half of this file's refusals were this one sentence written by hand, and writing it by hand let
/// it drift three ways at once: 63 different phrasings of "not a ULID"; no `details` array, so a
/// console could not mark the offending input; and — the one that misinforms a caller — a tuple
/// parse whose message named *every* field it had looked at (`"tenant_id or store_id is not a
/// ULID"`) instead of the one that was wrong. About 120 sites did that. A caller reading it has to
/// guess which half of its request to fix.
///
/// Taking the fields as an argument is what makes that ambiguity *unwriteable* rather than merely
/// fixed here: there is no signature in this module that accepts a message about fields it did not
/// check, so the next id parse cannot reintroduce the shape.
///
/// Returning an array rather than a slice is what keeps the ids typed at the call site — a caller
/// destructures positionally, `Ok([tenant, store]) => (TenantId::new(tenant), StoreId::new(store))`,
/// so nothing is indexed and the newtype stays where it belongs, in the handler.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub fn parse_ulid_fields<const N: usize>(fields: [(&str, &str); N]) -> Result<[Ulid; N], Response> {
    let mut bad: Vec<&str> = Vec::new();
    // The placeholder for a field that did not parse never escapes: `bad` is non-empty in exactly
    // that case, and a non-empty `bad` returns the refusal instead of the array.
    let parsed = fields.map(|(field, raw)| match raw.parse::<Ulid>() {
        Ok(id) => id,
        Err(_ignored) => {
            bad.push(field);
            Ulid::NIL
        }
    });
    if bad.is_empty() {
        Ok(parsed)
    } else {
        Err(ulid_refusal(&bad))
    }
}

/// The refusal [`parse_ulid_fields`] returns: `INVALID_ARGUMENT`, a `NOT_A_ULID` detail per failed
/// field, and prose naming those fields in the order the caller listed them.
///
/// A single bad field reads `"tenant_id is not a ULID"` — the wording 51 of these sites already
/// used — so for the common case only the `details` array is new, and no client that was reading
/// the message sees it change.
pub fn ulid_refusal(bad: &[&str]) -> Response {
    let details: Vec<(&str, &str)> = bad.iter().map(|field| (*field, "NOT_A_ULID")).collect();
    let message = match bad {
        [field] => format!("{field} is not a ULID"),
        [head @ .., last] => format!("{} and {last} are not ULIDs", head.join(", ")),
        // Unreachable: the only caller refuses to build a refusal with nothing to refuse.
        [] => "a field is not a ULID".to_owned(),
    };
    api_error_with_details(ErrorStatus::InvalidArgument, message, &details)
}

/// The refusal for a dependency that is down: `SERVICE_UNAVAILABLE`, and no details.
///
/// `ErrorStatus::Unavailable`, not `InvalidArgument`, which is the distinction deriving the code
/// from [`ErrorStatus::http_code`] exists to keep: this refusal is **retryable** and the caller's
/// request was fine. A client that reads it as its own fault stops retrying something that would
/// have succeeded.
pub fn service_unavailable(service: &str) -> Response {
    api_error(
        ErrorStatus::Unavailable,
        format!("the {service} service is unavailable"),
    )
}

/// The `axum` status code for an [`ErrorStatus`], over `pos-proto`'s authoritative map.
///
/// The fallback is unreachable — every code [`ErrorStatus::http_code`] returns is a valid status,
/// and `every_status_maps_to_a_valid_code` holds it to that — but an unmappable code would be a
/// server-side fault by elimination, which is what `500` says.
pub fn http_status(status: ErrorStatus) -> StatusCode {
    StatusCode::from_u16(status.http_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}
