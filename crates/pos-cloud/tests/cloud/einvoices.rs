// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console's invoice list and its corrections (ADR-0241 decision 5).

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::config_tree::ConfigTreeStore as _;
use pos_cloud::einvoice::feed::{InMemorySubmissions, SubmissionStore as _};
use pos_cloud::einvoice::requests::{CorrectionRequests as _, InMemoryCorrectionRequests};
use pos_cloud::einvoice::symbols::{
    InMemorySymbolLedger, SymbolLedger as _, SymbolRecord, SymbolSource,
};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use store_postgres::{CorrectionRequestRow, SubmissionRow};
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
    let (router, submissions, admin, _queue, _trees) = app_with_queue().await;
    (router, submissions, admin)
}

async fn app_with_queue() -> (
    axum::Router,
    InMemorySubmissions,
    crate::FakeAdmin,
    InMemoryCorrectionRequests,
    FakeConfigTrees,
) {
    let admin = provisioned_admin();
    let trees = FakeConfigTrees::default();
    trees.seed_store_layer(
        tenant(),
        store_id(),
        serde_json::json!({ "locale": { "country_code": "VN" } }),
    );
    let submissions = InMemorySubmissions::default();
    submissions.record(original()).await.expect("record");
    let queue = InMemoryCorrectionRequests::default();
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
        trees.clone(),
        admin.clone(),
        clock(),
        Arc::new(NoopAuditRecorder),
        Arc::new(submissions.clone()),
        Arc::new(ledger),
        Arc::new(queue.clone()),
    ));
    (router, submissions, admin, queue, trees)
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
    // The replacement stands the invoice at 150,000: an adjustment may take it to nothing, never
    // below, counting every correction recorded so far.
    for total in [-150_001, -200_000] {
        let refused = router
            .clone()
            .oneshot(correct("ADJUSTMENT", total))
            .await
            .expect("route");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{total}");
        assert_eq!(
            json_body(refused).await["error"]["details"][0]["field"],
            "total_minor"
        );
    }
    let to_nothing = router
        .clone()
        .oneshot(correct("ADJUSTMENT", -150_000))
        .await
        .expect("route");
    assert_eq!(to_nothing.status(), StatusCode::CREATED);
    let below = router
        .clone()
        .oneshot(correct("ADJUSTMENT", -1))
        .await
        .expect("route");
    assert_eq!(
        below.status(),
        StatusCode::BAD_REQUEST,
        "already at nothing"
    );
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

const REQUEST: &str = "01KDDDDDDDDDDDDDDDDDDDDDDD";
const BILL: &str = "01KBBBBBBBBBBBBBBBBBBBBBBB";

fn pending(request_id: &str, bill_id: &str) -> CorrectionRequestRow {
    CorrectionRequestRow {
        tenant_id: tenant().to_string(),
        request_id: request_id.to_owned(),
        store_id: store_id().to_string(),
        bill_id: bill_id.to_owned(),
        amount_minor: 165_000,
        currency_code: "VND".to_owned(),
        kind: "ADJUSTMENT".to_owned(),
        reason_code_id: Some("01KEEEEEEEEEEEEEEEEEEEEEEE".to_owned()),
        requested_by: Some("01KFFFFFFFFFFFFFFFFFFFFFFF".to_owned()),
        requested_time_ms: 10,
        status: "PENDING".to_owned(),
        decided_by_user_id: None,
        decided_time_ms: None,
    }
}

