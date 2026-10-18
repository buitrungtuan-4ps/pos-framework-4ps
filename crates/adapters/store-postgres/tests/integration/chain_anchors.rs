// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's half of the chained event log, against a real planner
//! ([ADR-0131](../../../../../docs/adr/0131-a-chained-event-log.md) decision 4).
//!
//! These cases exist because the refusal is enforced by the **schema**, not by the application
//! layer above it: `chain_anchors` is keyed `(tenant_id, store_id, chain_seq)` and written with
//! `ON CONFLICT DO NOTHING`, so a store gets one head per chain length and nothing can replace
//! it. Reasoning about a primary key is not the same as watching PostgreSQL enforce one.

use super::{TENANT_A, TENANT_B, block_on, prepared};
use store_postgres::{AnchorRow, ConflictRow};

fn anchor(chain_seq: i64, head: &str) -> AnchorRow {
    AnchorRow {
        chain_seq,
        chain_head: head.to_owned(),
        unchained: 0,
        observed_at: 1_700_000_000_000,
    }
}

#[test]
fn a_stores_head_is_the_highest_anchor_it_published() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let anchors = store.chain_anchors();

        for row in [anchor(10, "aa"), anchor(25, "bb"), anchor(61, "cc")] {
            anchors
                .record_anchor(TENANT_A, "store-1", &row)
                .await
                .expect("record an anchor");
        }
        // Another store's chain, and another tenant's, must not be mistaken for this one's.
        anchors
            .record_anchor(TENANT_A, "store-2", &anchor(999, "dd"))
            .await
            .expect("record store-2");
        anchors
            .record_anchor(TENANT_B, "store-1", &anchor(999, "ee"))
            .await
            .expect("record tenant-b");

        let (head, at_length) = anchors
            .held_for(TENANT_A, "store-1", 25)
            .await
            .expect("read what is held");
        assert_eq!(
            head.map(|row| row.chain_seq),
            Some(61),
            "the head is the longest chain this store published — not another store's, and \
             not another tenant's"
        );
        assert_eq!(
            at_length.map(|row| row.chain_head),
            Some("bb".to_owned()),
            "and the row at the offered length is the one recorded there"
        );
    });
}

#[test]
fn the_head_is_also_the_row_at_its_own_length() {
    block_on(async {
        // A store re-sending its latest anchor: one row is both answers, and a read that
        // returned it as only one of them would make a re-delivery look like a backfill.
        let (store, _admin) = prepared().await.expect("prepare the database");
        let anchors = store.chain_anchors();
        anchors
            .record_anchor(TENANT_A, "store-1", &anchor(42, "ab"))
            .await
            .expect("record an anchor");

        let (head, at_length) = anchors
            .held_for(TENANT_A, "store-1", 42)
            .await
            .expect("read what is held");
        assert_eq!(head.as_ref().map(|row| row.chain_seq), Some(42));
        assert_eq!(at_length.as_ref().map(|row| row.chain_seq), Some(42));
    });
}

#[test]
fn a_store_with_no_anchors_holds_nothing() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let (head, at_length) = store
            .chain_anchors()
            .held_for(TENANT_A, "store-1", 1)
            .await
            .expect("read what is held");
        assert!(head.is_none() && at_length.is_none());
    });
}

#[test]
fn a_re_delivered_anchor_is_a_no_op() {
    block_on(async {
        // At-least-once ingest guarantees this happens. It must not fail and must not duplicate.
        let (store, _admin) = prepared().await.expect("prepare the database");
        let anchors = store.chain_anchors();
        for _ in 0..3 {
            anchors
                .record_anchor(TENANT_A, "store-1", &anchor(42, "ab"))
                .await
                .expect("a re-delivery must not fail");
        }
        let (head, _) = anchors
            .held_for(TENANT_A, "store-1", 42)
            .await
            .expect("read what is held");
        assert_eq!(head.map(|row| row.chain_head), Some("ab".to_owned()));
    });
}

#[test]
fn a_contradicting_head_cannot_displace_the_one_already_recorded() {
    block_on(async {
        // The recomputation case, and the reason the primary key is the mechanism rather than a
        // tidiness measure: the store rewrote its history, re-derived every link, and now hashes
        // its log of length 42 to something else. The write succeeds — `ON CONFLICT DO NOTHING`
        // — and changes nothing, so the record that makes the contradiction visible survives
        // even if the layer above forgot to check.
        let (store, _admin) = prepared().await.expect("prepare the database");
        let anchors = store.chain_anchors();
        anchors
            .record_anchor(TENANT_A, "store-1", &anchor(42, "ab"))
            .await
            .expect("record the original");

        anchors
            .record_anchor(TENANT_A, "store-1", &anchor(42, "ff"))
            .await
            .expect("the write does not error");

        let (head, at_length) = anchors
            .held_for(TENANT_A, "store-1", 42)
            .await
            .expect("read what is held");
        assert_eq!(
            head.map(|row| row.chain_head),
            Some("ab".to_owned()),
            "the cloud's record is not overwritten by the one that contradicts it"
        );
        assert_eq!(at_length.map(|row| row.chain_head), Some("ab".to_owned()));
    });
}

