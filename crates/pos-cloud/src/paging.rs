// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Paged reads for the console ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md)).
//!
//! A `/admin` list answers one of two questions, and they are not the same question. "Give me every
//! item, because I am filling a dropdown and need all of them" is what five of the six console
//! consumers of `GET /admin/items` ask, and what the menu compiler asks. "Give me twenty-five rows
//! and tell me how many there are" is what a table asks. The first read is unchanged and permanent:
//! `?limit=` absent still returns a bare array of every row. This module is the vocabulary for the
//! second.
//!
//! # `limit` has no default, and that is the point
//!
//! If an absent `?limit=` meant "25", every picker in the console would quietly start seeing a
//! quarter of a page, and a menu would compile without the items that fell off it. Nothing would
//! raise. So paging is something a caller opts into by naming a limit, and [`PageRequest`] cannot be
//! constructed without one.
//!
//! # Out of range is refused, not clamped
//!
//! [`PageRequest::new`] rejects a `limit` outside `1..=MAX_PAGE_LIMIT` and an `offset` above
//! [`MAX_PAGE_OFFSET`] rather than pulling them into range.
//!
//! It is worth being exact about why, because the obvious reason is not quite right. A paged
//! response echoes the `limit` and `offset` it actually used, so a clamp would not strictly be
//! *silent* — a caller could compare what it sent with what came back. But no caller does, and a
//! request outside the contract is a client bug: reporting it where it happens is more useful than
//! answering a different question and leaving the client to diff the two. For `offset` it is also
//! substantive, since a caller stitching pages together would place the returned rows at the offset
//! it asked for, not the one it got.
//!
//! Note this differs from [`RollupWindow`](crate::dashboard::rollup::RollupWindow), which *clamps*
//! its `limit` to a maximum window of days (ADR-0081). That behaviour is unchanged and out of scope
//! here; the rule going forward, for paged reads, is the one in this module.

/// The most rows one page may carry.
///
/// Generous — a console table shows tens, and an export reads unpaged anyway — and bounded so that
/// `?limit=1000000` cannot ask the cloud to build a response it should not.
pub const MAX_PAGE_LIMIT: u32 = 500;

/// The furthest into a set a page may start.
///
/// A backstop, not a tuned figure. Deep offsets are the cost `OFFSET` cannot avoid — the database
/// still walks the skipped rows — so a caller reaching this is the signal that the read wants keyset
/// paging, which ADR-0098 deliberately did not decide.
pub const MAX_PAGE_OFFSET: u32 = 100_000;

/// What a caller asked of a page: how many rows, starting where.
///
/// There is no `Default`, and the fields are private behind [`limit`](Self::limit) and
/// [`offset`](Self::offset), so the only way to hold one is to have gone through
/// [`new`](Self::new) and had the range checked. A store implementation can therefore put
/// `self.limit()` straight into a `LIMIT` clause without re-validating it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PageRequest {
    limit: u32,
    offset: u32,
}

impl PageRequest {
    /// Checks a requested page against the caps.
    ///
    /// # Errors
    ///
    /// [`PageRequestError::LimitOutOfRange`] if `limit` is zero or above [`MAX_PAGE_LIMIT`];
    /// [`PageRequestError::OffsetOutOfRange`] if `offset` is above [`MAX_PAGE_OFFSET`].
    pub const fn new(limit: u32, offset: u32) -> Result<Self, PageRequestError> {
        if limit == 0 || limit > MAX_PAGE_LIMIT {
            return Err(PageRequestError::LimitOutOfRange);
        }
        if offset > MAX_PAGE_OFFSET {
            return Err(PageRequestError::OffsetOutOfRange);
        }
        Ok(Self { limit, offset })
    }

    /// How many rows the page carries at most.
    #[must_use]
    pub const fn limit(&self) -> u32 {
        self.limit
    }

    /// How many rows to skip before the page starts.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }
}

/// Why a requested page is not one.
///
/// An enum rather than a message, for the reason
/// [`WindowError`](crate::dashboard::rollup::WindowError) is one: this module knows *what* is out of
/// range and only the route knows the wire names, so the field naming stays at the HTTP seam and
/// this type grows no opinion about `details`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageRequestError {
    /// `limit` is zero — which selects nothing — or above [`MAX_PAGE_LIMIT`].
    LimitOutOfRange,
    /// `offset` is above [`MAX_PAGE_OFFSET`].
    OffsetOutOfRange,
}

/// One page of rows, and the size of the set they came from.
///
/// `total` counts what matched, not what this page carries, so a pager can render "1–25 of 812". It
/// is exact and comes from the same statement as the rows (`COUNT(*) OVER()` in the store-postgres
/// impl): an estimate would make the pager disagree with a set the admin is actively editing.
///
/// Not `Serialize`. The wire envelope also echoes the `limit` and `offset` the request carried, and
/// this type is what a *store* returns — it was told the limit and has no business repeating it back.
/// Assembling the two is the HTTP layer's job.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Page<T> {
    /// The rows, in the read's own order.
    pub items: Vec<T>,
    /// How many rows matched in total, across every page.
    pub total: u32,
}

impl<T> Page<T> {
    /// Pairs a page's rows with the size of the set they came from.
    #[must_use]
    pub const fn new(items: Vec<T>, total: u32) -> Self {
        Self { items, total }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PAGE_LIMIT, MAX_PAGE_OFFSET, Page, PageRequest, PageRequestError};

    #[test]
    fn a_page_of_zero_rows_is_refused_because_it_selects_nothing() {
        // `?limit=0` is not "no limit" — the absence of the parameter is. Reading zero as unbounded
        // is the one interpretation that turns a typo into a full-table read.
        assert_eq!(
            PageRequest::new(0, 0),
            Err(PageRequestError::LimitOutOfRange)
        );
    }

    #[test]
    fn the_caps_are_inclusive_and_one_past_them_is_refused() {
        assert!(PageRequest::new(MAX_PAGE_LIMIT, MAX_PAGE_OFFSET).is_ok());
        assert_eq!(
            PageRequest::new(MAX_PAGE_LIMIT + 1, 0),
            Err(PageRequestError::LimitOutOfRange)
        );
        assert_eq!(
            PageRequest::new(1, MAX_PAGE_OFFSET + 1),
            Err(PageRequestError::OffsetOutOfRange)
        );
    }

    #[test]
    fn an_out_of_range_request_names_which_field_was_out_of_range() {
        // The two are separate variants because the route answers them with different `details`
        // entries; collapsing them would send a caller to check the parameter that was fine.
        assert_ne!(
            PageRequestError::LimitOutOfRange,
            PageRequestError::OffsetOutOfRange
        );
    }

    #[test]
    fn a_request_that_passed_the_check_hands_its_values_through_unchanged() {
        // Nothing is clamped on the way through: what the caller asked for is what a store's
        // `LIMIT`/`OFFSET` receives, which is what makes re-validating in the adapter unnecessary.
        let request = PageRequest::new(25, 50).expect("in range");
        assert_eq!(request.limit(), 25);
        assert_eq!(request.offset(), 50);
    }

    #[test]
    fn a_pages_total_is_the_size_of_the_set_not_of_the_page() {
        // The distinction the pager renders: twenty-five rows out of eight hundred and twelve.
        let page = Page::new(vec![(); 25], 812);
        assert_eq!(page.items.len(), 25);
        assert_eq!(page.total, 812);
    }
}
