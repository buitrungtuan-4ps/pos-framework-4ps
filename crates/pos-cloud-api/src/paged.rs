// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A list route's paging params and the page it answers with
//! ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md)).
//!
//! Moved out of `pos-cloud`'s `http.rs` (ADR-0170 decision 4), beside [`crate::paging`]'s
//! `PageRequest` and `Page`. `pos-cloud`'s `http` module imports them.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pos_proto::ErrorStatus;
use serde::Serialize;

use crate::paging::{MAX_PAGE_LIMIT, MAX_PAGE_OFFSET, Page, PageRequest, PageRequestError};
use crate::refusal::present_param;
use crate::response::api_error_with_details;

/// Reads a list route's paging params: `None` for an unpaged read, `Some(Ok(_))` for a page,
/// `Some(Err(_))` for a refusal.
///
/// The three-way answer is the shape of ADR-0098's central decision. An absent `limit` is not a
/// missing value to default — it is the caller asking for the whole set, which is what a picker and
/// a compiler want and what these routes have always returned. Only a caller that names a limit gets
/// a page.
///
/// **`offset` without `limit` is refused**, not ignored. On its own it selects nothing coherent — the
/// unpaged read returns everything regardless — so honouring it silently would answer a question
/// nobody asked, and dropping it silently would lose one somebody did. Both are the kind of silence
/// ADR-0098 exists to remove.
///
/// **Two `&str` parameters rather than one flattened struct.** A `PageParams` carrying the pair and
/// `#[serde(flatten)]`-ed into each query struct would read better and does not compile here: the
/// flatten attribute makes serde's derive generate its buffering `Content` enum, whose `f32`/`f64`
/// variants `clippy.toml` bans outright (ADR-0013 — money is never a float), and it does so
/// regardless of the field types being flattened. So each list query declares its own
/// `Option<String>` pair and passes them here, and this function stays the only place the rule is
/// written. Measured, not assumed: flattening two `Option<String>` fields fails the `lints` gate with
/// `use of a disallowed type f32`.
///
/// The params arrive as text, not `u32`, for the same reason [`parse_ulid_fields`] takes strings:
/// typed as a number, axum's `Query` extractor would reject `?limit=abc` itself with its own
/// plain-text body, and every refusal on this surface is an AIP-193 envelope naming the field
/// (Q3a/Q3b).
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the refusal a route returns as-is"
)]
pub fn parse_page(
    limit: Option<&str>,
    offset: Option<&str>,
) -> Option<Result<PageRequest, Response>> {
    let Some(limit_text) = present_param(limit) else {
        // No limit, so no page. An offset with nothing to offset into is a caller mistake worth
        // naming rather than dropping.
        return present_param(offset).map(|_offset| Err(offset_without_limit_refusal()));
    };
    let Ok(limit) = limit_text.parse::<u32>() else {
        return Some(Err(page_bound_refusal("limit")));
    };
    let offset = match present_param(offset) {
        None => 0,
        Some(text) => match text.parse::<u32>() {
            Ok(value) => value,
            Err(_ignored) => return Some(Err(page_bound_refusal("offset"))),
        },
    };
    Some(match PageRequest::new(limit, offset) {
        Ok(request) => Ok(request),
        Err(PageRequestError::LimitOutOfRange) => Err(page_bound_refusal("limit")),
        Err(PageRequestError::OffsetOutOfRange) => Err(page_bound_refusal("offset")),
    })
}

/// The refusal for a `limit` or `offset` that is absent-shaped, unparseable, or out of range.
///
/// One message for all three because they are one mistake from the caller's side: the value it sent
/// is not a page bound this API accepts, and the sentence says what one is. Splitting "not a number"
/// from "too large" would give a client two strings to branch on where the fix is identical.
pub fn page_bound_refusal(field: &str) -> Response {
    let accepted = if field == "limit" {
        format!("an integer from 1 to {MAX_PAGE_LIMIT}")
    } else {
        format!("an integer from 0 to {MAX_PAGE_OFFSET}")
    };
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        format!("{field} must be {accepted}"),
        &[(field, "OUT_OF_RANGE")],
    )
}

/// The refusal for an `offset` sent without a `limit`.
///
/// Its own sentence because the fix is not "correct the offset" but "add a limit", and a caller told
/// only that its offset was refused would go looking at the wrong parameter.
pub fn offset_without_limit_refusal() -> Response {
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        "offset needs a limit: paging is a limit and an offset together, and without a limit this \
         read answers with its unpaged form instead",
        &[("offset", "MISSING_DEPENDENT_FIELD")],
    )
}

