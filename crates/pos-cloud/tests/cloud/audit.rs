// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console audit trail (ADR-0069): the trail a store keeps, and `GET /admin/audit`.

use axum::http::StatusCode;
use pos_cloud::audit::{AuditActor, AuditEntry, AuditId, AuditQuery, AuditStore, TrailOrder};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::paging::PageRequest;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::TenantId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, NOW_MS, admin_cookie, app_all,
    clock, get, get_with_cookie, json_body, provisioned_admin, store_id, tenant,
};

#[tokio::test]
async fn audit_appends_and_lists_newest_first_scoped_by_tenant() {
    let audit = FakeAudit::default();
    let entry = |id: u128, tenant: Option<TenantId>, action: &str, at_ms: i64| AuditEntry {
        id: AuditId::new(Ulid::from_u128(id)),
        tenant_id: tenant,
        actor: AuditActor {
            admin_id: "01ADMIN0000000000000000OPS".to_owned(),
            email: "ops@pizza4ps.test".to_owned(),
            role: AdminRole::Ops,
        },
        action: action.to_owned(),
        entity_type: "store".to_owned(),
        entity_id: store_id().to_string(),
        before: None,
        after: Some(serde_json::json!({ "name": "Bến Thành" })),
        request_id: None,
        at: Timestamp::from_milliseconds_since_epoch(at_ms).expect("a valid instant"),
    };
    let mine = tenant();
    let other = TenantId::new(Ulid::from_u128(0xB0B));
    audit
        .append(&entry(1, Some(mine), "store.update", NOW_MS - 2_000))
        .await
        .expect("append 1");
    audit
        .append(&entry(2, Some(mine), "store.archive", NOW_MS - 1_000))
        .await
        .expect("append 2");
    audit
        .append(&entry(3, Some(other), "store.update", NOW_MS))
        .await
        .expect("append for another tenant");

    let listed = audit.list(Some(mine), 10).await.expect("list this tenant");
    assert_eq!(listed.len(), 2, "only this tenant's entries are listed");
    let newest = listed.first().expect("an entry");
    assert_eq!(
        newest.action, "store.archive",
        "the newest entry sorts first"
    );
    assert_eq!(
        newest.actor.email, "ops@pizza4ps.test",
        "the acting admin is snapshotted onto the entry"
    );
    assert!(
        listed.iter().any(|entry| entry.action == "store.update"),
        "the tenant's earlier entry is present too"
    );

    let all = audit.list(None, 10).await.expect("list across tenants");
    assert_eq!(all.len(), 3, "no tenant filter reads across every tenant");
}

/// Every page of the trail in one order, stitched into the sequence a caller paging through sees.
///
/// Reads until a page comes back empty rather than dividing by a total, so a page that dropped or
/// repeated a row shows up in the stitched sequence instead of being masked by the arithmetic.
async fn stitched_trail(
    audit: &FakeAudit,
    filter: &AuditQuery,
    order: TrailOrder,
    limit: u32,
) -> Vec<String> {
    let mut actions: Vec<String> = Vec::new();
    let mut offset = 0;
    loop {
        let page = PageRequest::new(limit, offset).expect("a valid page");
        let read = audit
            .query_page(filter, page, order)
            .await
            .expect("a page of the trail");
        if read.items.is_empty() {
            return actions;
        }
        actions.extend(read.items.iter().map(|entry| entry.action.clone()));
        offset += limit;
    }
}

/// The paged trail reads from either end, and both orders window one set.
///
/// Asserted as a whole stitched sequence rather than as two literal first pages: reversing *after*
/// the window — flipping one page's rows in place — would satisfy "the first page starts at the
/// other end" and still leave every page holding exactly the rows it held before.
#[tokio::test]
async fn the_paged_trail_reads_from_either_end_and_both_orders_window_the_same_set() {
    let audit = FakeAudit::default();
    let entry = |step: i64| AuditEntry {
        id: AuditId::new(Ulid::from_u128(u128::try_from(step).expect("a small step"))),
        tenant_id: Some(tenant()),
        actor: AuditActor {
            admin_id: "01ADMIN0000000000000000OPS".to_owned(),
            email: "ops@pizza4ps.test".to_owned(),
            role: AdminRole::Owner,
        },
        action: format!("store.step{step}"),
        entity_type: "store".to_owned(),
        entity_id: store_id().to_string(),
        before: None,
        after: None,
        request_id: None,
        at: Timestamp::from_milliseconds_since_epoch(NOW_MS - (5 - step) * 1_000)
            .expect("a valid instant"),
    };
    // Five entries a second apart — an odd count, so the last page is short in either direction.
    for step in 1..=5 {
        audit.append(&entry(step)).await.expect("append");
    }
    let filter = AuditQuery {
        tenant: Some(tenant()),
        ..AuditQuery::default()
    };

    let newest = stitched_trail(&audit, &filter, TrailOrder::Newest, 2).await;
    let oldest = stitched_trail(&audit, &filter, TrailOrder::Oldest, 2).await;
    assert_eq!(
        newest,
        vec![
            "store.step5".to_owned(),
            "store.step4".to_owned(),
            "store.step3".to_owned(),
            "store.step2".to_owned(),
            "store.step1".to_owned(),
        ],
        "newest-first is the default and is unchanged by the order existing",
    );
    let mut reversed = oldest.clone();
    reversed.reverse();
    assert_eq!(
        reversed, newest,
        "one order is the other read backwards — every page of it, not one page flipped in place",
    );

    // The order chooses which page a row lands on; it never changes which rows matched.
    let first = PageRequest::new(2, 0).expect("a valid page");
    let counted = audit
        .query_page(&filter, first, TrailOrder::Oldest)
        .await
        .expect("the first page oldest-first");
    assert_eq!(counted.total, 5, "the total is the match count either way");
}

