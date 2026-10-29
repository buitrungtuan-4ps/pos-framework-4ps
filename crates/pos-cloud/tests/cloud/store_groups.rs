// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The batch publish to a store group (ADR-0122): an outcome per member, a batch that cannot
//! compile, an unknown node, an empty or archived cohort, the group's history and the session.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::AuditSink;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::fees::InMemoryFeeRules;
use pos_cloud::registry::EntityStatus;
use pos_cloud::store_groups::{StoreGroup, StoreGroupId};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeCatalog, FakeConfigTrees, FakeRegistry, FakeStoreGroups, admin_cookie,
    author_a_menu, get_with_cookie, json_body, post_json, post_with_cookie, provisioned_admin,
    put_with_cookie, put_with_etag, role_session_cookie, seed_menu_prerequisites, seed_store,
    store_group_app, store_group_app_with_fees, ulid_text,
};

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one batch over three shops in three different states, asserted end to end — the \
              per-store report is the deliverable ADR-0122 §5 names, so splitting the assertions \
              across tests would duplicate the whole three-store fixture to check it in pieces"
)]
async fn a_batch_publish_reports_an_outcome_for_every_member() {
    // Three shops in one cohort, each in a different state, and the third must be published to
    // *regardless* of the first two — which is the whole of ADR-0122 §5 and §7 in one assertion.
    let tenant_id = TenantId::new(Ulid::from_u128(1));
    let archived = StoreId::new(Ulid::from_u128(11));
    let incomplete = StoreId::new(Ulid::from_u128(12));
    let ready = StoreId::new(Ulid::from_u128(13));

    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, archived, false);
    seed_store(&registry, tenant_id, incomplete, true);
    seed_store(&registry, tenant_id, ready, true);

    let config_trees = FakeConfigTrees::default();
    seed_menu_prerequisites(&config_trees, tenant_id, ready);

    let group_id = StoreGroupId::new(Ulid::from_u128(21));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "Airport branches".to_owned(),
            status: EntityStatus::Active,
        },
        &[archived, incomplete, ready],
    );

    let router = store_group_app(
        provisioned_admin(),
        groups.clone(),
        registry,
        config_trees.clone(),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();
    let menu_id = author_a_menu(&router, &cookie, &tenant).await;

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/store-groups/{}/publish", group_id.as_ulid()),
            &serde_json::json!({
                "tenant_id": tenant,
                "node": "menu",
                "arguments": { "menu_id": menu_id },
            }),
            &cookie,
        ))
        .await
        .expect("route the batch publish");
    assert_eq!(published.status(), StatusCode::OK);
    let report = json_body(published).await;

    assert_eq!(report["applied"], 1);
    assert_eq!(report["skipped"], 2);
    assert_eq!(report["failed"], 0);
    let results = report["results"].as_array().expect("results");
    assert_eq!(
        results.len(),
        3,
        "every member has a row, which is the half of a batch that must not be partial"
    );

    let outcome_of = |store: StoreId| -> serde_json::Value {
        results
            .iter()
            .find(|row| row["store_id"] == store.as_ulid().to_string())
            .cloned()
            .expect("a row for the store")
    };

    let archived_row = outcome_of(archived);
    assert_eq!(archived_row["outcome"], "skipped");
    assert!(
        archived_row["detail"]
            .as_str()
            .expect("a detail")
            .contains("archived"),
        "a protected store says why, rather than being filed beside one that broke"
    );

    let incomplete_row = outcome_of(incomplete);
    assert_eq!(incomplete_row["outcome"], "skipped");
    assert!(
        incomplete_row["detail"]
            .as_str()
            .expect("a detail")
            .contains("`tax`"),
        "the missing prerequisite is named: this is the store that would have taken the order and \
         then failed at the payment screen"
    );

    let ready_row = outcome_of(ready);
    assert_eq!(
        ready_row["outcome"], "applied",
        "a shop that was ready is published to whatever happened at the other two"
    );
    assert!(
        ready_row["config_version_id"].is_string(),
        "an applied row carries the version it produced, which is what makes drift visible"
    );

    // The tree really moved, and only at the store that applied.
    let rows = config_trees.rows.lock().expect("lock");
    assert!(
        rows.get(&(tenant_id, ready)).expect("a tree").record.layers[2]
            .get("menu")
            .is_some(),
        "the ready store's Store layer carries the compiled book"
    );
    assert!(
        rows.get(&(tenant_id, archived)).is_none(),
        "a skipped store was not written to at all"
    );
    drop(rows);

    // The batch is durable and closed, and the report is reconstructible from the two tables.
    let stored = groups.batches.lock().expect("lock");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].node, "menu");
    assert!(
        stored[0].finished_at_ms.is_some(),
        "a batch that ran to the end records that it finished"
    );
    assert_eq!(groups.results.lock().expect("lock").len(), 3);
}

