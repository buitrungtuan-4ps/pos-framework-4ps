// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The reason-code routes (ADR-0115): authoring, its audit trail and who may read and write it, and
//! the publish to a store's `reason_codes` node.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditRecorder, AuditSink, NoopAuditRecorder};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::config_tree::ConfigTreeStore;
use pos_cloud::version::{Version, Versioned};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::ReasonCodeId;
use pos_proto::reason_codes::{PublishedReasonCode, ReasonAction, ReasonCode};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeAudit, FakeConfigTrees, FakeKeys, FakeReasonCodes, FakeRollups, FakeWebhooks,
    admin_cookie, app_all, clock, delete_with_cookie, etag_of, get_with_cookie, json_body,
    last_trail_entry, post_with_cookie, provisioned_admin, put_with_cookie, put_with_etag,
    role_session_cookie, store_id, tenant, ulid_text,
};

/// The admin surface plus the reason-code router, on fakes, recording into `audit`.
///
/// The trail is a real `AuditSink` over `FakeAudit` rather than the no-op the inventory routes use:
/// these entries are the record of who changed a **fraud control**, so what lands in them is part
/// of what this slice has to get right, not incidental.
fn reason_code_app(
    admin: FakeAdmin,
    reason_codes: FakeReasonCodes,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::reason_code_router(
        reason_codes,
        admin,
        clock(),
        audit,
    ))
}

/// A reason-code write body: the report handle, the name staff read, one Vietnamese translation, and
/// the actions it may be cited for.
fn reason_code_body(tenant: &str, code: &str, actions: &[&str], active: bool) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": tenant,
        "code": code,
        "display_name": "Waste or spoilage",
        "display_name_translations": { "vi": "Hàng hỏng hoặc hết hạn" },
        "applies_to": actions,
        "active": active,
    })
}

