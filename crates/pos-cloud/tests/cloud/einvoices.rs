// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console's invoice list and its corrections (ADR-0241 decision 5).

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::einvoice::feed::{InMemorySubmissions, SubmissionStore as _};
use pos_cloud::einvoice::symbols::{
    InMemorySymbolLedger, SymbolLedger as _, SymbolRecord, SymbolSource,
};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use store_postgres::SubmissionRow;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, admin_cookie, app_all, clock,
    get_with_cookie, json_body, post_with_cookie, provisioned_admin, role_session_cookie, store_id,
    tenant,
};

const INVOICE: &str = "01KAAAAAAAAAAAAAAAAAAAAAAA";

fn original() -> SubmissionRow {
    SubmissionRow {
        tenant_id: tenant().to_string(),
        invoice_id: INVOICE.to_owned(),
        store_id: store_id().to_string(),
        bill_id: "01KBBBBBBBBBBBBBBBBBBBBBBB".to_owned(),
        invoice_number: "1C23MAA/00000001".to_owned(),
        kind: "ORIGINAL".to_owned(),
        issued_on: "2023-11-14".to_owned(),
        total_minor: 165_000,
        currency: "VND".to_owned(),
        buyer_subject_id: Some("01KCCCCCCCCCCCCCCCCCCCCCCC".to_owned()),
        connection_id: None,
        corrects_invoice_number: None,
        state: "SUBMITTED".to_owned(),
        failed_attempts: 0,
        next_attempt_ms: 0,
        authority_reference: Some("SANDBOX-1".to_owned()),
        refusal: None,
        recorded_at_ms: 1,
        updated_at_ms: 1,
    }
}

async fn app() -> (axum::Router, InMemorySubmissions, crate::FakeAdmin) {
    let admin = provisioned_admin();
    let trees = FakeConfigTrees::default();
    trees.seed_store_layer(
        tenant(),
        store_id(),
        serde_json::json!({ "locale": { "country_code": "VN" } }),
    );
    let submissions = InMemorySubmissions::default();
    submissions.record(original()).await.expect("record");
    // The store's own symbol is in the ledger, as every symbol given since ADR-0240 is.
    let ledger = InMemorySymbolLedger::default();
    ledger
        .record(
            tenant(),
            SymbolRecord {
                symbol: "1C23MAA".to_owned(),
                store_id: Some(store_id().to_string()),
                lease_generation: None,
                source: SymbolSource::Allocated,
                recorded_at_ms: 1,
            },
        )
        .await
        .expect("record");
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        trees.clone(),
        FakeWebhooks::default(),
    ))
    .merge(http::einvoices_router(
        trees,
        admin.clone(),
        clock(),
        Arc::new(NoopAuditRecorder),
        Arc::new(submissions.clone()),
        Arc::new(ledger),
    ));
    (router, submissions, admin)
}

/// The list names each invoice's state and no buyer; a correction is numbered under the tenant's
/// correction symbol, names what it corrects, and waits to be submitted.
#[tokio::test]
async fn an_invoice_is_listed_and_corrected_under_the_correction_symbol() {
    let (router, _submissions, _admin) = app().await;
    let cookie = admin_cookie(&router).await;
    let list = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/einvoices?tenant_id={}", tenant()),
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(list.status(), StatusCode::OK);
    let body = json_body(list).await;
    assert_eq!(body[0]["invoice_number"], "1C23MAA/00000001");
    assert_eq!(body[0]["state"], "SUBMITTED");
    assert!(
        body[0].get("buyer_subject_id").is_none(),
        "no buyer is named"
    );

    let correct = |kind: &str, total: i64| {
        post_with_cookie(
            &format!("/admin/einvoices/{INVOICE}/correct"),
            &serde_json::json!({ "tenant_id": tenant().to_string(), "kind": kind, "total_minor": total }),
            &cookie,
        )
    };
    let first = router
        .clone()
        .oneshot(correct("ADJUSTMENT", -165_000))
        .await
        .expect("route");
    assert_eq!(first.status(), StatusCode::CREATED);
    let first = json_body(first).await;
    assert_eq!(
        first["invoice_number"], "1C23MAB/00000001",
        "the first number of a correction symbol no store holds"
    );
    assert_eq!(first["corrects_invoice_number"], "1C23MAA/00000001");
    assert_eq!(first["state"], "PENDING");
    assert_eq!(first["total_minor"], -165_000);
    let second = router
        .clone()
        .oneshot(correct("REPLACEMENT", 150_000))
        .await
        .expect("route");
    assert_eq!(
        json_body(second).await["invoice_number"],
        "1C23MAB/00000002"
    );

    let refused = router
        .clone()
        .oneshot(correct("VOID", 0))
        .await
        .expect("route");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    // Security review E1: no correction of nothing, and no negative replacement.
    for (kind, total) in [("ADJUSTMENT", 0), ("REPLACEMENT", -1)] {
        let refused = router
            .clone()
            .oneshot(correct(kind, total))
            .await
            .expect("route");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{kind} {total}");
        let body = json_body(refused).await;
        assert_eq!(body["error"]["details"][0]["field"], "total_minor");
    }
}

/// Correcting is for the owner and the admin; ops may read the list.
#[tokio::test]
async fn ops_reads_the_list_and_cannot_correct() {
    let (router, _submissions, admin) = app().await;
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let list = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/einvoices?tenant_id={}", tenant()),
            &ops,
        ))
        .await
        .expect("route");
    assert_eq!(list.status(), StatusCode::OK);
    let correct = router
        .oneshot(post_with_cookie(
            &format!("/admin/einvoices/{INVOICE}/correct"),
            &serde_json::json!({ "tenant_id": tenant().to_string(), "kind": "ADJUSTMENT", "total_minor": 1 }),
            &ops,
        ))
        .await
        .expect("route");
    assert_eq!(correct.status(), StatusCode::FORBIDDEN);
}