#[tokio::test]
async fn a_batch_whose_instruction_cannot_compile_writes_to_no_store() {
    // The refusal that would be the same answer at all 200 members refuses the batch instead of
    // being recorded 200 times (ADR-0122 §3).
    let tenant_id = TenantId::new(Ulid::from_u128(2));
    let store_id = StoreId::new(Ulid::from_u128(14));
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store_id, true);

    let group_id = StoreGroupId::new(Ulid::from_u128(22));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "Pilots".to_owned(),
            status: EntityStatus::Active,
        },
        &[store_id],
    );

    let config_trees = FakeConfigTrees::default();
    let router = store_group_app(
        provisioned_admin(),
        groups.clone(),
        registry,
        config_trees.clone(),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();

    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/store-groups/{}/publish", group_id.as_ulid()),
            &serde_json::json!({
                "tenant_id": tenant,
                "node": "capabilities",
                "arguments": { "flags": { "tables_enabled_typo": true } },
            }),
            &cookie,
        ))
        .await
        .expect("route the batch publish");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("tables_enabled_typo"),
        "the batch refuses by the single-store route's own rule, naming the flag"
    );

    assert!(
        groups.batches.lock().expect("lock").is_empty(),
        "an unpublishable instruction records no batch: nothing happened, and the history should \
         not suggest otherwise"
    );
    assert!(
        config_trees.rows.lock().expect("lock").is_empty(),
        "and no store was written to"
    );
}

#[tokio::test]
async fn a_batch_names_the_nodes_it_can_publish_when_asked_for_one_it_cannot() {
    // `locale` is excluded by construction (ADR-0122 §4) — per-store under ADR-0114 — and an
    // operator who asks for it needs to be told that, not that it does not exist.
    let tenant_id = TenantId::new(Ulid::from_u128(3));
    let group_id = StoreGroupId::new(Ulid::from_u128(23));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "City".to_owned(),
            status: EntityStatus::Active,
        },
        &[StoreId::new(Ulid::from_u128(15))],
    );
    let router = store_group_app(
        provisioned_admin(),
        groups,
        FakeRegistry::default(),
        FakeConfigTrees::default(),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;

    let refused = router
        .oneshot(post_with_cookie(
            &format!("/admin/store-groups/{}/publish", group_id.as_ulid()),
            &serde_json::json!({
                "tenant_id": tenant_id.as_ulid().to_string(),
                "node": "locale",
                "arguments": {},
            }),
            &cookie,
        ))
        .await
        .expect("route the batch publish");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let message = json_body(refused).await["error"]["message"]
        .as_str()
        .expect("a message")
        .to_owned();
    for node in [
        "menu",
        "tax",
        "campaigns",
        "inventory",
        "reason_codes",
        "permissions",
        "floor",
        "capabilities",
        "channels",
        "tender",
        "origins",
        "qr",
        "vendor_policies",
    ] {
        assert!(
            message.contains(node),
            "the refusal lists `{node}`, so an operator can fix the request without the ADR"
        );
    }
}

