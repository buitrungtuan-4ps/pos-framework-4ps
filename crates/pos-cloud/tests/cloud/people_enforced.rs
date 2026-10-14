// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the console authors is what the store enforces (ADR-0158 decision 7): a removal, an archive
//! or a role change reaches every store it affects, and what a role grants with approval.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditSink, AuditStore, NoopAuditRecorder};
use pos_cloud::people::{
    AssignmentId, AssignmentScope, AssignmentStore, EmployeeId, EmployeeStore, EmployeeUpdate,
    NewAssignment, NewEmployee, NewRoleTemplate, RoleTemplateId, RoleTemplateStore,
    RoleTemplateUpdate,
};
use pos_cloud::registry::EntityStatus;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeConfigTrees, FakePeople, FakeRegistry, admin_cookie, applied, assignment_body,
    delete_with_cookie, etag_of, get_with_cookie, json_body, patch_with_etag, people_app,
    people_app_with_audit, post_assignment, post_with_cookie, provisioned_admin, publish_people,
    publish_report, seed_a_cashier, seed_store, staff_at, store_id, tenant, versions_at,
};

/// Seeds, through the seam, a person and a role that existed and have since been archived.
async fn seed_archived_person_and_role(people: &FakePeople) -> (EmployeeId, RoleTemplateId) {
    let gone = EmployeeId::new(Ulid::from_u128(0x60E));
    let version = EmployeeStore::create(
        people,
        &NewEmployee {
            employee_id: gone,
            tenant_id: tenant(),
            code: "C09".to_owned(),
            name: "Gone".to_owned(),
        },
    )
    .await
    .expect("create employee");
    let archive = EmployeeUpdate {
        employee_id: gone,
        tenant_id: tenant(),
        name: "Gone".to_owned(),
        status: EntityStatus::Archived,
    };
    EmployeeStore::update(people, &archive, &version)
        .await
        .expect("archive employee");
    let retired = RoleTemplateId::new(Ulid::from_u128(0x01D));
    let version = RoleTemplateStore::create(
        people,
        &NewRoleTemplate {
            role_template_id: retired,
            tenant_id: tenant(),
            name: "Old".to_owned(),
            permissions: Vec::new(),
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor: None,
        },
    )
    .await
    .expect("create role");
    let retire = RoleTemplateUpdate {
        role_template_id: retired,
        tenant_id: tenant(),
        name: "Old".to_owned(),
        permissions: Vec::new(),
        permissions_with_approval: Vec::new(),
        discount_ceiling_minor: None,
        status: EntityStatus::Archived,
    };
    RoleTemplateStore::update(people, &retire, &version)
        .await
        .expect("archive role");
    (gone, retired)
}

/// The assignment route checks every id it binds before writing
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 7,
/// and ADR-0070's route check). An employee, store or role the tenant does not have is `404`, one
/// it has archived is `409`, and each refusal names the field that carried the id.
#[tokio::test]
async fn an_assignment_to_an_unknown_or_archived_person_store_or_role_is_refused() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let (alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let (gone, retired) = seed_archived_person_and_role(&people).await;
    let registry = FakeRegistry::default();
    let open = StoreId::new(Ulid::from_u128(0x0BE));
    let closed = StoreId::new(Ulid::from_u128(0xC1));
    seed_store(&registry, tenant(), open, true);
    seed_store(&registry, tenant(), closed, false);
    let router = people_app(
        admin,
        people.clone(),
        registry,
        FakeConfigTrees::default(),
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;
    // Every id good: Alice, at a second store that is open, as a Cashier.
    let good = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "employee_id": alice.as_ulid().to_string(),
        "store_id": open.as_ulid().to_string(),
        "role_template_id": cashier.as_ulid().to_string(),
    });

    // Each field in turn names something the tenant never had, then something it has archived.
    let nobody = Ulid::from_u128(0xDEAD).to_string();
    for (field, archived) in [
        ("employee_id", gone.as_ulid().to_string()),
        ("store_id", closed.as_ulid().to_string()),
        ("role_template_id", retired.as_ulid().to_string()),
    ] {
        for (id, status, reason) in [
            (&nobody, StatusCode::NOT_FOUND, "NOT_FOUND"),
            (&archived, StatusCode::CONFLICT, "ARCHIVED"),
        ] {
            let mut request = good.clone();
            request[field] = serde_json::json!(id);
            let refused = router
                .clone()
                .oneshot(post_with_cookie("/admin/assignments", &request, &owner))
                .await
                .expect("route the assignment");
            assert_eq!(refused.status(), status, "{field} {reason}");
            let error = json_body(refused).await;
            let detail = &error["error"]["details"][0];
            assert_eq!(detail["field"], field, "{error}");
            assert_eq!(detail["reason"], reason, "{error}");
        }
    }
    let held = AssignmentStore::list_for_employee(&people, tenant(), alice)
        .await
        .expect("list");
    assert_eq!(
        held.len(),
        1,
        "no refusal wrote anything: Alice holds only her seeded grant"
    );

    let assigned = router
        .clone()
        .oneshot(post_with_cookie("/admin/assignments", &good, &owner))
        .await
        .expect("route the assignment");
    assert_eq!(
        assigned.status(),
        StatusCode::CREATED,
        "every id good, so it is written"
    );
}