/// The queue lists a pending request; approving it records the approver, keeps the requester, and
/// publishes the decision on the store's own `einvoice` node beside what the node held. A second
/// approval publishes it again; declining it then is refused, and another request is declined.
#[tokio::test]
async fn hq_approves_or_declines_a_request_and_the_store_learns_it_on_its_einvoice_node() {
    let (router, _submissions, _admin, queue, trees) = app_with_queue().await;
    trees.seed_store_layer(
        tenant(),
        store_id(),
        serde_json::json!({ "locale": { "country_code": "VN" }, "einvoice": { "symbol": "1C23MAA" } }),
    );
    queue.record(pending(REQUEST, BILL)).await.expect("record");
    let other = "01KGGGGGGGGGGGGGGGGGGGGGGG";
    queue
        .record(pending(other, "01KHHHHHHHHHHHHHHHHHHHHHHH"))
        .await
        .expect("record");
    let cookie = admin_cookie(&router).await;
    let list = |status: &str| {
        get_with_cookie(
            &format!(
                "/admin/einvoices/correction-requests?tenant_id={}&status={status}",
                tenant()
            ),
            &cookie,
        )
    };
    let listed = json_body(
        router
            .clone()
            .oneshot(list("PENDING"))
            .await
            .expect("route"),
    )
    .await;
    assert_eq!(listed.as_array().map(Vec::len), Some(2));
    assert_eq!(listed[0]["request_id"], REQUEST);
    assert_eq!(listed[0]["requested_by"], "01KFFFFFFFFFFFFFFFFFFFFFFF");
    let bad = router.clone().oneshot(list("MAYBE")).await.expect("route");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let decide = |request: &str, verb: &str| {
        post_with_cookie(
            &format!("/admin/einvoices/correction-requests/{request}/{verb}"),
            &serde_json::json!({ "tenant_id": tenant().to_string() }),
            &cookie,
        )
    };
    let approved = router
        .clone()
        .oneshot(decide(REQUEST, "approve"))
        .await
        .expect("route");
    assert_eq!(approved.status(), StatusCode::OK);
    let approved = json_body(approved).await;
    assert_eq!(approved["status"], "APPROVED");
    assert_eq!(approved["requested_by"], "01KFFFFFFFFFFFFFFFFFFFFFFF");
    assert!(
        approved["decided_by_user_id"].is_string(),
        "the approver is recorded by id"
    );
    assert!(approved["config_version_id"].is_string(), "published");

    let declined = router
        .clone()
        .oneshot(decide(other, "decline"))
        .await
        .expect("route");
    assert_eq!(declined.status(), StatusCode::OK);
    let again = router
        .clone()
        .oneshot(decide(REQUEST, "approve"))
        .await
        .expect("route");
    assert_eq!(
        again.status(),
        StatusCode::OK,
        "approving again publishes again"
    );
    let flip = router
        .clone()
        .oneshot(decide(REQUEST, "decline"))
        .await
        .expect("route");
    assert_eq!(flip.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(flip).await["error"]["details"][0]["reason"],
        "CORRECTION_ALREADY_DECIDED"
    );
    let missing = router
        .clone()
        .oneshot(decide("01KJJJJJJJJJJJJJJJJJJJJJJJ", "approve"))
        .await
        .expect("route");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    the_store_learns_both_decisions(&trees).await;
}

/// The store's node keeps its symbol and holds one entry per decided bill, which the store reads.
async fn the_store_learns_both_decisions(trees: &FakeConfigTrees) {
    let tree = trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a tree");
    let node = &tree.record.effective()["einvoice"];
    assert_eq!(
        node["symbol"], "1C23MAA",
        "the node's other fields are kept"
    );
    let decisions = node["correction_decisions"].as_array().expect("decisions");
    assert_eq!(decisions.len(), 2, "one entry per bill");
    let published: pos_proto::einvoice::PublishedEInvoice =
        serde_json::from_value(node.clone()).expect("the store reads its node");
    let bill = pos_proto::ids::BillId::new(BILL.parse().expect("a ULID"));
    assert!(
        published
            .correction_decision(bill)
            .is_some_and(pos_proto::einvoice::DecidedCorrection::approved)
    );
    let declined_bill =
        pos_proto::ids::BillId::new("01KHHHHHHHHHHHHHHHHHHHHHHH".parse().expect("a ULID"));
    assert!(
        published
            .correction_decision(declined_bill)
            .is_some_and(|decided| !decided.approved())
    );
}

#[tokio::test]
async fn ops_reads_the_queue_and_cannot_decide() {
    let (router, _submissions, admin, queue, _trees) = app_with_queue().await;
    queue.record(pending(REQUEST, BILL)).await.expect("record");
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let list = router
        .clone()
        .oneshot(get_with_cookie(
            &format!(
                "/admin/einvoices/correction-requests?tenant_id={}",
                tenant()
            ),
            &ops,
        ))
        .await
        .expect("route");
    assert_eq!(list.status(), StatusCode::OK);
    let approve = router
        .oneshot(post_with_cookie(
            &format!("/admin/einvoices/correction-requests/{REQUEST}/approve"),
            &serde_json::json!({ "tenant_id": tenant().to_string() }),
            &ops,
        ))
        .await
        .expect("route");
    assert_eq!(approve.status(), StatusCode::FORBIDDEN);
}
