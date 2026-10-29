// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The generic publish preview (`/admin/config/preview`, roadmap-v3 F11), and what a preview of
//! the people nodes may show.

use axum::http::StatusCode;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::config_tree::ConfigTreeState;
use pos_cloud::people::EmployeeStore;
use pos_cloud::registry::EntityStatus;
use pos_cloud::store_groups::{StoreGroup, StoreGroupId};
use pos_cloud::version::{Version, Versioned};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeCatalog, FakeConfigTrees, FakePeople, FakeRegistry, FakeStoreGroups, admin_cookie,
    author_a_menu, json_body, people_preview_app, post_with_cookie, provisioned_admin,
    role_session_cookie, seed_a_cashier, seed_menu_prerequisites, seed_store, store_group_app,
    store_id, tenant,
};

// ---------------------------------------------------------------------------------------------
// The generic publish preview (roadmap-v3 F11)
// ---------------------------------------------------------------------------------------------

/// The preview of a node nobody has published yet is the whole node, against no version.
#[tokio::test]
async fn a_preview_of_an_unpublished_node_is_the_whole_node_against_no_version() {
    let tenant_id = TenantId::new(Ulid::from_u128(1));
    let store_id = StoreId::new(Ulid::from_u128(11));

    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store_id, true);
    let config_trees = FakeConfigTrees::default();

    let router = store_group_app(
        provisioned_admin(),
        FakeStoreGroups::default(),
        registry,
        config_trees.clone(),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();
    let menu_id = author_a_menu(&router, &cookie, &tenant).await;

    let previewed = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/preview",
            &serde_json::json!({
                "tenant_id": tenant,
                "store_id": store_id.as_ulid().to_string(),
                "node": "menu",
                "arguments": { "menu_id": menu_id },
            }),
            &cookie,
        ))
        .await
        .expect("route the preview");
    assert_eq!(previewed.status(), StatusCode::OK);
    let preview = json_body(previewed).await;

    assert_eq!(preview["node"], "menu");
    assert!(
        preview["from_version_id"].is_null(),
        "a store with no tree has no version to diff against: {preview}"
    );
    assert_eq!(preview["unchanged"], false);
    assert!(
        preview["diff"]["menu"].is_object(),
        "the diff carries the node the publish would write: {preview}"
    );

    // The point of a dry run: it left nothing behind.
    assert!(
        config_trees
            .rows
            .lock()
            .expect("lock")
            .get(&(tenant_id, store_id))
            .is_none(),
        "previewing wrote a config tree for a store that had none"
    );
}

/// The assertion the whole route exists for: the preview compiles what the publish writes, so
/// previewing the same node straight after publishing it says nothing would change.
#[tokio::test]
async fn a_preview_taken_after_the_publish_says_nothing_would_change() {
    let tenant_id = TenantId::new(Ulid::from_u128(2));
    let store_id = StoreId::new(Ulid::from_u128(21));

    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store_id, true);
    let config_trees = FakeConfigTrees::default();
    seed_menu_prerequisites(&config_trees, tenant_id, store_id);

    let group_id = StoreGroupId::new(Ulid::from_u128(31));
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
        config_trees,
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant = tenant_id.as_ulid().to_string();
    let menu_id = author_a_menu(&router, &cookie, &tenant).await;
    let body = serde_json::json!({
        "tenant_id": tenant,
        "store_id": store_id.as_ulid().to_string(),
        "node": "menu",
        "arguments": { "menu_id": menu_id.clone() },
    });

    let before = router
        .clone()
        .oneshot(post_with_cookie("/admin/config/preview", &body, &cookie))
        .await
        .expect("route the first preview");
    assert_eq!(before.status(), StatusCode::OK);
    assert_eq!(
        json_body(before).await["unchanged"],
        false,
        "the menu is not published yet, so the preview has something to show"
    );

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
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    assert_eq!(json_body(published).await["applied"], 1);

    let after = router
        .clone()
        .oneshot(post_with_cookie("/admin/config/preview", &body, &cookie))
        .await
        .expect("route the second preview");
    assert_eq!(after.status(), StatusCode::OK);
    let preview = json_body(after).await;
    assert_eq!(
        preview["unchanged"], true,
        "the preview compiled a different document from the publish it previews: {preview}"
    );
    assert_eq!(preview["diff"], serde_json::json!({}));
    assert!(
        preview["from_version_id"].is_string(),
        "the diff now has a published version to be computed against: {preview}"
    );
}

/// A node the registry does not carry is the request's own mistake, and the refusal names the ones
/// that are carried rather than leaving the caller to guess.
#[tokio::test]
async fn a_node_the_registry_does_not_carry_is_refused_by_name() {
    let tenant_id = TenantId::new(Ulid::from_u128(3));
    let store_id = StoreId::new(Ulid::from_u128(31));

    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store_id, true);

    let router = store_group_app(
        provisioned_admin(),
        FakeStoreGroups::default(),
        registry,
        FakeConfigTrees::default(),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;

    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/preview",
            &serde_json::json!({
                "tenant_id": tenant_id.as_ulid().to_string(),
                "store_id": store_id.as_ulid().to_string(),
                // Per-store by definition (ADR-0122 §4), so there is no tenant-wide document to
                // compile and no preview to give.
                "node": "locale",
            }),
            &cookie,
        ))
        .await
        .expect("route the preview");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    let message = body["error"]["message"].as_str().expect("a message");
    assert!(
        message.contains("locale") && message.contains("menu"),
        "the refusal names what was asked for and what is on offer: {message}"
    );
}