/// Removing an assignment publishes its store's `permissions` node at once, so the person cannot
/// sign in there a moment longer (ADR-0158 decision 7). The answer says how the store's publish
/// went; the trail records the removal by the ids it bound and the publish by its version and
/// count.
#[tokio::test]
async fn removing_an_assignment_takes_the_person_off_the_stores_node_at_once() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let (alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let router = people_app(
        admin,
        people,
        FakeRegistry::default(),
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let owner = admin_cookie(&router).await;
    publish_people(&router, &owner, store_id()).await;
    assert_eq!(
        staff_at(&config, store_id()).await.len(),
        1,
        "Alice is on the node"
    );

    let grant = AssignmentId::new(Ulid::from_u128(1));
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!(
                "/admin/assignments/{}?tenant_id={}",
                grant.as_ulid(),
                tenant().as_ulid()
            ),
            &owner,
        ))
        .await
        .expect("route the removal");
    assert_eq!(removed.status(), StatusCode::OK);
    assert_eq!(publish_report(removed).await, vec![applied(store_id())]);
    assert!(
        staff_at(&config, store_id()).await.is_empty(),
        "the store's node no longer lists her, without anyone pressing publish"
    );

    let recorded = audit.list(None, 20).await.expect("list audit");
    let removal = recorded
        .iter()
        .find(|entry| entry.action == "assignment.remove")
        .expect("the removal is recorded");
    assert_eq!(
        removal.before,
        Some(serde_json::json!({
            "id": grant.to_string(),
            "employee_id": alice.to_string(),
            "scope_kind": "ASSIGNMENT_SCOPE_STORE",
            "store_id": store_id().to_string(),
            "role_template_id": cashier.to_string(),
        })),
        "the ids it bound, and no name or code"
    );
    let caused = recorded
        .iter()
        .find(|entry| {
            entry.action == "permissions.publish"
                && entry
                    .after
                    .as_ref()
                    .is_some_and(|after| after.get("assignment_id").is_some())
        })
        .expect("the publish the removal caused is recorded");
    assert_eq!(caused.entity_id, store_id().to_string());
    let after = caused.after.as_ref().expect("an after");
    assert_eq!(after["assignment_id"], serde_json::json!(grant.to_string()));
    assert_eq!(after["staff_count"], 0);
    assert!(
        !serde_json::to_string(after)
            .expect("serialise")
            .contains("Alice"),
        "never a name"
    );
}

/// Archiving a person publishes the node at once to every store they work at; a rename publishes as
/// before, which is to say not until someone presses publish (ADR-0158 decision 7).
#[tokio::test]
async fn archiving_a_person_tells_every_store_they_work_at_and_a_rename_tells_none() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let (alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let second = StoreId::new(Ulid::from_u128(0x5EC));
    AssignmentStore::assign(
        &people,
        &NewAssignment {
            assignment_id: AssignmentId::new(Ulid::from_u128(2)),
            tenant_id: tenant(),
            employee_id: alice,
            scope: AssignmentScope::Store(second),
            role_template_id: cashier,
        },
    )
    .await
    .expect("assign the second store");
    let router = people_app(
        admin,
        people,
        FakeRegistry::default(),
        config.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;
    for store in [store_id(), second] {
        publish_people(&router, &owner, store).await;
    }
    let tenant_ulid = tenant().as_ulid().to_string();
    let url = format!("/admin/employees/{}", alice.as_ulid());

    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{url}?tenant_id={tenant_ulid}"),
            &owner,
        ))
        .await
        .expect("route the read");
    let renamed = router
        .clone()
        .oneshot(patch_with_etag(
            &url,
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Alice Nguyen", "status": "active" }),
            &owner,
            &etag_of(&read),
        ))
        .await
        .expect("route the rename");
    assert_eq!(
        renamed.status(),
        StatusCode::NO_CONTENT,
        "a rename answers as before"
    );
    for store in [store_id(), second] {
        assert_eq!(
            versions_at(&config, store).await,
            1,
            "and publishes nothing"
        );
    }

    let archived = router
        .clone()
        .oneshot(patch_with_etag(
            &url,
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Alice Nguyen", "status": "archived" }),
            &owner,
            &etag_of(&renamed),
        ))
        .await
        .expect("route the archive");
    assert_eq!(archived.status(), StatusCode::OK);
    assert!(
        !etag_of(&archived).is_empty(),
        "the person's new version still comes back"
    );
    let mut expected = vec![applied(store_id()), applied(second)];
    expected.sort();
    assert_eq!(
        publish_report(archived).await,
        expected,
        "both of her stores, at once"
    );
    for store in [store_id(), second] {
        assert!(
            staff_at(&config, store).await.is_empty(),
            "she is off {store}'s node"
        );
        assert_eq!(versions_at(&config, store).await, 2);
    }
}