/// A page on the wire: the rows, the size of the whole set, and the bounds that produced them.
///
/// The bounds are echoed because a pager needs them to render "1–25 of 812" and to build the next
/// request, and because the alternative — the client tracking what it sent — breaks the moment a
/// link is shared or a page is reloaded.
#[derive(Debug, Clone, Serialize)]
struct PagedBody<T> {
    items: Vec<T>,
    total: u32,
    limit: u32,
    offset: u32,
}

/// Answers a paged read: `200` with the page, its total, and the bounds it used.
pub fn paged_ok<T: Serialize>(page: Page<T>, request: PageRequest) -> Response {
    (
        StatusCode::OK,
        Json(PagedBody {
            items: page.items,
            total: page.total,
            limit: request.limit(),
            offset: request.offset(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod page_params_tests {
    //! The rule for `?limit=`/`?offset=`, pinned where a future list route cannot fork it.
    //!
    //! The load-bearing case is the first one: an absent `limit` must stay "the whole set". Give it
    //! a default and five item pickers and the menu compiler start reading a page and saying nothing
    //! about it (ADR-0098).

    use super::{parse_page, present_param};
    use crate::paging::{MAX_PAGE_LIMIT, MAX_PAGE_OFFSET};

    #[test]
    fn no_limit_means_the_whole_set_and_not_a_default_page() {
        assert!(
            parse_page(None, None).is_none(),
            "an absent limit is the unpaged read, not a value to default"
        );
        // Blank and whitespace are absent too: a console clearing its page-size box sends `""`.
        assert!(parse_page(Some(""), None).is_none());
        assert!(parse_page(Some("   "), None).is_none());
    }

    #[test]
    fn a_limit_alone_starts_at_the_head_of_the_set() {
        let page = parse_page(Some("25"), None)
            .expect("a limit means a page")
            .expect("25 is in range");
        assert_eq!(page.limit(), 25);
        assert_eq!(page.offset(), 0, "no offset starts at the beginning");
    }

    #[test]
    fn an_offset_without_a_limit_is_refused_rather_than_ignored() {
        // Silently dropping it loses a question the caller asked; silently honouring it answers one
        // nobody did, since the unpaged read returns everything regardless.
        let refusal = parse_page(None, Some("50"))
            .expect("an offset is not nothing")
            .expect_err("but it is not a page either");
        assert_eq!(refusal.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_bound_that_is_not_a_number_is_refused_by_this_layer() {
        // Not axum's `Query` extractor: these arrive as text precisely so the refusal is an AIP-193
        // envelope naming the field rather than a plain-text rejection body.
        for (limit, offset) in [
            (Some("abc"), None),
            (Some("25"), Some("-1")),
            (Some("2.5"), None),
        ] {
            let refusal = parse_page(limit, offset)
                .expect("a limit was named")
                .expect_err("but it does not parse");
            assert_eq!(refusal.status(), axum::http::StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn a_bound_past_its_cap_is_refused_rather_than_clamped() {
        // Clamping would answer a different question than the one asked, and leave the caller to
        // notice by diffing what it sent against what came back. Nothing does that.
        for (limit, offset) in [
            (format!("{}", MAX_PAGE_LIMIT + 1), "0".to_owned()),
            ("0".to_owned(), "0".to_owned()),
            ("25".to_owned(), format!("{}", MAX_PAGE_OFFSET + 1)),
        ] {
            let refusal = parse_page(Some(&limit), Some(&offset))
                .expect("a limit was named")
                .expect_err("but a bound is out of range");
            assert_eq!(
                refusal.status(),
                axum::http::StatusCode::BAD_REQUEST,
                "limit={limit} offset={offset}"
            );
        }
        // The caps themselves are accepted: the range is inclusive on both ends.
        assert!(
            parse_page(
                Some(&MAX_PAGE_LIMIT.to_string()),
                Some(&MAX_PAGE_OFFSET.to_string())
            )
            .expect("a limit was named")
            .is_ok()
        );
    }

    #[test]
    fn a_present_bound_is_trimmed_before_it_is_parsed() {
        let page = parse_page(Some(" 25 "), Some(" 50 "))
            .expect("a limit was named")
            .expect("both are in range once trimmed");
        assert_eq!((page.limit(), page.offset()), (25, 50));
    }

    #[test]
    fn absent_blank_and_whitespace_are_the_same_absence() {
        // The shared rule, asserted on the helper itself so both bounds inherit it and a third
        // param added later cannot spell it differently.
        assert_eq!(present_param(None), None);
        assert_eq!(present_param(Some("")), None);
        assert_eq!(present_param(Some(" \t ")), None);
        assert_eq!(present_param(Some(" 7 ")), Some("7"));
    }
}
