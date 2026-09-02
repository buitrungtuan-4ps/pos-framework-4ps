// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One error shape, everywhere.
//!
//! `docs/naming-and-api.md` §4 fixes it (AIP-193):
//!
//! ```json
//! { "error": { "code": 400, "status": "INVALID_ARGUMENT",
//!              "message": "price_amount_minor must be positive",
//!              "details": [ { "field": "price_amount_minor",
//!                             "reason": "MUST_BE_POSITIVE" } ] } }
//! ```
//!
//! One shape means a client writes one error path rather than one per endpoint, and a
//! store's status bar can render any failure without knowing what produced it.
//!
//! # Two deliberate deviations, and why they are safe
//!
//! **`UNSPECIFIED`.** The canonical AIP statuses carry no prefix and AIP defines no
//! `*_UNSPECIFIED` member, which sits awkwardly beside the naming standard's rule that
//! every enum has one. [`ErrorStatus`] resolves it by implementing
//! [`WireEnum`](crate::wire_enum::WireEnum) by hand, with an `UNSPECIFIED` token that
//! AIP does not list. Nothing emits it; it exists so that a status added by a newer
//! server degrades in an older client instead of failing the whole response parse —
//! which for an *error* response would replace a useful message with a parse error, the
//! worst possible moment to lose information.
//!
//! **`VERSION_MISMATCH`.** A conditional write whose `If-Match` does not match owes
//! `412`, which RFC 9110 fixes and no canonical status maps to
//! ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)). It is
//! deliberately not named `PRECONDITION_FAILED`: beside the existing
//! [`FailedPrecondition`](ErrorStatus::FailedPrecondition) (`409`) that would be two
//! tokens differing only in word order with different HTTP codes, which is a defect
//! waiting to be written by someone reading quickly. It is safe for the same reason
//! `UNSPECIFIED` is: a client built before it reads it as unrecognised and still gets an
//! intact `code`, `message` and `details`.

use serde::{Deserialize, Serialize};

use crate::wire_enum::{Open, WireEnum};

/// The canonical statuses, as `docs/naming-and-api.md` §4 lists them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum ErrorStatus {
    /// Absent, or a status this build does not recognise. Never emitted.
    #[default]
    Unspecified,
    /// The request was malformed or a value was out of range.
    InvalidArgument,
    /// The resource does not exist.
    NotFound,
    /// The resource already exists.
    AlreadyExists,
    /// The caller is known but not allowed.
    PermissionDenied,
    /// The caller is not identified.
    Unauthenticated,
    /// The system is in the wrong state for this request.
    ///
    /// What a second `bill:settle` returns: settlement is a one-time transition
    /// (`docs/pos-spec.md` §14.4).
    FailedPrecondition,
    /// A quota or rate limit was reached.
    ResourceExhausted,
    /// A dependency is unreachable. Retryable.
    Unavailable,
    /// Something broke on our side.
    Internal,
    /// The caller's `If-Match` names a version the resource no longer holds
    /// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)).
    ///
    /// Not retryable as sent: the same stale token would fail again. The caller re-reads,
    /// shows the reader what changed, and writes against the version it got back.
    VersionMismatch,
}

impl WireEnum for ErrorStatus {
    const UNSPECIFIED: Self = Self::Unspecified;

    const ALL: &'static [Self] = &[
        Self::Unspecified,
        Self::InvalidArgument,
        Self::NotFound,
        Self::AlreadyExists,
        Self::PermissionDenied,
        Self::Unauthenticated,
        Self::FailedPrecondition,
        Self::ResourceExhausted,
        Self::Unavailable,
        Self::Internal,
        Self::VersionMismatch,
    ];

    fn as_wire(self) -> &'static str {
        match self {
            Self::Unspecified => "UNSPECIFIED",
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::NotFound => "NOT_FOUND",
            Self::AlreadyExists => "ALREADY_EXISTS",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::Unauthenticated => "UNAUTHENTICATED",
            Self::FailedPrecondition => "FAILED_PRECONDITION",
            Self::ResourceExhausted => "RESOURCE_EXHAUSTED",
            Self::Unavailable => "UNAVAILABLE",
            Self::Internal => "INTERNAL",
            Self::VersionMismatch => "VERSION_MISMATCH",
        }
    }

    fn from_wire(token: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_wire() == token)
    }
}

