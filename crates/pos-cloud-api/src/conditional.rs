// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Conditional writes on the console surface
//! ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md),
//! [ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)): reading `If-Match`,
//! the refusals for a stale or duplicate write, and the responses that carry a record's version.
//!
//! Moved out of `pos-cloud`'s `http.rs` (ADR-0170 decision 4), beside [`crate::version`].
//! `pos-cloud`'s `http` module imports them.

use axum::Json;
use axum::http::header::{ETAG, IF_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_proto::ErrorStatus;
use serde::Serialize;

use crate::response::{api_error, api_error_with_details};
use crate::version::{Version, Versioned};

/// What was wrong with an `If-Match` value that is present but unusable.
///
/// Separate from the refusal it becomes so the parse can be tested as a pure function: the exact
/// strings a client branches on are worth asserting, and a test that only reads status codes would
/// pass on any of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntityTagRefusal {
    /// `If-Match: *` — legal HTTP, and refused here. See [`if_match`].
    Wildcard,
    /// Not one strong entity-tag: a weak `W/"…"`, a comma-separated list, or unquoted.
    Malformed,
}

/// Reads one strong entity-tag out of a raw header value.
///
/// **Strong only.** RFC 9110 §13.1.1 evaluates `If-Match` with the strong comparison function,
/// under which a weak validator never matches — so accepting `W/"…"` would accept a token that can
/// only ever fail, which is worse than refusing it, because the caller would read the resulting
/// `412` as a real conflict and go looking for an edit nobody made.
///
/// **One tag, not a list.** A list is legal HTTP and means "any of these"; here the version is a
/// single row's, so a list has no meaning a caller could intend. The inner-quote check is what
/// rejects it: `"a", "b"` unwraps to `a", "b`, which still contains a quote.
pub fn parse_entity_tag(raw: &str) -> Result<Version, EntityTagRefusal> {
    let trimmed = raw.trim();
    if trimmed == "*" {
        return Err(EntityTagRefusal::Wildcard);
    }
    let inner = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or(EntityTagRefusal::Malformed)?;
    if inner.contains('"') {
        return Err(EntityTagRefusal::Malformed);
    }
    Ok(Version::new(inner))
}

/// The version a mutating route is being asked to replace
/// ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)).
///
/// **Required, not optional.** Treating an absent header as "no opinion" would leave the silent
/// clobber this exists to close, and leave it as the *default* — every caller that had not been
/// updated would keep overwriting. Absent is therefore an ordinary missing-field refusal, in the
/// shape Q3b gave every other one, rather than a status of its own.
///
/// `If-Match: *` is refused too. It is legal HTTP meaning "whatever is there now", which is
/// precisely last-write-wins under a different name, and no caller in this tree wants it.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub fn if_match(headers: &HeaderMap) -> Result<Version, Response> {
    let Some(raw) = headers.get(IF_MATCH) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match is required: send the etag the record was read at",
            &[("if-match", "REQUIRED")],
        ));
    };
    let Ok(text) = raw.to_str() else {
        return Err(entity_tag_refusal(EntityTagRefusal::Malformed));
    };
    parse_entity_tag(text).map_err(entity_tag_refusal)
}

/// The `If-Match` a **collection** write must carry
/// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
///
/// Same requirement as [`if_match`] and one difference: a wildcard is accepted, and means something
/// the record routes have no use for. A record is created by a `POST` that names no version; a
/// collection is not created at all — the tenant's tax table and translation grid are always
/// *there*, conceptually, and the only question is whether anything has been saved into them yet.
/// `If-Match: *` is how a caller says "nothing has", and `Ok(None)` carries that down to the store,
/// which is the only party that can tell whether it is still true. It is an assertion, not a waiver:
/// a store that has been saved to refuses it.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
pub fn if_match_collection(headers: &HeaderMap) -> Result<Option<Version>, Response> {
    let Some(raw) = headers.get(IF_MATCH) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match is required: send the etag the collection was read at, or * if it has never been saved",
            &[("if-match", "REQUIRED")],
        ));
    };
    let Ok(text) = raw.to_str() else {
        return Err(entity_tag_refusal(EntityTagRefusal::Malformed));
    };
    match parse_entity_tag(text) {
        Ok(version) => Ok(Some(version)),
        Err(EntityTagRefusal::Wildcard) => Ok(None),
        Err(refusal @ EntityTagRefusal::Malformed) => Err(entity_tag_refusal(refusal)),
    }
}

