// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `If-Match` a lease bump and a config write name.

use axum::http::HeaderMap;
use axum::http::header::IF_MATCH;
use axum::response::Response;
use pos_cloud_api::response::api_error_with_details;
use pos_core::lease::LeaseGeneration;
use pos_proto::ErrorStatus;
use pos_proto::ids::ConfigVersionId;

use crate::http::{EntityTagRefusal, parse_entity_tag, version_mismatch};

/// The generation a lease bump's `If-Match` names, or `None` for `*`.
///
/// **This parses; it does not check.** Unlike [`if_match_config`], which compares against a version
/// it has already read, the expectation here is handed to the write and evaluated inside the
/// statement's own `WHERE` — so there is no read-then-write window for a second admin to land in.
/// That matters more here than elsewhere: two bumps racing do not merely produce a lost update, they
/// let the second overwrite `superseded_generation`, destroying the record that the *first*
/// displaced machine never drained — with the very column that exists to hold that record.
///
/// `*` means "this store has never been issued a lease", the same claim `if_match_config` uses it
/// for, and the insert branch is the one that honours it: a numbered tag against a store with no
/// lease row inserts nothing.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
pub(in crate::http) fn if_match_lease(
    headers: &HeaderMap,
) -> Result<Option<LeaseGeneration>, Response> {
    let Some(raw) = headers.get(IF_MATCH) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a lease bump must carry the generation it was read at, as an If-Match header; \
             If-Match: * asserts the store has never been issued a lease",
            &[("if-match", "REQUIRED")],
        ));
    };
    let Ok(raw) = raw.to_str() else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must be a strong entity-tag",
            &[("if-match", "INVALID_FORMAT")],
        ));
    };
    match parse_entity_tag(raw.trim()) {
        Ok(tag) => match tag.as_str().parse::<u64>() {
            Ok(generation) => Ok(Some(LeaseGeneration::new(generation))),
            Err(_) => Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a lease If-Match names a generation, which is a whole number",
                &[("if-match", "INVALID_FORMAT")],
            )),
        },
        Err(EntityTagRefusal::Wildcard) => Ok(None),
        Err(EntityTagRefusal::Malformed) => Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must be a strong entity-tag",
            &[("if-match", "INVALID_FORMAT")],
        )),
    }
}

/// The `If-Match` a config publish must carry, compared against the version the tree holds.
///
/// A store that has never been published to has no current version, and says so: `If-Match: *` is
/// how a caller asserts "there is nothing here yet". That is the one place a wildcard is accepted in
/// this tree — the record-shaped routes refuse it, because for them it means "overwrite whatever is
/// there", which is the behaviour ADR-0094 exists to remove. Here it means the opposite: *only* if
/// nothing is there.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
pub(in crate::http) fn if_match_config(
    headers: &HeaderMap,
    current: Option<ConfigVersionId>,
) -> Result<(), Response> {
    let Some(raw) = headers.get(IF_MATCH) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "this write must carry the version it was read at, as an If-Match header",
            &[("if-match", "REQUIRED")],
        ));
    };
    let Ok(raw) = raw.to_str() else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must be a strong entity-tag",
            &[("if-match", "INVALID_FORMAT")],
        ));
    };
    let trimmed = raw.trim();

    let Some(current) = current else {
        return if trimmed == "*" {
            Ok(())
        } else {
            Err(version_mismatch())
        };
    };
    match parse_entity_tag(trimmed) {
        Ok(expected) if expected.as_str() == current.to_string() => Ok(()),
        // A wildcard here claims the store has never been published to, and it has: same refusal as
        // naming the wrong version, because it is the same wrong claim about the same tree.
        Ok(_) | Err(EntityTagRefusal::Wildcard) => Err(version_mismatch()),
        Err(EntityTagRefusal::Malformed) => Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "if-match must be a strong entity-tag",
            &[("if-match", "INVALID_FORMAT")],
        )),
    }
}