impl ErrorStatus {
    /// The HTTP status code to send alongside.
    ///
    /// Kept next to the status rather than in the transport layer, so the mapping is
    /// stated once and cannot differ between endpoints.
    #[must_use]
    pub const fn http_code(self) -> u16 {
        match self {
            Self::InvalidArgument => 400,
            Self::Unauthenticated => 401,
            Self::PermissionDenied => 403,
            Self::NotFound => 404,
            Self::AlreadyExists | Self::FailedPrecondition => 409,
            Self::VersionMismatch => 412,
            Self::ResourceExhausted => 429,
            // An unrecognised status is a server-side problem by elimination.
            Self::Unspecified | Self::Internal => 500,
            Self::Unavailable => 503,
        }
    }

    /// Whether a caller may reasonably retry.
    ///
    /// The edge uses this to decide between backing off and surfacing: a retryable
    /// failure must never block a sale, it must leave the event in the outbox.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Unavailable | Self::ResourceExhausted)
    }
}

impl core::fmt::Display for ErrorStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// One reason a request failed, tied to the field responsible.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ErrorDetail {
    /// The offending field, in the same `snake_case` name the request used.
    pub field: String,
    /// A stable, machine-readable reason such as `MUST_BE_POSITIVE`.
    ///
    /// Stable so a client can branch on it; the `message` is for people and may be
    /// reworded at any time.
    pub reason: String,
}

/// The error body.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ErrorBody {
    /// The HTTP status code, repeated here so a body is self-describing once logged.
    pub code: u16,
    /// The canonical status.
    pub status: Open<ErrorStatus>,
    /// A human-readable explanation.
    ///
    /// Never shown raw to restaurant staff — `docs/ui-ux.md` §1.7 requires an exit
    /// rather than an error code — and never containing personal data, because
    /// responses are logged.
    pub message: String,
    /// Field-level detail. Empty when the failure is not about a particular field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<ErrorDetail>,
}

/// The envelope wrapping [`ErrorBody`], so a response is unambiguous at the top level.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ErrorResponse {
    /// The error.
    pub error: ErrorBody,
}

impl ErrorResponse {
    /// Builds a response, deriving the HTTP code from the status so the two cannot
    /// disagree.
    #[must_use]
    pub fn new(status: ErrorStatus, message: impl Into<String>) -> Self {
        Self {
            error: ErrorBody {
                code: status.http_code(),
                status: Open::from_known(status),
                message: message.into(),
                details: Vec::new(),
            },
        }
    }

    /// Adds field-level detail.
    #[must_use]
    pub fn with_detail(mut self, field: impl Into<String>, reason: impl Into<String>) -> Self {
        self.error.details.push(ErrorDetail {
            field: field.into(),
            reason: reason.into(),
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorResponse, ErrorStatus};
    use crate::wire_enum::{Open, WireEnum};

    #[test]
    fn the_documented_example_serialises_exactly() {
        let response = ErrorResponse::new(
            ErrorStatus::InvalidArgument,
            "price_amount_minor must be positive",
        )
        .with_detail("price_amount_minor", "MUST_BE_POSITIVE");
        let json = serde_json::to_string(&response).expect("serialise");
        assert_eq!(
            json,
            r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"price_amount_minor must be positive","details":[{"field":"price_amount_minor","reason":"MUST_BE_POSITIVE"}]}}"#
        );
    }

    #[test]
    fn every_canonical_status_round_trips() {
        for status in ErrorStatus::ALL {
            assert_eq!(ErrorStatus::from_wire(status.as_wire()), Some(*status));
        }
    }

    #[test]
    fn the_nine_canonical_statuses_are_all_present() {
        // `docs/naming-and-api.md` §4 lists exactly these.
        for token in [
            "INVALID_ARGUMENT",
            "NOT_FOUND",
            "ALREADY_EXISTS",
            "PERMISSION_DENIED",
            "UNAUTHENTICATED",
            "FAILED_PRECONDITION",
            "RESOURCE_EXHAUSTED",
            "UNAVAILABLE",
            "INTERNAL",
        ] {
            assert!(
                ErrorStatus::from_wire(token).is_some(),
                "{token} is missing from the canonical set"
            );
        }
        assert_eq!(
            ErrorStatus::ALL.len(),
            11,
            "nine canonical statuses plus the two documented deviations, UNSPECIFIED and \
             VERSION_MISMATCH"
        );
    }