/// The refusal for an `If-Match` this surface will not act on.
pub fn entity_tag_refusal(refusal: EntityTagRefusal) -> Response {
    match refusal {
        EntityTagRefusal::Wildcard => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must name a version, not *",
            &[("if-match", "WILDCARD_NOT_ACCEPTED")],
        ),
        EntityTagRefusal::Malformed => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must be one strong entity-tag, in double quotes",
            &[("if-match", "INVALID_FORMAT")],
        ),
    }
}

/// The refusal for a write against a version the record no longer holds: `412`, and no details.
///
/// No `details` array, for the same reason absence and outage carry none: the caller's fields were
/// all fine. What went stale is the caller's *copy*, not any one input, so naming a field would
/// send it to fix something that was right.
pub fn version_mismatch() -> Response {
    api_error(
        ErrorStatus::VersionMismatch,
        "the record changed since you read it: re-read it and try again",
    )
}

/// The `409` a create earns when the key it asked for is taken.
///
/// Names the entity, because the caller's next move depends on it: edit the one that is there, or
/// pick a different key. Before [ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)
/// split the six keyed upserts, this answer did not exist — an `upsert` overwrote instead, and the
/// only way a duplicate could surface was as the store's own `503`, which tells a caller the server
/// is broken and invites the retry that can never succeed.
pub fn already_exists(entity: &str) -> Response {
    api_error(
        ErrorStatus::AlreadyExists,
        format!("that {entity} already exists: edit it instead of creating another"),
    )
}

/// A freshly created record and the version it starts at, as `201`.
///
/// A create answers with a version for the same reason a read does: without one the caller has to
/// list the collection again before it can edit what it just made, which is a round trip to learn
/// something the server already knew. RFC 9110 provides for exactly this on `201`.
pub fn versioned_created<T>(record: T, version: &Version) -> Response
where
    T: Serialize,
{
    let body = Json(Versioned::new(record, version.clone()));
    match HeaderValue::from_str(&format!("\"{version}\"")) {
        Ok(tag) => (StatusCode::CREATED, [(ETAG, tag)], body).into_response(),
        Err(_ignored) => (StatusCode::CREATED, body).into_response(),
    }
}

/// Stamps the version a write left the record at onto a response, without changing its shape.
///
/// Five entities — areas, tables, stations, employees, role templates — answer a write with `204`
/// or a bare `{"id": …}`, not the record. That is not an oversight to correct here: their update
/// payloads do not carry every field (an `AreaUpdate` has no `store_id`), so a returned
/// "representation" would be one this handler had to invent. The header is the whole contract for
/// them — RFC 9110 §8.8.3 puts `ETag` on exactly this response — so a caller that wants the record
/// re-reads it, and a caller that wants to keep writing already holds the token it needs.
///
/// A token that cannot be a header value is the adapter's fault, not the caller's, and the write
/// itself still succeeded; the response goes out unstamped rather than turning a completed write
/// into a `500`.
pub fn with_etag(mut response: Response, version: &Version) -> Response {
    if let Ok(tag) = HeaderValue::from_str(&format!("\"{version}\"")) {
        response.headers_mut().insert(ETAG, tag);
    }
    response
}

