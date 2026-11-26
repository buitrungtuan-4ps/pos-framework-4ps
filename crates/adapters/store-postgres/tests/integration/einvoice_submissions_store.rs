// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The e-invoice submissions and the feed's cursors (ADR-0241, migration 0106).

use super::{TENANT_A, TENANT_B, block_on, prepared};

use store_postgres::SubmissionRow;

const STORE: &str = "01J0000000000000000000STR1";

fn row(invoice: &str, next_attempt_ms: i64) -> SubmissionRow {
    SubmissionRow {
        tenant_id: TENANT_A.to_owned(),
        invoice_id: invoice.to_owned(),
        store_id: STORE.to_owned(),
        bill_id: "01J00000000000000000000BL1".to_owned(),
        invoice_number: format!("1C26MAA/{invoice}"),
        kind: "ORIGINAL".to_owned(),
        issued_on: "2026-11-25".to_owned(),
        total_minor: 165_000,
        currency: "VND".to_owned(),
        buyer_subject_id: None,
        connection_id: None,
        corrects_invoice_number: None,
        state: "PENDING".to_owned(),
        failed_attempts: 0,
        next_attempt_ms,
        authority_reference: None,
        refusal: None,
        recorded_at_ms: next_attempt_ms,
        updated_at_ms: next_attempt_ms,
    }
}

/// An invoice is recorded once; the due read and the outcome write move it through its states; the
/// list reads one tenant only; the cursor is per store.
#[test]
fn a_submission_is_recorded_once_and_moves_through_its_states() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let submissions = store.einvoice_submissions();
        assert!(submissions.record(&row("I1", 10)).await.expect("record"));
        assert!(!submissions.record(&row("I1", 99)).await.expect("record"));
        assert!(submissions.record(&row("I2", 20)).await.expect("record"));
        let due = submissions.due(15, 10).await.expect("due");
        assert_eq!(
            due.iter()
                .map(|row| row.invoice_id.as_str())
                .collect::<Vec<_>>(),
            ["I1"]
        );

        let mut submitted = due.first().expect("one invoice is due").clone();
        submitted.state = "SUBMITTED".to_owned();
        submitted.authority_reference = Some("SANDBOX-1".to_owned());
        submissions.settle(&submitted).await.expect("settle");
        assert_eq!(
            submissions.get(TENANT_A, "I1").await.expect("get"),
            Some(submitted)
        );
        let open = submissions
            .list(TENANT_A, Some(STORE), true, 10)
            .await
            .expect("list");
        assert_eq!(
            open.iter()
                .map(|row| row.invoice_id.as_str())
                .collect::<Vec<_>>(),
            ["I2"]
        );
        assert!(
            submissions
                .list(TENANT_B, None, false, 10)
                .await
                .expect("list")
                .is_empty()
        );

        assert_eq!(
            submissions.cursor(TENANT_A, STORE).await.expect("cursor"),
            None
        );
        submissions
            .advance(TENANT_A, STORE, "E1")
            .await
            .expect("advance");
        submissions
            .advance(TENANT_A, STORE, "E2")
            .await
            .expect("advance");
        assert_eq!(
            submissions
                .cursor(TENANT_A, STORE)
                .await
                .expect("cursor")
                .as_deref(),
            Some("E2")
        );
    });
}

/// The corrections of one invoice are read oldest first, then by number, and of its tenant only.
#[test]
fn the_corrections_of_an_invoice_are_read_in_order() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let submissions = store.einvoice_submissions();
        submissions
            .record(&row("00000001", 10))
            .await
            .expect("records");
        for (id, number, at, kind) in [
            ("00000003", "1C26MAB/00000002", 30, "ADJUSTMENT"),
            ("00000002", "1C26MAB/00000001", 30, "REPLACEMENT"),
            ("00000004", "1C26MAB/00000003", 20, "ADJUSTMENT"),
        ] {
            let correction = SubmissionRow {
                invoice_number: number.to_owned(),
                kind: kind.to_owned(),
                corrects_invoice_number: Some("1C26MAA/00000001".to_owned()),
                ..row(id, at)
            };
            submissions.record(&correction).await.expect("records");
        }
        let read = submissions
            .corrections_of(TENANT_A, "1C26MAA/00000001")
            .await
            .expect("reads");
        let numbers: Vec<&str> = read.iter().map(|row| row.invoice_number.as_str()).collect();
        assert_eq!(
            numbers,
            ["1C26MAB/00000003", "1C26MAB/00000001", "1C26MAB/00000002"]
        );
        let other = submissions
            .corrections_of(TENANT_B, "1C26MAA/00000001")
            .await
            .expect("reads");
        assert!(other.is_empty(), "another tenant's are not read");
    });
}

/// A bill's original invoice is found by its bill, never a correction of it, and in its store only
/// (ADR-0245, migration 0108).
#[test]
fn a_bills_original_invoice_is_found_by_its_bill() {
    // Its own store and late due times, since the other tests here read the same table.
    const OWN_STORE: &str = "01J0000000000000000000STR3";
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let submissions = store.einvoice_submissions();
        let bill = "01J00000000000000000000BL1";
        assert_eq!(
            submissions
                .original_for_bill(TENANT_A, OWN_STORE, bill)
                .await
                .expect("reads"),
            None
        );
        let correction = SubmissionRow {
            invoice_number: "1K26TAA/00000001".to_owned(),
            kind: "ADJUSTMENT".to_owned(),
            corrects_invoice_number: Some("1C26MAA/00000007".to_owned()),
            store_id: OWN_STORE.to_owned(),
            ..row("00000008", 9_000_000_005)
        };
        submissions.record(&correction).await.expect("records");
        submissions
            .record(&SubmissionRow {
                store_id: OWN_STORE.to_owned(),
                ..row("00000007", 9_000_000_010)
            })
            .await
            .expect("records");
        let found = submissions
            .original_for_bill(TENANT_A, OWN_STORE, bill)
            .await
            .expect("reads");
        assert_eq!(
            found.map(|row| row.invoice_number),
            Some("1C26MAA/00000007".to_owned())
        );
        for (tenant, store_id) in [(TENANT_B, OWN_STORE), (TENANT_A, STORE)] {
            assert_eq!(
                submissions
                    .original_for_bill(tenant, store_id, bill)
                    .await
                    .expect("reads"),
                None
            );
        }
    });
}