/// A preview of the `permissions` node names the store's staff only to a role holding
/// `console.people.read`, and carries a PIN hash to nobody
/// ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 9).
///
/// The preview needs only `console.config.publish`, which Ops holds, and the node it compiles is
/// the store's roster: each member's name, staff code and PIN hash. Its diff gets the config
/// reads' treatment — the credential for nobody (S7), the person only for Owner and Admin — and
/// keeps the member and what they may do.
#[tokio::test]
async fn a_permissions_preview_names_staff_only_to_a_role_that_reads_staff_records() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let (alice, _cashier) = seed_a_cashier(&people, store_id()).await;
    let router = people_preview_app(admin.clone(), people, FakeConfigTrees::default());
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "node": "permissions",
    });

    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-preview").await;
    let refused = router
        .clone()
        .oneshot(post_with_cookie("/admin/config/preview", &body, &viewer))
        .await
        .expect("route the viewer's preview");
    assert_eq!(
        refused.status(),
        StatusCode::FORBIDDEN,
        "a Viewer cannot publish, so cannot preview"
    );

    for (role, token, names_staff) in [
        (AdminRole::Ops, "ops-preview", false),
        (AdminRole::Admin, "admin-preview", true),
        (AdminRole::Owner, "owner-preview", true),
    ] {
        let cookie = role_session_cookie(&admin, role, token).await;
        let previewed = router
            .clone()
            .oneshot(post_with_cookie("/admin/config/preview", &body, &cookie))
            .await
            .expect("route the preview");
        assert_eq!(previewed.status(), StatusCode::OK, "{role:?} may publish");
        let preview = json_body(previewed).await;
        let text = preview.to_string();
        let member = preview["diff"]["permissions"]["staff"][0]
            .as_object()
            .expect("the store has no node yet, so the diff is the whole node");
        assert_eq!(
            member["id"],
            serde_json::json!(alice.as_ulid().to_string()),
            "{role:?}: the member stays"
        );
        assert_eq!(
            member["permissions"],
            serde_json::json!(["billing.discount.apply"]),
            "{role:?}: and so does what they may do"
        );
        assert!(
            !member.contains_key("pin_phc") && !text.contains("argon2id"),
            "{role:?}: never the credential (S7): {text}"
        );
        if names_staff {
            assert_eq!(
                member["name"], "Alice",
                "{role:?} holds console.people.read"
            );
            assert_eq!(member["code"], "C01", "{role:?} holds console.people.read");
        } else {
            assert!(
                !member.contains_key("name") && !member.contains_key("code"),
                "{role:?} lacks console.people.read: {member:?}"
            );
            assert!(
                !text.contains("Alice") && !text.contains("C01"),
                "{role:?}: the person is nowhere in the answer: {text}"
            );
        }
    }
}

/// A preview is redacted *after* it is computed, never before. A PIN reset changes nothing but the
/// hash, so a preview that diffed redacted documents would say nothing would change, and the
/// operator would not publish the PIN the till is waiting for. The change shows; the hash does not.
#[tokio::test]
async fn a_preview_after_a_pin_reset_still_has_something_to_publish_and_carries_no_hash() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let (alice, _cashier) = seed_a_cashier(&people, store_id()).await;
    let router = people_preview_app(admin.clone(), people.clone(), FakeConfigTrees::default());
    let owner = admin_cookie(&router).await;
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-pin-reset").await;
    let store = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
    });

    let published = router
        .clone()
        .oneshot(post_with_cookie("/admin/people/publish", &store, &owner))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    // The same roster with a new PIN: the store needs the new hash, and nothing else changed.
    EmployeeStore::set_pin(&people, tenant(), alice, "argon2id$phc$alice-reset")
        .await
        .expect("reset the pin");

    let mut body = store.clone();
    body["node"] = serde_json::json!("permissions");
    for (who, cookie, names_staff) in [("Ops", &ops, false), ("the Owner", &owner, true)] {
        let previewed = router
            .clone()
            .oneshot(post_with_cookie("/admin/config/preview", &body, cookie))
            .await
            .expect("route the preview");
        assert_eq!(previewed.status(), StatusCode::OK);
        let preview = json_body(previewed).await;
        assert_eq!(
            preview["unchanged"], false,
            "{who}: a new PIN is a change the store must receive: {preview}"
        );
        assert!(
            preview["from_version_id"].is_string(),
            "{who}: the diff is against the version just published: {preview}"
        );
        let member = preview["diff"]["permissions"]["staff"][0]
            .as_object()
            .expect("the changed staff array is in the diff, whole");
        assert!(
            !member.contains_key("pin_phc") && !preview.to_string().contains("argon2id"),
            "{who}: the new hash is not in the answer: {preview}"
        );
        assert_eq!(
            member.contains_key("name"),
            names_staff,
            "{who}: {member:?}"
        );
        assert_eq!(
            member.contains_key("code"),
            names_staff,
            "{who}: {member:?}"
        );
    }
}

