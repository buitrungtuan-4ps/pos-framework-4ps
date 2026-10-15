// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The org registry (ADR-0065): its audit trail, the roles a new tenant starts with, and tenants and
//! stores created and renamed by name.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditRecorder, AuditSink, AuditStore, NoopAuditRecorder};
use pos_cloud::people::{RoleTemplate, RoleTemplateStore, RoleTemplateUpdate};
use pos_cloud::registry::EntityStatus;
use pos_cloud::starting_roles::{seed_starting_roles, starting_role_grants, starting_role_name};
use pos_cloud::version::Versioned;
use pos_core::permission::Role;
use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakePeople, FakeRegistry, admin_cookie, create_tenant, get_with_cookie, json_body,
    patch_with_etag, post_with_cookie, provisioned_admin, registry_app, registry_app_over_people,
    registry_app_with_audit,
};

#[tokio::test]
async fn registry_writes_record_to_the_audit_trail() {
    let audit = FakeAudit::default();
    let sink: Arc<dyn AuditRecorder> = Arc::new(AuditSink::new(audit.clone()));
    let router = registry_app_with_audit(provisioned_admin(), FakeRegistry::default(), sink);
    let cookie = admin_cookie(&router).await;

    // A tenant create mints its id server-side and, on success, records one audit entry.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Pizza 4P's" }),
            &cookie,
        ))
        .await
        .expect("route create tenant");
    assert_eq!(created.status(), StatusCode::CREATED);
    let tenant_id = json_body(created).await["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();

    // A store create under it records a second entry, scoped to the tenant.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/stores",
            &serde_json::json!({ "tenant_id": tenant_id, "name": "Bến Thành" }),
            &cookie,
        ))
        .await
        .expect("route create store");
    assert_eq!(created.status(), StatusCode::CREATED);
    let store_id = json_body(created).await["store_id"]
        .as_str()
        .expect("a store id")
        .to_owned();

    let recorded = audit.list(None, 10).await.expect("list audit entries");
    assert_eq!(
        recorded.len(),
        3,
        "each successful write records one entry, and a new tenant's starting roles one more"
    );
    let store_entry = recorded
        .iter()
        .find(|entry| entry.action == "store.create")
        .expect("the store create was recorded");
    assert_eq!(store_entry.entity_type, "store");
    assert_eq!(
        store_entry.entity_id, store_id,
        "the new entity's id is recorded"
    );
    assert_eq!(
        store_entry.tenant_id.map(|id| id.to_string()),
        Some(tenant_id.clone()),
        "the entry is scoped to the owning tenant"
    );
    assert!(store_entry.before.is_none(), "a create has no prior value");
    assert!(
        store_entry.after.is_some(),
        "a create records the new value"
    );
    assert!(
        !store_entry.actor.email.is_empty() && !store_entry.actor.admin_id.is_empty(),
        "the acting admin is snapshotted onto the entry"
    );
    assert!(
        recorded.iter().any(|entry| entry.action == "tenant.create"),
        "the tenant create was recorded too"
    );
}

/// The roles a people store holds for a tenant.
async fn roles_of(people: &FakePeople, tenant: TenantId) -> Vec<Versioned<RoleTemplate>> {
    RoleTemplateStore::list(people, tenant)
        .await
        .expect("list roles")
}

