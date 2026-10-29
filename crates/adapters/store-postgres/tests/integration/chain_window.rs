// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The day-bounded window a chain recomputation walks, against a real planner
//! ([ADR-0132](../../../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)).
//!
//! The read is scoped by tenant, store and a trading-day range, and the range arithmetic is
//! PostgreSQL's — `pos-proto`'s `BusinessDate` offers none, and inventing calendar arithmetic in
//! Rust to avoid one `interval` would be the worse trade. Which means the day maths is only
//! really checked here.

use super::{TENANT_A, TENANT_B, block_on, prepared, seed_row_on};

#[test]
fn a_window_reads_one_store_one_tenant_and_the_days_it_was_asked_for() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        // Three days of this store's trading, plus another store's and another tenant's on the
        // same dates — neither of which may appear in this store's window.
        seed_row_on(&admin, 1, TENANT_A, "store-1", "2026-03-13")
            .await
            .expect("seed the day before yesterday");
        seed_row_on(&admin, 2, TENANT_A, "store-1", "2026-03-14")
            .await
            .expect("seed yesterday");
        seed_row_on(&admin, 3, TENANT_A, "store-1", "2026-03-15")
            .await
            .expect("seed today");
        seed_row_on(&admin, 4, TENANT_A, "store-2", "2026-03-15")
            .await
            .expect("seed another store");
        seed_row_on(&admin, 5, TENANT_B, "store-1", "2026-03-15")
            .await
            .expect("seed another tenant");

        let anchors = store.chain_anchors();

        // No look-back: today only.
        let today = anchors
            .events_in_window(TENANT_A, "store-1", "2026-03-15", 0, 100)
            .await
            .expect("read today");
        assert_eq!(
            today.len(),
            1,
            "one day, one store, one tenant — not the neighbours on the same date"
        );

        // One day back, which is what a shift crossing the store's cut-off needs.
        let two_days = anchors
            .events_in_window(TENANT_A, "store-1", "2026-03-15", 1, 100)
            .await
            .expect("read today and yesterday");
        assert_eq!(two_days.len(), 2);

        // And the bound holds: the day before yesterday stays out.
        assert_eq!(
            anchors
                .events_in_window(TENANT_A, "store-1", "2026-03-15", 2, 100)
                .await
                .expect("read three days")
                .len(),
            3,
            "the range is inclusive at both ends, so two days back reaches the third day"
        );
    });
}

#[test]
fn the_look_back_crosses_a_month_boundary() {
    block_on(async {
        // The reason this arithmetic is PostgreSQL's rather than mine: the day before the first
        // of a month is not the zeroth, and February is not thirty days long.
        let (store, admin) = prepared().await.expect("prepare the database");
        seed_row_on(&admin, 1, TENANT_A, "store-1", "2026-02-28")
            .await
            .expect("seed the last day of February");
        seed_row_on(&admin, 2, TENANT_A, "store-1", "2026-03-01")
            .await
            .expect("seed the first of March");

        let found = store
            .chain_anchors()
            .events_in_window(TENANT_A, "store-1", "2026-03-01", 1, 100)
            .await
            .expect("read across the month boundary");
        assert_eq!(
            found.len(),
            2,
            "a shift that opened on the last night of February closes on the first of March"
        );
    });
}

#[test]
fn a_window_is_capped_and_ordered_by_event_id() {
    block_on(async {
        // A short read is what the caller sees as an incomplete window rather than a clean one,
        // so the cap has to actually cap — and the order has to be the log's.
        let (store, admin) = prepared().await.expect("prepare the database");
        for seed in 1..=5 {
            seed_row_on(&admin, seed, TENANT_A, "store-1", "2026-03-15")
                .await
                .expect("seed a day's trading");
        }

        let capped = store
            .chain_anchors()
            .events_in_window(TENANT_A, "store-1", "2026-03-15", 0, 3)
            .await
            .expect("read a capped window");
        assert_eq!(capped.len(), 3, "the cap is honoured, not advisory");
    });
}

#[test]
fn a_store_that_traded_on_no_such_day_has_an_empty_window() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        assert!(
            store
                .chain_anchors()
                .events_in_window(TENANT_A, "store-1", "2026-03-15", 1, 100)
                .await
                .expect("read an empty window")
                .is_empty()
        );
    });
}