/// A router over a trail of three entries: two `store` rows and one `tenant` row, oldest first.
///
/// Shared by the filter test and the paging test so both read the same trail — a page whose set
/// differed from the filtered read's would prove nothing about the two agreeing.
async fn audit_trail_app() -> axum::Router {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let entry = |id: u128, action: &str, entity_type: &str, at_ms: i64| AuditEntry {
        id: AuditId::new(Ulid::from_u128(id)),
        tenant_id: Some(tenant()),
        actor: AuditActor {
            admin_id: "01ADMIN0000000000000000OPS".to_owned(),
            email: "ops@pizza4ps.test".to_owned(),
            role: AdminRole::Owner,
        },
        action: action.to_owned(),
        entity_type: entity_type.to_owned(),
        entity_id: store_id().to_string(),
        before: None,
        after: Some(serde_json::json!({ "name": "Bến Thành" })),
        request_id: None,
        at: Timestamp::from_milliseconds_since_epoch(at_ms).expect("a valid instant"),
    };
    audit
        .append(&entry(1, "store.create", "store", NOW_MS - 2_000))
        .await
        .expect("append 1");
    audit
        .append(&entry(2, "store.update", "store", NOW_MS - 1_000))
        .await
        .expect("append 2");
    audit
        .append(&entry(3, "tenant.update", "tenant", NOW_MS))
        .await
        .expect("append 3");

    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::audit_router(audit, admin, clock()))
}

#[tokio::test]
async fn the_audit_read_filters_and_needs_a_session() {
    let router = audit_trail_app().await;

    // No session → the trail is behind the guard.
    let denied = router
        .clone()
        .oneshot(get("/admin/audit", None))
        .await
        .expect("route the unauthenticated read");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let cookie = admin_cookie(&router).await;

    // Unfiltered: every entry, newest first.
    let all = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit", &cookie))
        .await
        .expect("route the read");
    assert_eq!(all.status(), StatusCode::OK);
    let all = json_body(all).await;
    let rows = all.as_array().expect("array");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["action"], "tenant.update", "newest first");
    assert_eq!(
        rows[0]["actor_email"], "ops@pizza4ps.test",
        "the actor snapshot is flattened onto the view"
    );

    // Filter by entity type.
    let stores = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?entity_type=store", &cookie))
        .await
        .expect("route the filtered read");
    let stores = json_body(stores).await;
    assert_eq!(stores.as_array().expect("array").len(), 2);
    assert!(
        stores
            .as_array()
            .expect("array")
            .iter()
            .all(|row| row["entity_type"] == "store"),
        "only store entries survive the filter"
    );

    // Filter by action.
    let updates = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?action=store.update", &cookie))
        .await
        .expect("route the action-filtered read");
    let updates = json_body(updates).await;
    assert_eq!(updates.as_array().expect("array").len(), 1);
    assert_eq!(updates[0]["action"], "store.update");
}

