// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The queue of correction requests (ADR-0250 decision 3, migration 0109).

use super::{TENANT_A, TENANT_B, block_on, prepared};

use store_postgres::CorrectionRequestRow;

fn row(request: &str, at: i64) -> CorrectionRequestRow {
    CorrectionRequestRow {
        tenant_id: TENANT_A.to_owned(),
        request_id: request.to_owned(),
        store_id: "01J0000000000000000000RQS1".to_owned(),
        bill_id: "01J0000000000000000000BIL1".to_owned(),
        amount_minor: 165_000,
        currency_code: "VND".to_owned(),
        kind: "ADJUSTMENT".to_owned(),
        reason_code_id: None,
        requested_by: Some("01J0000000000000000000EMP1".to_owned()),
        requested_time_ms: at,
        status: "PENDING".to_owned(),
        decided_by_user_id: None,
        decided_time_ms: None,
    }
}

/// A request is recorded once, listed by status, decided once, and another tenant reads none.
#[test]
fn a_request_is_queued_once_and_decided_once() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let queue = store.einvoice_correction_requests();
        let first = row("01J0000000000000000000REQ1", 1);
        assert!(queue.record(&first).await.expect("record"));
        assert!(
            !queue.record(&first).await.expect("record"),
            "recorded once"
        );
        let second = row("01J0000000000000000000REQ2", 2);
        assert!(queue.record(&second).await.expect("record"));
        let pending = queue
            .list(TENANT_A, Some("PENDING"), 10)
            .await
            .expect("list");
        assert_eq!(pending, vec![first.clone(), second.clone()]);
        assert!(
            queue
                .list(TENANT_B, None, 10)
                .await
                .expect("list")
                .is_empty()
        );

        assert!(
            queue
                .decide(
                    TENANT_A,
                    &first.request_id,
                    "APPROVED",
                    "01J0000000000000000000USR1",
                    9
                )
                .await
                .expect("decide")
        );
        assert!(
            !queue
                .decide(
                    TENANT_A,
                    &first.request_id,
                    "DECLINED",
                    "01J0000000000000000000USR2",
                    10
                )
                .await
                .expect("decide"),
            "a decided request stays decided"
        );
        let decided = queue
            .get(TENANT_A, &first.request_id)
            .await
            .expect("get")
            .expect("a row");
        assert_eq!(
            (
                decided.status.as_str(),
                decided.decided_by_user_id.as_deref(),
                decided.decided_time_ms
            ),
            ("APPROVED", Some("01J0000000000000000000USR1"), Some(9))
        );
        assert_eq!(
            queue
                .list(TENANT_A, Some("PENDING"), 10)
                .await
                .expect("list"),
            vec![second]
        );
    });
}