/// Authoring an entry mints its id, hands back the version to edit it at, and records in the trail
/// the two facts that decide whether the control holds: which actions it covers, and that it is live.
#[tokio::test]
async fn authoring_a_reason_code_mints_its_id_and_the_trail_records_what_it_covers() {
    let audit = FakeAudit::default();
    let router = reason_code_app(
        provisioned_admin(),
        FakeReasonCodes::default(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/reason-codes",
            &reason_code_body(
                &tenant,
                "WASTE",
                &["REASON_ACTION_VOID_LINE", "REASON_ACTION_STOCK_WASTE"],
                true,
            ),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let at_create = etag_of(&created);
    let body = json_body(created).await;
    let minted = body["id"]
        .as_str()
        .expect("the create answers the entry it stored")
        .to_owned();
    assert_eq!(
        minted.len(),
        26,
        "the server minted the id, so a caller cannot choose one an event already names"
    );
    assert_eq!(
        body["etag"].as_str().expect("an etag field"),
        at_create,
        "the version is in the body as well as the header, so the console can edit straight from a \
         create without re-reading"
    );
    assert_eq!(
        body["display_name_translations"]["vi"], "Hàng hỏng hoặc hết hạn",
        "§12: the picker's Vietnamese name survives the round trip, got {body}"
    );

    let entry = last_trail_entry(&audit, "reason_codes.create")
        .await
        .expect("the create is in the trail");
    assert_eq!(entry.entity_type, "reason_code");
    assert_eq!(entry.entity_id, minted);
    let after = entry.after.expect("the trail records what was stored");
    assert_eq!(after["code"], "WASTE");
    assert_eq!(
        after["applies_to"],
        serde_json::json!(["REASON_ACTION_VOID_LINE", "REASON_ACTION_STOCK_WASTE"]),
        "the actions are in the summary: narrowing them is how the control gets defeated, so a \
         trail that omitted them would record that a reason changed while hiding the change"
    );
    assert_eq!(after["active"], true);
}

/// Retiring an entry is the ordinary edit, needs the version the read carried, and shows in the
/// trail as the change of standing it is.
#[tokio::test]
async fn retiring_a_reason_code_needs_the_version_and_is_visible_in_the_trail() {
    let audit = FakeAudit::default();
    let store = FakeReasonCodes::default();
    let router = reason_code_app(
        provisioned_admin(),
        store.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/reason-codes",
            &reason_code_body(&tenant, "WASTE", &["REASON_ACTION_VOID_LINE"], true),
            &cookie,
        ))
        .await
        .expect("route the create");
    let at_create = etag_of(&created);
    let id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();
    let row = format!("/admin/reason-codes/{id}");
    let retire = reason_code_body(&tenant, "WASTE", &["REASON_ACTION_VOID_LINE"], false);

    let unconditional = router
        .clone()
        .oneshot(put_with_cookie(&row, &retire, &cookie))
        .await
        .expect("route the unconditional edit");
    assert_eq!(
        unconditional.status(),
        StatusCode::BAD_REQUEST,
        "ADR-0094: an edit without If-Match is refused rather than clobbering"
    );

    let retired = router
        .clone()
        .oneshot(put_with_etag(&row, &retire, &cookie, &at_create))
        .await
        .expect("route the retire");
    assert_eq!(retired.status(), StatusCode::OK);
    let at_retire = etag_of(&retired);
    assert_ne!(at_retire, at_create, "an applied edit moves the version");

    let replayed = router
        .clone()
        .oneshot(put_with_etag(&row, &retire, &cookie, &at_create))
        .await
        .expect("route the replay");
    assert_eq!(
        replayed.status(),
        StatusCode::PRECONDITION_FAILED,
        "a second manager holding the stale version is refused, not silently applied"
    );

    let entry = last_trail_entry(&audit, "reason_codes.update")
        .await
        .expect("the retire is in the trail");
    let before = entry.before.expect("the trail records what stood before");
    let after = entry.after.expect("and what stands now");
    assert_eq!(before["active"], true);
    assert_eq!(
        after["active"], false,
        "the trail says a reason left service, which is the half of the control an auditor asks \
         about: whether the reason a colleague's void would have had to cite was removed"
    );

    // And retiring keeps it resolvable: the list still holds it, because a historic
    // `sales.order_line.voided` names its id forever.
    let listed = json_body(
        router
            .oneshot(get_with_cookie(
                &format!("/admin/reason-codes?tenant_id={tenant}"),
                &cookie,
            ))
            .await
            .expect("route the list"),
    )
    .await;
    let rows = listed.as_array().expect("an array");
    assert_eq!(rows.len(), 1, "a retired entry stays in the list");
    assert_eq!(rows[0]["active"], false);
}

/// The three data-entry faults a reason-code form can make, each naming its own field.
#[tokio::test]
async fn a_reason_that_could_be_cited_for_nothing_is_refused_and_the_refusal_names_the_field() {
    let router = reason_code_app(
        provisioned_admin(),
        FakeReasonCodes::default(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    // No action at all: it would sit in the table and appear on no picker.
    let unusable = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/reason-codes",
            &reason_code_body(&tenant, "WASTE", &[], true),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(unusable.status(), StatusCode::BAD_REQUEST);
    let body = json_body(unusable).await;
    assert_eq!(body["error"]["details"][0]["field"], "applies_to");
    assert_eq!(body["error"]["details"][0]["reason"], "REQUIRED");

    // An action this cloud does not know matches nothing at the till, so storing it would author a
    // reason that silently applies to nothing.
    let unknown = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/reason-codes",
            &reason_code_body(&tenant, "WASTE", &["REASON_ACTION_TELEPORT"], true),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let body = json_body(unknown).await;
    assert_eq!(body["error"]["details"][0]["field"], "applies_to");
    assert_eq!(body["error"]["details"][0]["reason"], "INVALID_ENUM_VALUE");

    // A blank handle: a report would group by an empty string.
    let blank = router
        .oneshot(post_with_cookie(
            "/admin/reason-codes",
            &reason_code_body(&tenant, "   ", &["REASON_ACTION_VOID_LINE"], true),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(blank.status(), StatusCode::BAD_REQUEST);
    let body = json_body(blank).await;
    assert_eq!(body["error"]["details"][0]["field"], "code");
    assert_eq!(body["error"]["details"][0]["reason"], "REQUIRED");
}

/// Deleting is for the entry created by mistake, and it leaves the whole summary in the trail —
/// which is the only thing that can be done for an id an event may already name.
#[tokio::test]
async fn deleting_a_reason_code_leaves_a_readable_trail_and_the_entry_stops_resolving() {
    let audit = FakeAudit::default();
    let router = reason_code_app(
        provisioned_admin(),
        FakeReasonCodes::default(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/reason-codes",
            &reason_code_body(&tenant, "TYPO", &["REASON_ACTION_VOID_LINE"], true),
            &cookie,
        ))
        .await
        .expect("route the create");
    let id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();
    let row = format!("/admin/reason-codes/{id}?tenant_id={tenant}");

    let removed = router
        .clone()
        .oneshot(delete_with_cookie(&row, &cookie))
        .await
        .expect("route the delete");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    let entry = last_trail_entry(&audit, "reason_codes.delete")
        .await
        .expect("the delete is in the trail");
    let before = entry.before.expect("the trail keeps the whole entry");
    assert_eq!(before["code"], "TYPO");
    assert_eq!(before["name"], "Waste or spoilage");
    assert_eq!(
        before["applies_to"],
        serde_json::json!(["REASON_ACTION_VOID_LINE"]),
        "so a reader of the trail can still say what the deleted id meant"
    );

    let gone = router
        .clone()
        .oneshot(get_with_cookie(&row, &cookie))
        .await
        .expect("route the read");
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);

    // A second delete is refused as a `404` rather than answered as a success: the seam is
    // idempotent, but the route reads the row first so it has a `before` to record.
    let again = router
        .oneshot(delete_with_cookie(&row, &cookie))
        .await
        .expect("route the second delete");
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

/// Authoring the list and reading it are different permissions: a viewer may see what a store's
/// staff must cite, and may not change it.
#[tokio::test]
async fn a_viewer_reads_the_reason_codes_and_cannot_author_them() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let router = reason_code_app(
        admin,
        FakeReasonCodes::default(),
        Arc::new(NoopAuditRecorder),
    );
    let tenant = ulid_text(1);

    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/reason-codes?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the list");
    assert_eq!(
        listed.status(),
        StatusCode::OK,
        "console.data.read is granted to every role, so a viewer may read the list"
    );

    let denied = router
        .oneshot(post_with_cookie(
            "/admin/reason-codes",
            &reason_code_body(&tenant, "WASTE", &["REASON_ACTION_VOID_LINE"], true),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "authoring is behind console.reason_codes.manage — Owner and Admin only, because \
         removing a reason or adding a vague catch-all is how the fraud control gets defeated"
    );
}

/// The list carries when each entry was last written, so the console can tell the operator whether
/// what is on their screen is newer than what the shop is running.
///
/// Every one of these tables has had an `updated_at` since it was created and not one read returned
/// it, so the publish bar had to approximate from a within-session signal that forgot on reload — an
/// operator who edited yesterday and came back today was told the store was up to date (Wave 4).
///
/// Asserted on the wire rather than on the seam because the field's whole job is to leave the
/// process: a `Versioned` that holds the instant and a response that drops it would pass every
/// test above this one.
#[tokio::test]
async fn the_reason_code_list_says_when_each_entry_was_last_written() {
    let router = reason_code_app(
        provisioned_admin(),
        FakeReasonCodes::default(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/reason-codes",
            &reason_code_body(&tenant, "WASTE", &["REASON_ACTION_VOID_LINE"], true),
            &cookie,
        ))
        .await
        .expect("route the create");
    assert_eq!(created.status(), StatusCode::CREATED);

    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/reason-codes?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the list");
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_body(listed).await;
    let first = &rows.as_array().expect("an array")[0];
    assert!(
        first["updated_at_ms"].as_i64().is_some_and(|at| at > 0),
        "the row carries the instant it was written, in Unix milliseconds: {first}"
    );
}

/// One tenant's list never reaches another's, and a malformed tenant is a refusal rather than a
/// cross-tenant read.
#[tokio::test]
async fn a_tenants_reason_codes_are_its_own() {
    let router = reason_code_app(
        provisioned_admin(),
        FakeReasonCodes::default(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let mine = ulid_text(1);
    let theirs = ulid_text(2);

    router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/reason-codes",
            &reason_code_body(&mine, "WASTE", &["REASON_ACTION_VOID_LINE"], true),
            &cookie,
        ))
        .await
        .expect("route the create");

    let others = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/reason-codes?tenant_id={theirs}"),
                &cookie,
            ))
            .await
            .expect("route the other tenant's list"),
    )
    .await;
    assert_eq!(
        others.as_array().expect("an array").len(),
        0,
        "the other tenant authored nothing, and sees nothing"
    );

    let mistyped = router
        .oneshot(get_with_cookie(
            "/admin/reason-codes?tenant_id=nope",
            &cookie,
        ))
        .await
        .expect("route the mistyped listing");
    assert_eq!(mistyped.status(), StatusCode::BAD_REQUEST);
}

// --- Reason-code publish (ADR-0115, B2.2 slice 4) -----------------------------------------------

/// The admin surface plus the reason-code publish route, on fakes.
fn reason_code_publish_app(
    admin: FakeAdmin,
    reason_codes: FakeReasonCodes,
    config_trees: FakeConfigTrees,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config_trees.clone(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::config_reason_codes_router(
        reason_codes,
        config_trees,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// Seeds one authored entry directly into the store, valid for `actions`.
fn seed_reason_code(store: &FakeReasonCodes, n: u128, code: &str, actions: &[ReasonAction]) {
    store.rows.lock().expect("lock").push((
        tenant(),
        Versioned::new(
            PublishedReasonCode::new(
                ReasonCodeId::new(Ulid::from_u128(n)),
                ReasonCode::new(code),
                DisplayName::new(code),
                actions.to_vec(),
            ),
            Version::new("1"),
        ),
    ));
}

/// The publish writes the node onto the Store layer and says what the store can no longer record.
#[tokio::test]
async fn publishing_reason_codes_writes_the_node_and_names_what_nothing_now_covers() {
    let store = FakeReasonCodes::default();
    seed_reason_code(
        &store,
        10,
        "WASTE",
        &[ReasonAction::VoidLine, ReasonAction::StockWaste],
    );
    let config_trees = FakeConfigTrees::default();
    let router = reason_code_publish_app(provisioned_admin(), store, config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
    });

    let published = router
        .oneshot(put_with_cookie(
            "/admin/config/reason-codes",
            &body,
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let answered = json_body(published).await;
    assert_eq!(answered["published"], 1);
    assert_eq!(answered["active"], 1);

    // The heart of it: a store that publishes one void reason can no longer record a comp, a
    // refund, a drawer opening or a cash movement, because the node REPLACED the framework set and
    // offers nothing for those. That may be what the operator meant; it must not be a surprise.
    let uncovered = answered["uncovered_actions"]
        .as_array()
        .expect("the publish reports coverage");
    assert!(
        uncovered.contains(&serde_json::json!("REASON_ACTION_COMP")),
        "a comp has no reason to cite after this publish, and the response says so: {answered}"
    );
    assert!(
        !uncovered.contains(&serde_json::json!("REASON_ACTION_VOID_LINE")),
        "while the action they did author for is not listed"
    );
    assert!(
        !uncovered.contains(&serde_json::json!("REASON_ACTION_UNSPECIFIED")),
        "the wire enum's zero value is not an act anyone performs, so it is not a gap"
    );
    assert!(
        !uncovered.contains(&serde_json::json!("REASON_ACTION_CASH_VARIANCE")),
        "a list with no reason for a variance still lets every drawer close (ADR-0167 decision \
         12), so it is not a gap either: {answered}"
    );

    // And the node is on the store's Store layer (index 2), beside the other keys.
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    let node = &state.record.layers[2]["reason_codes"];
    assert_eq!(
        node["codes"].as_array().expect("the codes array").len(),
        1,
        "the published node is the authored list"
    );
}

/// Publishing an empty list is refused, because the node replaces the framework set.
///
/// This is the hazard ADR-0115's "absence is not a brick" reasoning creates in the other direction:
/// absence is safe *because* the framework default is still there, and an empty publish is the one
/// way to take it away. A store that took it would be unable to void a mis-keyed line at all.
#[tokio::test]
async fn publishing_an_empty_reason_list_is_refused_rather_than_disarming_the_store() {
    let config_trees = FakeConfigTrees::default();
    let router = reason_code_publish_app(
        provisioned_admin(),
        FakeReasonCodes::default(),
        config_trees.clone(),
    );
    let cookie = admin_cookie(&router).await;

    let refused = router
        .oneshot(put_with_cookie(
            "/admin/config/reason-codes",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
            }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "reason_codes");
    assert_eq!(body["error"]["details"][0]["reason"], "EMPTY_NOT_ALLOWED");

    assert!(
        config_trees
            .load(tenant(), store_id())
            .await
            .expect("load")
            .is_none(),
        "and nothing was written, so the store keeps the framework set it was trading on"
    );
}

/// An entry valid for no action is refused at publish — the guarantee ADR-0115 puts here.
#[tokio::test]
async fn publishing_a_reason_valid_for_nothing_is_refused_and_names_it() {
    let store = FakeReasonCodes::default();
    seed_reason_code(&store, 10, "WASTE", &[ReasonAction::VoidLine]);
    seed_reason_code(&store, 11, "ORPHAN", &[]);
    let config_trees = FakeConfigTrees::default();
    let router = reason_code_publish_app(provisioned_admin(), store, config_trees.clone());
    let cookie = admin_cookie(&router).await;

    let refused = router
        .oneshot(put_with_cookie(
            "/admin/config/reason-codes",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
            }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let body = json_body(refused).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("ORPHAN"),
        "the refusal names the entry so an operator knows which row to fix, got {body}"
    );
    assert_eq!(body["error"]["details"][0]["field"], "applies_to");
    assert!(
        config_trees
            .load(tenant(), store_id())
            .await
            .expect("load")
            .is_none(),
        "and the whole publish is refused rather than half-applied"
    );
}