/// Every preview's diff is redacted, whichever node it previews, because any of them can carry the
/// staff. A store whose layers were written without a published version — by a migration or a
/// fixture — has nothing to diff against, so the diff of any node is the whole document.
#[tokio::test]
async fn a_preview_of_another_node_carries_no_staff_credential_or_unread_identity() {
    let admin = provisioned_admin();
    let config = FakeConfigTrees::default();
    config.rows.lock().expect("lock").insert(
        (tenant(), store_id()),
        Versioned::new(
            ConfigTreeState {
                layers: [
                    serde_json::json!({}),
                    serde_json::json!({}),
                    serde_json::json!({ "permissions": { "staff": [{
                        "id": "01J",
                        "code": "C01",
                        "name": "Alice",
                        "permissions": ["billing.discount.apply"],
                        "pin_phc": "argon2id$phc$alice",
                    }] } }),
                    serde_json::json!({}),
                ],
                history: Vec::new(),
                k: 8,
            },
            Version::new("seed"),
        ),
    );
    let router = people_preview_app(admin.clone(), FakePeople::default(), config);
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-other-node").await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    for (url, body) in [
        (
            "/admin/config/preview",
            serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "node": "capabilities",
                "arguments": { "flags": { "tips_enabled": true } },
            }),
        ),
        (
            "/admin/config/campaigns/preview",
            serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
        ),
    ] {
        let previewed = router
            .clone()
            .oneshot(post_with_cookie(url, &body, &ops))
            .await
            .expect("route the preview");
        assert_eq!(previewed.status(), StatusCode::OK, "{url}");
        let preview = json_body(previewed).await;
        let member = preview["diff"]["permissions"]["staff"][0]
            .as_object()
            .expect("with no version to diff against, the roster is in the diff");
        for field in ["pin_phc", "name", "code"] {
            assert!(
                !member.contains_key(field),
                "{url}: `{field}` reached Ops: {member:?}"
            );
        }
        assert_eq!(member["id"], "01J", "{url}: the member stays");
    }
}

/// A preview withholds prices from a role without `console.reports.revenue`, as the config read
/// does (production-readiness S8).
///
/// Both previews need only `console.config.publish`, which Ops holds and `console.reports.revenue`
/// does not. A `menu` preview's diff is the store's price book, and a campaigns preview's is the
/// promotions' terms, so Ops gets neither node; Owner and Admin read revenue and get both. Whether
/// anything would change is computed before the redaction, so Ops is still told that something
/// would.
#[tokio::test]
async fn a_preview_shows_prices_only_to_a_role_that_reads_revenue() {
    let tenant_id = TenantId::new(Ulid::from_u128(4));
    let store_id = StoreId::new(Ulid::from_u128(41));
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant_id, store_id, true);
    let admin = provisioned_admin();
    let menus = store_group_app(
        admin.clone(),
        FakeStoreGroups::default(),
        registry,
        FakeConfigTrees::default(),
        FakeCatalog::default(),
    );
    let campaigns = people_preview_app(
        admin.clone(),
        FakePeople::default(),
        FakeConfigTrees::default(),
    );
    let owner = admin_cookie(&menus).await;
    let tenant = tenant_id.as_ulid().to_string();
    let store = store_id.as_ulid().to_string();
    let menu_id = author_a_menu(&menus, &owner, &tenant).await;

    for (role, token, reads_revenue) in [
        (AdminRole::Ops, "ops-prices", false),
        (AdminRole::Admin, "admin-prices", true),
        (AdminRole::Owner, "owner-prices", true),
    ] {
        let cookie = role_session_cookie(&admin, role, token).await;
        for (router, url, body, node) in [
            (
                &menus,
                "/admin/config/preview",
                serde_json::json!({
                    "tenant_id": tenant,
                    "store_id": store,
                    "node": "menu",
                    "arguments": { "menu_id": menu_id },
                }),
                "menu",
            ),
            (
                &campaigns,
                "/admin/config/campaigns/preview",
                serde_json::json!({ "tenant_id": tenant, "store_id": store }),
                "campaigns",
            ),
        ] {
            let previewed = router
                .clone()
                .oneshot(post_with_cookie(url, &body, &cookie))
                .await
                .expect("route the preview");
            assert_eq!(
                previewed.status(),
                StatusCode::OK,
                "{role:?} may publish: {url}"
            );
            let preview = json_body(previewed).await;
            assert_eq!(
                preview["diff"].get(node).is_some(),
                reads_revenue,
                "{role:?} and the `{node}` node: {preview}"
            );
            assert_eq!(
                preview["unchanged"], false,
                "{role:?}: a store never published to has something to publish: {preview}"
            );
        }
    }
}