#[tokio::test]
async fn an_empty_or_archived_cohort_is_refused_rather_than_reported_as_a_success() {
    let tenant_id = TenantId::new(Ulid::from_u128(4));
    let empty = StoreGroupId::new(Ulid::from_u128(24));
    let retired = StoreGroupId::new(Ulid::from_u128(25));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id: empty,
            tenant_id,
            name: "Nobody".to_owned(),
            status: EntityStatus::Active,
        },
        &[],
    );
    groups.seed(
        StoreGroup {
            group_id: retired,
            tenant_id,
            name: "Last year's pilots".to_owned(),
            status: EntityStatus::Archived,
        },
        &[StoreId::new(Ulid::from_u128(16))],
    );
    let router = store_group_app(
        provisioned_admin(),
        groups,
        FakeRegistry::default(),
        FakeConfigTrees::default(),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();
    let body = serde_json::json!({ "tenant_id": tenant, "node": "tax", "arguments": {} });

    for (group_id, why) in [(empty, "reach nobody"), (retired, "archived")] {
        let refused = router
            .clone()
            .oneshot(post_with_cookie(
                &format!("/admin/store-groups/{}/publish", group_id.as_ulid()),
                &body,
                &cookie,
            ))
            .await
            .expect("route the batch publish");
        assert_eq!(
            refused.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "a publish that reaches no shop is not a success with zero results"
        );
        assert!(
            json_body(refused).await["error"]["message"]
                .as_str()
                .expect("a message")
                .contains(why)
        );
    }
}