/// Archiving a role publishes the node at once to every store where someone holds it, and there it
/// grants nothing: the person stays on the roster through the assignment and holds what their other
/// roles grant, here nothing (ADR-0158 decision 7). A store where nobody holds the role is left
/// alone.
#[tokio::test]
async fn archiving_a_role_takes_what_it_granted_off_every_store_that_held_it() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let (alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let elsewhere = StoreId::new(Ulid::from_u128(0x5EC));
    let cook = RoleTemplateId::new(Ulid::from_u128(0xC00C));
    RoleTemplateStore::create(
        &people,
        &NewRoleTemplate {
            role_template_id: cook,
            tenant_id: tenant(),
            name: "Cook".to_owned(),
            permissions: vec!["sales.ticket.bump".to_owned()],
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor: None,
        },
    )
    .await
    .expect("create role");
    AssignmentStore::assign(
        &people,
        &NewAssignment {
            assignment_id: AssignmentId::new(Ulid::from_u128(2)),
            tenant_id: tenant(),
            employee_id: alice,
            scope: AssignmentScope::Store(elsewhere),
            role_template_id: cook,
        },
    )
    .await
    .expect("assign");
    let router = people_app(
        admin,
        people,
        FakeRegistry::default(),
        config.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;
    for store in [store_id(), elsewhere] {
        publish_people(&router, &owner, store).await;
    }
    let tenant_ulid = tenant().as_ulid().to_string();
    let url = format!("/admin/roles/{}", cashier.as_ulid());
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{url}?tenant_id={tenant_ulid}"),
            &owner,
        ))
        .await
        .expect("route the read");

    let archived = router
        .clone()
        .oneshot(patch_with_etag(
            &url,
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "name": "Cashier",
                "permissions": ["billing.discount.apply"],
                "status": "archived",
            }),
            &owner,
            &etag_of(&read),
        ))
        .await
        .expect("route the archive");
    assert_eq!(archived.status(), StatusCode::OK);
    assert_eq!(
        publish_report(archived).await,
        vec![applied(store_id())],
        "only the store where someone holds the role"
    );
    let staff = staff_at(&config, store_id()).await;
    assert_eq!(
        staff.len(),
        1,
        "Alice stays on the roster through her assignment"
    );
    assert_eq!(
        staff[0]["permissions"],
        serde_json::json!([]),
        "but the archived role grants her nothing"
    );
    assert_eq!(
        versions_at(&config, elsewhere).await,
        1,
        "the other store was left alone"
    );
    assert_eq!(
        staff_at(&config, elsewhere).await[0]["permissions"],
        serde_json::json!(["sales.ticket.bump"])
    );
}

