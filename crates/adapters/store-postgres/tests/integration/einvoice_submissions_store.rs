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