    #[test]
    fn a_stale_conditional_write_is_the_only_412_and_is_not_the_409() {
        // The two are a homonym pair by construction — `FAILED_PRECONDITION` and
        // `VERSION_MISMATCH` both describe a precondition that did not hold — so the thing
        // worth pinning is that they never collapse into one answer. A second settlement
        // owes 409; a write against a version the row no longer holds owes 412, which is
        // what RFC 9110 requires of a failed `If-Match`.
        assert_eq!(ErrorStatus::VersionMismatch.http_code(), 412);
        assert_eq!(ErrorStatus::FailedPrecondition.http_code(), 409);
        assert_eq!(ErrorStatus::VersionMismatch.as_wire(), "VERSION_MISMATCH");
        assert_eq!(
            ErrorStatus::from_wire("PRECONDITION_FAILED"),
            None,
            "the word-order twin must not exist: two tokens differing only in word order, \
             with different HTTP codes, is a defect waiting to be written"
        );
    }

    #[test]
    fn a_stale_conditional_write_does_not_invite_a_retry() {
        // Retrying with the same `If-Match` fails identically. The caller has to re-read
        // first, so telling it to back off and try again would be telling it to spin.
        assert!(!ErrorStatus::VersionMismatch.is_retryable());
    }

    #[test]
    fn http_codes_match_the_statuses() {
        assert_eq!(ErrorStatus::InvalidArgument.http_code(), 400);
        assert_eq!(ErrorStatus::Unauthenticated.http_code(), 401);
        assert_eq!(ErrorStatus::PermissionDenied.http_code(), 403);
        assert_eq!(ErrorStatus::NotFound.http_code(), 404);
        assert_eq!(ErrorStatus::AlreadyExists.http_code(), 409);
        assert_eq!(ErrorStatus::FailedPrecondition.http_code(), 409);
        assert_eq!(ErrorStatus::ResourceExhausted.http_code(), 429);
        assert_eq!(ErrorStatus::Internal.http_code(), 500);
        assert_eq!(ErrorStatus::Unavailable.http_code(), 503);
    }

    #[test]
    fn a_second_settlement_is_a_failed_precondition_and_not_retryable() {
        // Settlement is a one-time transition. Retrying it would be trying to charge a
        // guest twice, so the status must not invite a retry.
        assert!(!ErrorStatus::FailedPrecondition.is_retryable());
        assert!(ErrorStatus::Unavailable.is_retryable());
        assert!(ErrorStatus::ResourceExhausted.is_retryable());
    }

    #[test]
    fn a_status_from_a_newer_server_does_not_break_the_parse() {
        // Losing an error body to a parse failure is the worst moment to lose
        // information, so tolerance matters more here than almost anywhere.
        let json = r#"{"error":{"code":418,"status":"TEAPOT","message":"short and stout"}}"#;
        let response: ErrorResponse = serde_json::from_str(json).expect("parses");
        assert!(response.error.status.is_unrecognised());
        assert_eq!(response.error.message, "short and stout");
        assert_eq!(response.error.status.as_wire(), "TEAPOT");
    }

    #[test]
    fn details_are_omitted_when_empty_rather_than_sent_as_an_empty_array() {
        let json =
            serde_json::to_string(&ErrorResponse::new(ErrorStatus::NotFound, "no such order"))
                .expect("serialise");
        assert!(!json.contains("details"), "got {json}");
    }

    #[test]
    fn an_unspecified_status_is_never_what_a_server_emits() {
        // It exists only so an older client can read a newer server's response.
        let built = ErrorResponse::new(ErrorStatus::Internal, "boom");
        assert_eq!(built.error.status, Open::from_known(ErrorStatus::Internal));
        assert!(!built.error.status.is_unspecified());
    }
}