#[tokio::test]
async fn a_batch_is_readable_afterwards_from_the_groups_history() {
    let tenant_id = TenantId::new(Ulid::from_u128(5));
    let store_id = StoreId::new(Ulid::from_u128(17));
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store_id, true);

    let group_id = StoreGroupId::new(Ulid::from_u128(26));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "One shop".to_owned(),
            status: EntityStatus::Active,
        },
        &[store_id],
    );
    let router = store_group_app(
        provisioned_admin(),
        groups,
        registry,
        FakeConfigTrees::default(),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/store-groups/{}/publish", group_id.as_ulid()),
            &serde_json::json!({ "tenant_id": tenant, "node": "tax", "arguments": {} }),
            &cookie,
        ))
        .await
        .expect("route the batch publish");
    assert_eq!(published.status(), StatusCode::OK);
    let batch_id = json_body(published).await["batch_id"]
        .as_str()
        .expect("a batch id")
        .to_owned();

    let history = router
        .clone()
        .oneshot(get_with_cookie(
            &format!(
                "/admin/store-groups/{}/batches?tenant_id={tenant}",
                group_id.as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the history");
    assert_eq!(history.status(), StatusCode::OK);
    let listed = json_body(history).await;
    assert_eq!(listed.as_array().expect("array").len(), 1);
    assert_eq!(listed[0]["batch_id"], batch_id);
    assert_eq!(listed[0]["applied"], 1);

    let one = router
        .clone()
        .oneshot(get_with_cookie(
            &format!(
                "/admin/store-groups/{}/batches/{batch_id}?tenant_id={tenant}",
                group_id.as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route the single batch");
    assert_eq!(one.status(), StatusCode::OK);
    let read = json_body(one).await;
    assert_eq!(
        read["results"][0]["store_id"],
        store_id.as_ulid().to_string()
    );
    assert_eq!(read["results"][0]["outcome"], "applied");

    // A batch id from nowhere is a `404` on this group, not somebody else's report.
    let missing = router
        .oneshot(get_with_cookie(
            &format!(
                "/admin/store-groups/{}/batches/{}?tenant_id={tenant}",
                group_id.as_ulid(),
                Ulid::from_u128(999)
            ),
            &cookie,
        ))
        .await
        .expect("route the missing batch");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_batch_publish_needs_a_session() {
    let router = store_group_app(
        provisioned_admin(),
        FakeStoreGroups::default(),
        FakeRegistry::default(),
        FakeConfigTrees::default(),
        FakeCatalog::default(),
    );
    let refused = router
        .oneshot(post_json(
            &format!("/admin/store-groups/{}/publish", ulid_text(9)),
            &serde_json::json!({ "tenant_id": ulid_text(1), "node": "tax" }),
        ))
        .await
        .expect("route the batch publish");
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
}

// --- pinned before `admin_set_store_group_members` and `admin_publish_to_store_group` are split --
// --- (ADR-0170 decision 7) ---------------------------------------------------------------------

/// A cohort of two seeded stores and the group routes recording to `audit`, under `admin`.
fn seeded_cohort(
    admin: crate::FakeAdmin,
    audit: &FakeAudit,
) -> (axum::Router, FakeStoreGroups, StoreGroupId, [StoreId; 2]) {
    let tenant_id = TenantId::new(Ulid::from_u128(1));
    let stores = [
        StoreId::new(Ulid::from_u128(11)),
        StoreId::new(Ulid::from_u128(12)),
    ];
    let registry = FakeRegistry::default();
    for store in stores {
        seed_store(&registry, tenant_id, store, true);
    }
    let group_id = StoreGroupId::new(Ulid::from_u128(21));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "Airport branches".to_owned(),
            status: EntityStatus::Active,
        },
        &[],
    );
    let router = store_group_app_with_fees(
        admin,
        groups.clone(),
        registry,
        FakeConfigTrees::default(),
        FakeCatalog::default(),
        InMemoryFeeRules::new(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    (router, groups, group_id, stores)
}

/// A membership write is refused in the order it reads its request — the session, the ids, the
/// cap, each store id, the stores the tenant has, then the `If-Match` and the version — and none of
/// them records anything.
#[tokio::test]
async fn a_membership_write_is_refused_in_the_order_it_reads_its_request() {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let (router, _, group_id, [first, _]) = seeded_cohort(admin.clone(), &audit);
    // The owner logs in first: the login names no email, so it is the only admin when it does.
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let url = format!("/admin/store-groups/{}/members", group_id.as_ulid());
    let body = |ids: Vec<String>| serde_json::json!({ "tenant_id": tenant, "store_ids": ids });
    let first = first.as_ulid().to_string();
    let stranger = ulid_text(99);

    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let by_ops = router
        .clone()
        .oneshot(put_with_etag(&url, &body(vec![first.clone()]), &ops, "1"))
        .await
        .expect("route the membership");
    assert_eq!(by_ops.status(), StatusCode::FORBIDDEN);

    let too_many: Vec<String> = (0..201).map(|_| "not-a-ulid".to_owned()).collect();
    for (target, sent, etag, status, field) in [
        (
            "not-a-ulid".to_owned(),
            body(vec![first.clone()]),
            Some("1"),
            StatusCode::BAD_REQUEST,
            Some("group_id"),
        ),
        (
            url.clone(),
            body(too_many),
            None,
            StatusCode::UNPROCESSABLE_ENTITY,
            None,
        ),
        (
            url.clone(),
            body(vec![first.clone(), "not-a-ulid".to_owned()]),
            None,
            StatusCode::BAD_REQUEST,
            Some("store_ids"),
        ),
        (
            url.clone(),
            body(vec![first.clone(), stranger]),
            None,
            StatusCode::UNPROCESSABLE_ENTITY,
            None,
        ),
        (
            url.clone(),
            body(vec![first.clone()]),
            None,
            StatusCode::BAD_REQUEST,
            Some("if-match"),
        ),
        (
            url.clone(),
            body(vec![first.clone()]),
            Some("7"),
            StatusCode::PRECONDITION_FAILED,
            None,
        ),
        (
            format!("/admin/store-groups/{}/members", ulid_text(22)),
            body(vec![first.clone()]),
            Some("1"),
            StatusCode::NOT_FOUND,
            None,
        ),
    ] {
        let uri = if target.starts_with('/') {
            target.clone()
        } else {
            format!("/admin/store-groups/{target}/members")
        };
        let request = match etag {
            Some(etag) => put_with_etag(&uri, &sent, &cookie, etag),
            None => put_with_cookie(&uri, &sent, &cookie),
        };
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("route the membership");
        assert_eq!(response.status(), status, "{uri}");
        if let Some(field) = field {
            let refusal = json_body(response).await;
            assert_eq!(
                refusal["error"]["details"][0]["field"], field,
                "{uri}: {refusal}"
            );
        }
    }
    assert!(audit.entries.lock().expect("lock").is_empty());
}

/// A membership write answers the membership deduplicated, with the new version in the header and
/// the body, and records the membership it stored.
#[tokio::test]
async fn a_membership_write_answers_and_records_the_deduplicated_membership() {
    let audit = FakeAudit::default();
    let (router, groups, group_id, [first, second]) = seeded_cohort(provisioned_admin(), &audit);
    let cookie = admin_cookie(&router).await;
    let ids = [first, second, first].map(|id| id.as_ulid().to_string());
    let written = router
        .oneshot(put_with_etag(
            &format!("/admin/store-groups/{}/members", group_id.as_ulid()),
            &serde_json::json!({ "tenant_id": ulid_text(1), "store_ids": ids }),
            &cookie,
            "1",
        ))
        .await
        .expect("route the membership");
    assert_eq!(written.status(), StatusCode::OK);
    let header = written
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = json_body(written).await;
    let membership = serde_json::json!([first.as_ulid().to_string(), second.as_ulid().to_string()]);
    assert_eq!(body["store_ids"], membership);
    assert_eq!(
        body["stores"],
        serde_json::json!([]),
        "no assignment names the group"
    );
    assert_eq!(
        header.as_deref(),
        body["etag"].as_str(),
        "the header and the body agree"
    );
    assert_eq!(
        groups
            .members
            .lock()
            .expect("lock")
            .get(&(TenantId::new(Ulid::from_u128(1)), group_id))
            .cloned(),
        Some(vec![first, second])
    );

    let entries = audit.entries.lock().expect("lock").clone();
    let [entry] = entries.as_slice() else {
        panic!("one audit entry: {entries:?}");
    };
    assert_eq!(entry.action, "store_group.members");
    assert_eq!(entry.entity_type, "store_group");
    assert_eq!(entry.entity_id, group_id.as_ulid().to_string());
    assert_eq!(entry.after, Some(membership));
}

/// A batch publish records one entry naming the batch, the node, its counts and its summary.
#[tokio::test]
async fn a_batch_publish_records_the_batch_its_node_and_its_counts() {
    let tenant_id = TenantId::new(Ulid::from_u128(1));
    let ready = StoreId::new(Ulid::from_u128(13));
    let archived = StoreId::new(Ulid::from_u128(14));
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, ready, true);
    seed_store(&registry, tenant_id, archived, false);
    let config_trees = FakeConfigTrees::default();
    seed_menu_prerequisites(&config_trees, tenant_id, ready);
    let group_id = StoreGroupId::new(Ulid::from_u128(21));
    let groups = FakeStoreGroups::default();
    groups.seed(
        StoreGroup {
            group_id,
            tenant_id,
            name: "Airport branches".to_owned(),
            status: EntityStatus::Active,
        },
        &[ready, archived],
    );
    let audit = FakeAudit::default();
    let router = store_group_app_with_fees(
        provisioned_admin(),
        groups.clone(),
        registry,
        config_trees,
        FakeCatalog::default(),
        InMemoryFeeRules::new(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();
    let menu_id = author_a_menu(&router, &cookie, &tenant).await;
    let published = router
        .oneshot(post_with_cookie(
            &format!("/admin/store-groups/{}/publish", group_id.as_ulid()),
            &serde_json::json!({ "tenant_id": tenant, "node": "menu", "arguments": { "menu_id": menu_id } }),
            &cookie,
        ))
        .await
        .expect("route the batch publish");
    assert_eq!(published.status(), StatusCode::OK);
    let report = json_body(published).await;

    let batch_id = groups.batches.lock().expect("lock")[0]
        .batch_id
        .as_ulid()
        .to_string();
    assert_eq!(report["batch_id"], batch_id.as_str());
    let entries = audit.entries.lock().expect("lock").clone();
    let [entry] = entries.as_slice() else {
        panic!("one audit entry: {entries:?}");
    };
    assert_eq!(entry.action, "store_group.publish");
    assert_eq!(entry.entity_type, "store_group");
    assert_eq!(entry.entity_id, group_id.as_ulid().to_string());
    assert_eq!(entry.tenant_id, Some(tenant_id));
    let after = entry.after.clone().expect("an after");
    assert_eq!(after["batch_id"], batch_id.as_str());
    assert_eq!(after["node"], "menu");
    assert_eq!(after["audit_action"], "catalog.publish");
    assert_eq!(after["summary"], serde_json::json!({ "menu_id": menu_id }));
    assert_eq!(
        (
            after["applied"].clone(),
            after["skipped"].clone(),
            after["failed"].clone()
        ),
        (
            serde_json::json!(1),
            serde_json::json!(1),
            serde_json::json!(0)
        )
    );
}