/// A person with a store's role and a tenant-wide one holds, at that store, the union of their
/// permissions and the highest of their ceilings (ADR-0158 decision 3). The tenant-wide grant is
/// made through the route, which publishes it to the store at once.
#[tokio::test]
async fn a_person_with_a_stores_role_and_a_tenant_wide_role_holds_both_there() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let (alice, _cashier) = seed_a_cashier(&people, store_id()).await;
    let lead = RoleTemplateId::new(Ulid::from_u128(0x1EAD));
    RoleTemplateStore::create(
        &people,
        &NewRoleTemplate {
            role_template_id: lead,
            tenant_id: tenant(),
            name: "Shift lead".to_owned(),
            permissions: vec![
                "cash.shift.open".to_owned(),
                "billing.discount.apply".to_owned(),
            ],
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor: Some(50_000),
        },
    )
    .await
    .expect("create role");
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant(), store_id(), true);
    let router = people_app(
        provisioned_admin(),
        people,
        registry,
        config.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;
    let everywhere = [("scope_kind", "ASSIGNMENT_SCOPE_TENANT".to_owned())];
    let created =
        post_assignment(&router, &owner, &assignment_body(alice, lead, &everywhere)).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(publish_report(created).await, vec![applied(store_id())]);

    let staff = staff_at(&config, store_id()).await;
    assert_eq!(staff.len(), 1, "one person, listed once");
    assert_eq!(
        staff[0]["permissions"],
        serde_json::json!(["billing.discount.apply", "cash.shift.open"]),
        "both roles, once each"
    );
    assert_eq!(
        staff[0]["discount_ceiling_minor"], 50_000,
        "the higher ceiling"
    );
}

/// A role grants each permission directly or with approval
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 4), and a create says which in two lists. One that leaves `permissions_with_approval`
/// out grants every permission it lists directly, as before the list existed, and reads back as
/// it was sent, PIN-flagged ids included: only a direct holder approves, so a script or an old
/// console tab must not create roles whose holders can no longer approve. The console's role
/// editor, which always sends both lists, is where a PIN-flagged permission starts with approval.
/// The audit entry records both lists apart.
#[tokio::test]
async fn a_new_role_grants_with_approval_only_what_it_says() {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let router = people_app_with_audit(
        admin,
        FakePeople::default(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let owner = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    for (body, directly, with_approval) in [
        (
            serde_json::json!({
                "tenant_id": tenant_ulid,
                "name": "Shift lead",
                "permissions": ["billing.bill.void", "sales.line.add", "cash.drawer.open_no_sale"],
            }),
            serde_json::json!([
                "billing.bill.void",
                "sales.line.add",
                "cash.drawer.open_no_sale"
            ]),
            serde_json::json!([]),
        ),
        (
            serde_json::json!({
                "tenant_id": tenant_ulid,
                "name": "Manager",
                "permissions": ["billing.bill.void", "sales.line.add"],
                "permissions_with_approval": [],
            }),
            serde_json::json!(["billing.bill.void", "sales.line.add"]),
            serde_json::json!([]),
        ),
        (
            serde_json::json!({
                "tenant_id": tenant_ulid,
                "name": "Server",
                "permissions": ["sales.line.add"],
                "permissions_with_approval": ["sales.line.void_fired"],
            }),
            serde_json::json!(["sales.line.add"]),
            serde_json::json!(["sales.line.void_fired"]),
        ),
    ] {
        let created = router
            .clone()
            .oneshot(post_with_cookie("/admin/roles", &body, &owner))
            .await
            .expect("route the create");
        assert_eq!(created.status(), StatusCode::CREATED, "{body}");
        let id = json_body(created).await["id"]
            .as_str()
            .expect("an id")
            .to_owned();
        let read = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/roles/{id}?tenant_id={tenant_ulid}"),
                &owner,
            ))
            .await
            .expect("route the read");
        let role = json_body(read).await;
        assert_eq!(role["permissions"], directly, "granted directly: {body}");
        assert_eq!(
            role["permissions_with_approval"], with_approval,
            "granted with approval: {body}"
        );
    }

    let recorded = audit.list(None, 50).await.expect("list audit");
    let after = recorded
        .iter()
        .filter(|entry| entry.action == "role.create")
        .filter_map(|entry| entry.after.as_ref())
        .find(|after| after["name"] == "Server")
        .expect("the create is audited");
    assert_eq!(after["permissions"], serde_json::json!(["sales.line.add"]));
    assert_eq!(
        after["permissions_with_approval"],
        serde_json::json!(["sales.line.void_fired"])
    );
}