#[tokio::test]
async fn the_audit_read_pages_when_asked_for_an_offset_and_windows_when_not() {
    let router = audit_trail_app().await;
    let cookie = admin_cookie(&router).await;

    // A limit alone is still the *windowed* read ADR-0069 shipped: the newest N as a bare array,
    // not a page. This is why `offset` and not `limit` is what asks this route for a page — a
    // caller sending `?limit=200` today must keep getting what it gets today (ADR-0098).
    let windowed = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?limit=2", &cookie))
        .await
        .expect("route the windowed read");
    let windowed = json_body(windowed).await;
    let windowed = windowed
        .as_array()
        .expect("a bare array, not a paged envelope");
    assert_eq!(windowed.len(), 2, "the newest two");
    assert_eq!(windowed[0]["action"], "tenant.update", "still newest-first");

    // An offset asks for the paged form: the window, plus how many matched in total.
    let page = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?limit=2&offset=0", &cookie))
        .await
        .expect("route the first page");
    let page = json_body(page).await;
    assert_eq!(page["items"].as_array().expect("the window").len(), 2);
    assert_eq!(
        page["total"], 3,
        "the total is the match count, not the page"
    );
    assert_eq!(page["limit"], 2);
    assert_eq!(page["offset"], 0);

    let tail = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?limit=2&offset=2", &cookie))
        .await
        .expect("route the second page");
    let tail = json_body(tail).await;
    assert_eq!(tail["items"].as_array().expect("the window").len(), 1);
    assert_eq!(tail["total"], 3);
    assert_eq!(
        tail["items"][0]["action"], "store.create",
        "the oldest entry is on the last page"
    );

    // The total counts what the *filters* matched, not the whole log — otherwise a pager over a
    // filtered view would offer pages that are empty.
    let filtered = router
        .clone()
        .oneshot(get_with_cookie(
            "/admin/audit?entity_type=store&limit=1&offset=0",
            &cookie,
        ))
        .await
        .expect("route the filtered page");
    let filtered = json_body(filtered).await;
    assert_eq!(filtered["items"].as_array().expect("the window").len(), 1);
    assert_eq!(filtered["total"], 2, "two store entries, not three entries");

    // The windowed form clamps an over-large limit (ADR-0069's behaviour, kept); the paged form
    // refuses it, because there a clamp answers a different question than the one asked.
    let clamped = router
        .clone()
        .oneshot(get_with_cookie("/admin/audit?limit=100000", &cookie))
        .await
        .expect("route the over-large window");
    assert_eq!(
        clamped.status(),
        StatusCode::OK,
        "the windowed read pulls the bound into range rather than refusing"
    );
    assert_eq!(json_body(clamped).await.as_array().expect("array").len(), 3);

    for (query, field) in [
        ("limit=100000&offset=0", "limit"),
        ("limit=0&offset=0", "limit"),
        ("limit=2&offset=lots", "offset"),
        ("offset=2", "offset"),
        ("limit=lots", "limit"),
    ] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(&format!("/admin/audit?{query}"), &cookie))
            .await
            .expect("route the bad bound");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "`{query}` is a client mistake, not a page",
        );
        assert_eq!(
            json_body(refused).await["error"]["details"][0]["field"],
            field,
            "`{query}` names the parameter that was wrong",
        );
    }
}

#[tokio::test]
async fn the_paged_audit_read_takes_an_order_and_the_windowed_one_refuses_it() {
    let router = audit_trail_app().await;
    let cookie = admin_cookie(&router).await;
    let read = |query: &str| {
        let uri = format!("/admin/audit?{query}");
        let cookie = cookie.clone();
        let router = router.clone();
        async move {
            router
                .oneshot(get_with_cookie(&uri, &cookie))
                .await
                .expect("route the read")
        }
    };

    // Naming the default changes nothing: a caller that omits `order` and one that spells out the
    // trail's own order get the same page.
    let defaulted = json_body(read("limit=3&offset=0").await).await;
    let named = json_body(read("limit=3&offset=0&order=newest").await).await;
    assert_eq!(
        defaulted["items"], named["items"],
        "`newest` is the default"
    );
    assert_eq!(defaulted["items"][0]["action"], "tenant.update");

    // The other end. The trail is `store.create`, `store.update`, `tenant.update` oldest-first.
    let oldest = json_body(read("limit=1&offset=0&order=oldest").await).await;
    assert_eq!(
        oldest["items"][0]["action"], "store.create",
        "oldest-first starts at the beginning of the trail"
    );
    assert_eq!(
        oldest["total"], 3,
        "the total is the match count, which the order does not change"
    );
    let tail = json_body(read("limit=1&offset=2&order=oldest").await).await;
    assert_eq!(
        tail["items"][0]["action"], "tenant.update",
        "and the window walks to the newest entry rather than flipping one page in place"
    );

    // The windowed read refuses the order rather than ignoring it: there `limit` already means "the
    // most recent this many", so an order has two honest readings and the route will not guess.
    let refused = read("order=oldest").await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let refused = json_body(refused).await;
    assert_eq!(refused["error"]["details"][0]["field"], "order");
    assert_eq!(
        refused["error"]["details"][0]["reason"], "MISSING_DEPENDENT_FIELD",
        "the fix is to name an offset, not to change the value"
    );

    // A token that names no order is refused with the two that do.
    let unknown = read("limit=1&offset=0&order=ascending").await;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let unknown = json_body(unknown).await;
    assert_eq!(unknown["error"]["details"][0]["field"], "order");
    assert_eq!(
        unknown["error"]["message"], "order must be newest or oldest",
        "the refusal names what this route accepts, so a caller need not guess again"
    );
}