/// A record and the version it is now at, as `200`.
///
/// Carries the version **twice**, deliberately: as the `ETag` header RFC 9110 defines for a single
/// resource, and as the `etag` field a list row has to use because one header cannot describe many
/// rows. Byte-identical either way, so a client has one code path for "remember this version" and
/// no opportunity to reformat a token it must not parse.
pub fn versioned_ok<T>(record: T, version: &Version) -> Response
where
    T: Serialize,
{
    let Ok(tag) = HeaderValue::from_str(&format!("\"{version}\"")) else {
        // Unreachable through the adapters in this tree, which mint digits. A token that cannot be
        // a header value is the adapter's fault, not the caller's, and the record is still correct.
        return (
            StatusCode::OK,
            Json(Versioned::new(record, version.clone())),
        )
            .into_response();
    };
    (
        StatusCode::OK,
        [(ETAG, tag)],
        Json(Versioned::new(record, version.clone())),
    )
        .into_response()
}

#[cfg(test)]
mod conditional_write_tests {
    //! What `If-Match` accepts, and — more usefully — what it refuses.
    //!
    //! Every case here is a header a real client could plausibly send. Two of them look correct and
    //! are not, which is why they are pinned rather than reasoned about: a weak validator, which
    //! `If-Match` can never satisfy, and a wildcard, which is legal HTTP for exactly the
    //! last-write-wins behaviour ADR-0094 removes. The parse is a pure function so the refusal each
    //! produces can be asserted by name rather than by status code alone.

    use super::{EntityTagRefusal, parse_entity_tag};

    #[test]
    fn a_strong_entity_tag_unwraps_to_the_token_the_adapter_minted() {
        // Byte-identical to what the read handed out, quotes stripped and nothing else touched —
        // the token is opaque, so any normalisation here would corrupt a fork's scheme.
        for (header, token) in [
            (r#""1847302""#, "1847302"),
            (r#""sha256:9f86d0818""#, "sha256:9f86d0818"),
            (r#"  "42"  "#, "42"),
            (r#""""#, ""),
        ] {
            assert_eq!(
                parse_entity_tag(header).map(|version| version.as_str().to_owned()),
                Ok(token.to_owned()),
                "parsing {header}"
            );
        }
    }

    #[test]
    fn a_weak_validator_is_refused_because_if_match_could_never_match_it() {
        // RFC 9110 §13.1.1: `If-Match` uses the *strong* comparison function, under which a weak
        // validator never matches. Accepting `W/"…"` would accept a tag that can only ever fail,
        // and the caller would read the resulting 412 as a real conflict and go hunting for an edit
        // that nobody made.
        assert_eq!(
            parse_entity_tag(r#"W/"1847302""#),
            Err(EntityTagRefusal::Malformed)
        );
    }

    #[test]
    fn a_wildcard_is_refused_because_it_is_last_write_wins_by_another_name() {
        // `If-Match: *` is legal HTTP meaning "whatever is there now" — precisely the behaviour
        // ADR-0094 exists to remove. It gets its own refusal rather than the malformed one, because
        // the caller's header is well-formed and the answer it needs is different.
        assert_eq!(parse_entity_tag("*"), Err(EntityTagRefusal::Wildcard));
        assert_eq!(parse_entity_tag("  *  "), Err(EntityTagRefusal::Wildcard));
    }

    #[test]
    fn a_list_of_tags_is_refused_because_one_row_has_one_version() {
        // A list is legal HTTP and means "any of these". A row has one version, so a list names no
        // intent this surface could honour. The inner-quote check is what catches it.
        assert_eq!(
            parse_entity_tag(r#""a", "b""#),
            Err(EntityTagRefusal::Malformed)
        );
    }

    #[test]
    fn an_unquoted_token_is_refused_rather_than_guessed_at() {
        // Sending the token bare is the likeliest client mistake. Guessing that the caller meant
        // `"1847302"` would work until the day a scheme mints a token that is itself quoted.
        for header in ["1847302", r#""unterminated"#, r#"unopened""#] {
            assert_eq!(
                parse_entity_tag(header),
                Err(EntityTagRefusal::Malformed),
                "parsing {header}"
            );
        }
    }
}