/// A tenant the console creates starts with six roles, one for each of `pos-core`'s built-in roles,
/// named in the language it was created in and granting what the catalogue's defaults say
/// ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)): a default
/// role holds a permission directly, and every other role holds a PIN-flagged one with approval.
/// The seeding is audited by ids and counts, never a name.
#[tokio::test]
async fn a_new_tenant_starts_with_six_roles_granting_the_catalogue_defaults() {
    let audit = FakeAudit::default();
    let people = FakePeople::default();
    let router = registry_app_over_people(
        provisioned_admin(),
        FakeRegistry::default(),
        people.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let body = serde_json::json!({ "name": "Pizza 4P's", "language": "vi" });
    let tenant_id = create_tenant(&router, &cookie, &body).await;

    let roles = roles_of(&people, tenant_id).await;
    assert_eq!(roles.len(), Role::ALL.len(), "one role per built-in role");
    for role in Role::ALL {
        let name = starting_role_name(*role, Some("vi"));
        let seeded = roles
            .iter()
            .find(|seeded| seeded.record.name == name)
            .unwrap_or_else(|| panic!("{role:?} is seeded as {name}"));
        let (directly, with_approval) = starting_role_grants(*role);
        assert_eq!(seeded.record.permissions, directly, "{role:?} directly");
        assert_eq!(
            seeded.record.permissions_with_approval, with_approval,
            "{role:?} with approval"
        );
        assert_eq!(seeded.record.discount_ceiling_minor, None);
        assert_eq!(seeded.record.status, EntityStatus::Active);
    }
    let cook = starting_role_name(Role::Cook, Some("vi"));
    let cook = roles
        .iter()
        .find(|seeded| seeded.record.name == cook)
        .expect("the cook");
    assert!(
        cook.record
            .permissions_with_approval
            .contains(&"billing.bill.void".to_owned()),
        "a role the catalogue does not default to a PIN-flagged act holds it with approval"
    );

    let recorded = audit.list(None, 10).await.expect("list audit entries");
    let seed = recorded
        .iter()
        .find(|entry| entry.action == "role.seed")
        .expect("the seeding is audited");
    let after = seed.after.as_ref().expect("an after");
    assert_eq!(after["created"], 6);
    assert_eq!(after["kept"], 0);
    assert_eq!(after["role_template_ids"].as_array().map(Vec::len), Some(6));
    assert_eq!(seed.entity_id, tenant_id.to_string());
    let trail = serde_json::to_string(after).expect("serialise");
    assert!(
        !trail.contains(&cook.record.name),
        "ids and counts, never a name: {trail}"
    );
}

/// Seeding a tenant again writes nothing: each starting role's id is derived from the tenant's,
/// so the six are found, and a role the owner has since renamed keeps its new name (ADR-0158).
#[tokio::test]
async fn seeding_a_tenant_again_writes_nothing_and_keeps_a_rename() {
    let people = FakePeople::default();
    let router = registry_app_over_people(
        provisioned_admin(),
        FakeRegistry::default(),
        people.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let body = serde_json::json!({ "name": "Pizza 4P's", "language": "vi" });
    let tenant_id = create_tenant(&router, &cookie, &body).await;

    let cashier_name = starting_role_name(Role::Cashier, Some("vi"));
    let roles = roles_of(&people, tenant_id).await;
    let cashier = roles
        .iter()
        .find(|seeded| seeded.record.name == cashier_name)
        .expect("the cashier");
    let rename = RoleTemplateUpdate {
        role_template_id: cashier.record.role_template_id,
        tenant_id,
        name: "Front of house".to_owned(),
        permissions: cashier.record.permissions.clone(),
        permissions_with_approval: cashier.record.permissions_with_approval.clone(),
        discount_ceiling_minor: None,
        status: EntityStatus::Active,
    };
    RoleTemplateStore::update(&people, &rename, &cashier.etag)
        .await
        .expect("rename");

    let again = seed_starting_roles(&people, tenant_id, Some("vi"))
        .await
        .expect("seed again");
    assert!(again.created.is_empty(), "nothing is written twice");
    assert_eq!(again.kept.len(), 6);
    let after = roles_of(&people, tenant_id).await;
    assert_eq!(after.len(), 6);
    assert!(
        after
            .iter()
            .any(|seeded| seeded.record.name == "Front of house"),
        "the rename stands"
    );
}

/// Without a language, or with one the console's packs do not carry, the starting roles are named
/// in English; and a tenant created later leaves an earlier one's roles alone (ADR-0158).
#[tokio::test]
async fn a_tenant_created_without_a_language_names_its_roles_in_english() {
    let people = FakePeople::default();
    let router = registry_app_over_people(
        provisioned_admin(),
        FakeRegistry::default(),
        people.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let first = serde_json::json!({ "name": "Pizza 4P's", "language": "vi" });
    let first = create_tenant(&router, &cookie, &first).await;

    for body in [
        serde_json::json!({ "name": "Pizza 4P's Japan" }),
        serde_json::json!({ "name": "Pizza 4P's India", "language": "hi" }),
    ] {
        let tenant_id = create_tenant(&router, &cookie, &body).await;
        let names: Vec<String> = roles_of(&people, tenant_id)
            .await
            .into_iter()
            .map(|seeded| seeded.record.name)
            .collect();
        for role in Role::ALL {
            assert!(
                names.contains(&starting_role_name(*role, None)),
                "{body}: {names:?}"
            );
        }
    }
    assert_eq!(
        roles_of(&people, first).await.len(),
        6,
        "the first tenant is untouched by the others"
    );
}

#[tokio::test]
async fn registry_creates_and_lists_named_tenant_and_store_without_typing_a_ulid() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&router).await;

    // Create a tenant by name; the id is minted server-side and returned once.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Pizza 4P's" }),
            &cookie,
        ))
        .await
        .expect("route create tenant");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["name"], "Pizza 4P's");
    assert_eq!(created["status"], "active");
    let tenant_id = created["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();

    // It shows in the listing the picker reads.
    let listed = router
        .clone()
        .oneshot(get_with_cookie("/admin/tenants", &cookie))
        .await
        .expect("route list tenants");
    assert_eq!(listed.status(), StatusCode::OK);
    let tenants = json_body(listed).await;
    assert_eq!(tenants.as_array().expect("array").len(), 1);
    assert_eq!(tenants[0]["tenant_id"], tenant_id);

    // Create a store under it — again, no ULID typed by the operator.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/stores",
            &serde_json::json!({ "tenant_id": tenant_id, "name": "Bến Thành" }),
            &cookie,
        ))
        .await
        .expect("route create store");
    assert_eq!(created.status(), StatusCode::CREATED);
    let store = json_body(created).await;
    assert_eq!(store["name"], "Bến Thành");
    assert_eq!(store["brand_id"], serde_json::Value::Null);
    let store_id = store["store_id"].as_str().expect("a store id").to_owned();

    // And it lists for its tenant.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/stores?tenant_id={tenant_id}"),
            &cookie,
        ))
        .await
        .expect("route list stores");
    let stores = json_body(listed).await;
    assert_eq!(stores.as_array().expect("array").len(), 1);
    assert_eq!(stores[0]["store_id"], store_id);
}