/// What a role grants with approval must be catalogue permissions, each PIN-flagged and none also
/// granted directly (ADR-0158 decision 4). A create or an update that lists otherwise is refused
/// `400`, naming the field and why.
#[tokio::test]
async fn a_role_write_refuses_an_approval_that_is_unknown_unflagged_or_also_direct() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let router = people_app_with_audit(admin, people, Arc::new(NoopAuditRecorder));
    let owner = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let url = format!("/admin/roles/{}", cashier.as_ulid());
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{url}?tenant_id={tenant_ulid}"),
            &owner,
        ))
        .await
        .expect("route the read");
    let etag = etag_of(&read);

    for (directly, with_approval, refused_fields, reason) in [
        (
            serde_json::json!([]),
            serde_json::json!(["not.a.real.permission"]),
            vec!["permissions_with_approval"],
            "INVALID_ENUM_VALUE",
        ),
        (
            serde_json::json!([]),
            serde_json::json!(["sales.line.add"]),
            vec!["permissions_with_approval"],
            "INVALID_VALUE",
        ),
        (
            serde_json::json!(["billing.bill.void"]),
            serde_json::json!(["billing.bill.void"]),
            vec!["permissions", "permissions_with_approval"],
            "MUTUALLY_EXCLUSIVE",
        ),
    ] {
        let body = serde_json::json!({
            "tenant_id": tenant_ulid,
            "name": "Cashier",
            "permissions": directly,
            "permissions_with_approval": with_approval,
            "status": "active",
        });
        for request in [
            post_with_cookie("/admin/roles", &body, &owner),
            patch_with_etag(&url, &body, &owner, &etag),
        ] {
            let refused = router
                .clone()
                .oneshot(request)
                .await
                .expect("route the write");
            assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{body}");
            let error = json_body(refused).await;
            let named: Vec<(&str, &str)> = error["error"]["details"]
                .as_array()
                .expect("details")
                .iter()
                .map(|detail| {
                    (
                        detail["field"].as_str().expect("a field"),
                        detail["reason"].as_str().expect("a reason"),
                    )
                })
                .collect();
            let expected: Vec<(&str, &str)> = refused_fields
                .iter()
                .map(|field| (*field, reason))
                .collect();
            assert_eq!(named, expected, "{error}");
        }
    }
}

/// An update written before roles said what they grant with approval keeps working (ADR-0158
/// decision 4). Leaving `permissions_with_approval` out keeps what the role grants with approval,
/// less anything the update now grants directly, and the store's node lists each person's two
/// lists apart.
#[tokio::test]
async fn an_update_that_leaves_approval_out_keeps_it_less_what_it_now_grants_directly() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    // The Cashier as migration 0074 leaves one: the direct list as it was, and two acts with approval.
    people.roles.rows.lock().expect("lock")[0]
        .record
        .permissions_with_approval = vec![
        "billing.bill.void".to_owned(),
        "sales.line.void_fired".to_owned(),
    ];
    let router = people_app(
        admin,
        people,
        FakeRegistry::default(),
        config.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let url = format!("/admin/roles/{}", cashier.as_ulid());
    let read_url = format!("{url}?tenant_id={tenant_ulid}");

    // A console from before the list: the void is now granted directly, and nothing else is said.
    let before = router
        .clone()
        .oneshot(get_with_cookie(&read_url, &owner))
        .await
        .expect("route the read");
    let updated = router
        .clone()
        .oneshot(patch_with_etag(
            &url,
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "name": "Cashier",
                "permissions": ["billing.discount.apply", "sales.line.void_fired"],
                "status": "active",
            }),
            &owner,
            &etag_of(&before),
        ))
        .await
        .expect("route the update");
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);
    let after = router
        .clone()
        .oneshot(get_with_cookie(&read_url, &owner))
        .await
        .expect("route the read");
    let after_etag = etag_of(&after);
    let role = json_body(after).await;
    assert_eq!(
        role["permissions_with_approval"],
        serde_json::json!(["billing.bill.void"]),
        "kept, less the void it now grants directly: {role}"
    );
    publish_people(&router, &owner, store_id()).await;
    let staff = staff_at(&config, store_id()).await;
    assert_eq!(
        staff[0]["permissions"],
        serde_json::json!(["billing.discount.apply", "sales.line.void_fired"])
    );
    assert_eq!(
        staff[0]["permissions_with_approval"],
        serde_json::json!(["billing.bill.void"])
    );

    // Saying `[]` is saying something: nothing with approval.
    let cleared = router
        .clone()
        .oneshot(patch_with_etag(
            &url,
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "name": "Cashier",
                "permissions": ["billing.discount.apply"],
                "permissions_with_approval": [],
                "status": "active",
            }),
            &owner,
            &after_etag,
        ))
        .await
        .expect("route the update");
    assert_eq!(cleared.status(), StatusCode::NO_CONTENT);
    let cleared = router
        .clone()
        .oneshot(get_with_cookie(&read_url, &owner))
        .await
        .expect("route the read");
    let role = json_body(cleared).await;
    assert_eq!(role["permissions_with_approval"], serde_json::json!([]));
}