#[test]
fn a_truncated_chain_cannot_reclaim_the_lengths_it_already_published() {
    block_on(async {
        // The truncation case, as far as the anchors alone reach. A store that published heads
        // at 10, 25 and 61, then cut its log back and re-traded, has to re-publish lengths the
        // cloud already holds — and every one of them is refused. What this does *not* catch is
        // a store that truncates and then only ever anchors *above* 61: that looks like honest
        // growth from here, and it is the events, not the anchors, that give it away. Recorded
        // as a limit rather than glossed.
        let (store, _admin) = prepared().await.expect("prepare the database");
        let anchors = store.chain_anchors();
        for row in [anchor(10, "aa"), anchor(25, "bb"), anchor(61, "cc")] {
            anchors
                .record_anchor(TENANT_A, "store-1", &row)
                .await
                .expect("record the real history");
        }

        // Cut back to 10 and trade on: the rewritten chain reaches 25 and 61 again with
        // different contents.
        for row in [anchor(25, "f1"), anchor(61, "f2")] {
            anchors
                .record_anchor(TENANT_A, "store-1", &row)
                .await
                .expect("the write does not error");
        }

        for (seq, expected) in [(10, "aa"), (25, "bb"), (61, "cc")] {
            let (_, at_length) = anchors
                .held_for(TENANT_A, "store-1", seq)
                .await
                .expect("read what is held");
            assert_eq!(
                at_length.map(|row| row.chain_head),
                Some(expected.to_owned()),
                "the head the cloud received at length {seq} is the one it keeps"
            );
        }
    });
}

#[test]
fn conflicts_are_listed_newest_first_and_scoped_to_their_tenant() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let anchors = store.chain_anchors();

        let conflict = |chain_seq: i64, offered: &str| ConflictRow {
            chain_seq,
            held_head: "ab".to_owned(),
            offered_head: offered.to_owned(),
            offered_event_id: "EVT0000000000000000000001".to_owned(),
        };
        anchors
            .record_conflict("c1", TENANT_A, "store-1", &conflict(42, "f1"))
            .await
            .expect("record c1");
        anchors
            .record_conflict("c2", TENANT_A, "store-2", &conflict(43, "f2"))
            .await
            .expect("record c2");
        anchors
            .record_conflict("c3", TENANT_B, "store-1", &conflict(44, "f3"))
            .await
            .expect("record c3");

        let tenant_wide = anchors
            .list_conflicts(TENANT_A, None, 10)
            .await
            .expect("list tenant conflicts");
        let ids: Vec<&str> = tenant_wide
            .iter()
            .map(|row| row.conflict_id.as_str())
            .collect();
        assert_eq!(ids.len(), 2, "tenant-b's conflict does not leak in");
        assert!(ids.contains(&"c1") && ids.contains(&"c2"));

        let one_store = anchors
            .list_conflicts(TENANT_A, Some("store-1"), 10)
            .await
            .expect("list one store's conflicts");
        assert_eq!(one_store.len(), 1);
        let found = one_store.first().expect("one row");
        assert_eq!(found.conflict_id, "c1");
        assert_eq!(found.conflict.held_head, "ab");
        assert_eq!(found.conflict.offered_head, "f1");
        assert!(
            found.noticed_at > 0,
            "the cloud's own clock stamped it, which is the timestamp an investigation can rely \
             on — the store's is on the anchor and a tamperer controls that one"
        );
    });
}

#[test]
fn recording_the_same_conflict_twice_is_one_row() {
    block_on(async {
        // Reconciliation re-pushes whole windows, so the anchor that was refused will arrive
        // again. One finding must stay one row rather than flooding the list it is read from.
        let (store, _admin) = prepared().await.expect("prepare the database");
        let anchors = store.chain_anchors();
        let row = ConflictRow {
            chain_seq: 42,
            held_head: "ab".to_owned(),
            offered_head: "ff".to_owned(),
            offered_event_id: "EVT0000000000000000000001".to_owned(),
        };
        for _ in 0..3 {
            anchors
                .record_conflict("c1", TENANT_A, "store-1", &row)
                .await
                .expect("a re-record must not fail");
        }
        let listed = anchors
            .list_conflicts(TENANT_A, None, 10)
            .await
            .expect("list conflicts");
        assert_eq!(listed.len(), 1);
    });
}