#[tokio::test]
async fn registry_renames_a_tenant_and_404s_an_unknown_one() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let cookie = admin_cookie(&router).await;

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/tenants",
            &serde_json::json!({ "name": "Placeholder" }),
            &cookie,
        ))
        .await
        .expect("route create");
    let created_body = json_body(created).await;
    let tenant_id = created_body["tenant_id"]
        .as_str()
        .expect("a tenant id")
        .to_owned();
    // A create hands back the version it starts at, so an edit needs no second round trip
    // (ADR-0094).
    let etag = created_body["etag"].as_str().expect("an etag").to_owned();

    // Rename it, naming the version being replaced.
    let renamed = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/tenants/{tenant_id}"),
            &serde_json::json!({ "name": "Pizza 4P's", "status": "active" }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename");
    assert_eq!(renamed.status(), StatusCode::OK);
    let renamed_body = json_body(renamed).await;
    assert_eq!(renamed_body["name"], "Pizza 4P's");
    let next_etag = renamed_body["etag"].as_str().expect("a new etag");
    assert_ne!(
        next_etag, etag,
        "an applied update moves the version, or the next write would be unguarded"
    );

    // Renaming an unknown tenant is a 404, not a silent success.
    let missing = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/tenants/{}", Ulid::from_u128(9_999)),
            &serde_json::json!({ "name": "Nope", "status": "active" }),
            &cookie,
            &etag,
        ))
        .await
        .expect("route rename missing");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
