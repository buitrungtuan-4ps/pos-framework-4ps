// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The error envelope as the console API document describes it.

/// One reason a request failed, tied to the field responsible — the schema mirror of
/// [`pos_proto::error::ErrorDetail`].
#[derive(utoipa::ToSchema)]
#[expect(
    dead_code,
    reason = "a schema-only mirror: utoipa reads the field names and types to build the document, \
              and nothing constructs or reads an instance"
)]
pub struct ErrorDetail {
    /// The offending field, in the same `snake_case` name the request used. For a whole-request
    /// fault this can be a header name (`if-match`) rather than a body field.
    pub(crate) field: String,
    /// A stable, machine-readable reason such as `MUST_BE_POSITIVE` or `INVALID_ENUM_VALUE`. Stable
    /// so a client can branch on it; `message` is for people and may be reworded at any time.
    pub(crate) reason: String,
}

/// The error body — the schema mirror of [`pos_proto::error::ErrorBody`].
#[derive(utoipa::ToSchema)]
#[expect(dead_code, reason = "a schema-only mirror, as `ErrorDetail` explains")]
pub struct ErrorBody {
    /// The HTTP status code, repeated here so a body is self-describing once logged.
    pub(crate) code: u16,
    /// The canonical status, e.g. `INVALID_ARGUMENT`, `NOT_FOUND`, `FAILED_PRECONDITION`.
    pub(crate) status: String,
    /// A human-readable explanation. Never contains personal data, because responses are logged.
    pub(crate) message: String,
    /// Field-level detail. Absent when the failure is not about a particular field.
    pub(crate) details: Vec<ErrorDetail>,
}

/// The envelope every `/admin` failure carries — the schema mirror of
/// [`pos_proto::error::ErrorResponse`].
///
/// One level of nesting so a response is unambiguous at the top level: a success body is the record,
/// a failure body is `{"error": …}`.
#[derive(utoipa::ToSchema)]
#[expect(dead_code, reason = "a schema-only mirror, as `ErrorDetail` explains")]
pub struct ErrorResponse {
    /// The error.
    pub(crate) error: ErrorBody,
}
