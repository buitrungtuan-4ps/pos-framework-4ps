// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Where an assignment applies (ADR-0158 decision 3): a store, a store group or every store; and
//! what each role at a store lacks before the store enforces it (the ADR-0158 rollout).

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditRecorder, AuditSink, AuditStore, NoopAuditRecorder};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::people::{
    AssignmentId, AssignmentScope, AssignmentStore, EmployeeId, EmployeeStore, NewAssignment,
    NewEmployee, NewRoleTemplate, RoleTemplateId, RoleTemplateStore,
};
use pos_cloud::registry::EntityStatus;
use pos_cloud::store_groups::{StoreGroup, StoreGroupId, StoreGroupStore};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeAudit, FakeConfigTrees, FakeKeys, FakePeople, FakeRegistry, FakeRollups,
    FakeSettings, FakeWebhooks, NOW_MS, admin_cookie, app_all, applied, assignment_body, clock,
    delete_with_cookie, etag_of, get_with_cookie, json_body, patch_with_etag, people_app,
    post_assignment, provisioned_admin, publish_people, publish_report, put_with_etag,
    role_session_cookie, seed_a_cashier, seed_store, staff_at, store_id, tenant, versions_at,
};

// --- ADR-0158 decision 3: an assignment reaches a store, a store group or every store -----------

/// A store these tests assign people through, numbered so the reports' store order is the order
/// the tests list them in.
fn shop(n: u128) -> StoreId {
    StoreId::new(Ulid::from_u128(0x4000 + n))
}

/// Seeds a person through the seam and returns their id.
async fn seed_person(people: &FakePeople, n: u128, code: &str, name: &str) -> EmployeeId {
    let employee_id = EmployeeId::new(Ulid::from_u128(n));
    EmployeeStore::create(
        people,
        &NewEmployee {
            employee_id,
            tenant_id: tenant(),
            code: code.to_owned(),
            name: name.to_owned(),
        },
    )
    .await
    .expect("create employee");
    employee_id
}

/// Seeds an active store group holding `members`, in the groups the roster's group assignments
/// reach through, and returns its id.
fn seed_group(people: &FakePeople, n: u128, members: &[StoreId]) -> StoreGroupId {
    let group_id = StoreGroupId::new(Ulid::from_u128(n));
    people.groups().seed(
        StoreGroup {
            group_id,
            tenant_id: tenant(),
            name: format!("Group {n}"),
            status: EntityStatus::Active,
        },
        members,
    );
    group_id
}

/// Seeds an assignment through the seam, as one written earlier.
async fn seed_assignment(
    people: &FakePeople,
    n: u128,
    employee_id: EmployeeId,
    scope: AssignmentScope,
    role_template_id: RoleTemplateId,
) -> AssignmentId {
    let assignment_id = AssignmentId::new(Ulid::from_u128(n));
    AssignmentStore::assign(
        people,
        &NewAssignment {
            assignment_id,
            tenant_id: tenant(),
            employee_id,
            scope,
            role_template_id,
        },
    )
    .await
    .expect("assign");
    assignment_id
}

/// The staff codes a store's node lists now.
async fn codes_at(config: &FakeConfigTrees, store: StoreId) -> Vec<String> {
    staff_at(config, store)
        .await
        .iter()
        .map(|member| member["code"].as_str().expect("a code").to_owned())
        .collect()
}

/// The assignments a listing returns, read through the route.
async fn listed(router: &axum::Router, cookie: &str, query: &str) -> Vec<serde_json::Value> {
    let response = router
        .clone()
        .oneshot(get_with_cookie(
            &format!(
                "/admin/assignments?tenant_id={}&{query}",
                tenant().as_ulid()
            ),
            cookie,
        ))
        .await
        .expect("route the listing");
    assert_eq!(response.status(), StatusCode::OK, "{query}");
    json_body(response)
        .await
        .as_array()
        .expect("an array")
        .clone()
}

/// The people routes and the store-group routes over one roster and one set of groups, as
/// `main.rs` wires them: a membership change reaches the people the group's assignments name.
fn people_and_groups_app(
    admin: FakeAdmin,
    people: FakePeople,
    registry: FakeRegistry,
    config: FakeConfigTrees,
    audit: Arc<dyn AuditRecorder>,
) -> axum::Router {
    people_app(
        admin.clone(),
        people.clone(),
        registry.clone(),
        config.clone(),
        Arc::clone(&audit),
    )
    .merge(http::store_group_router(
        people.groups(),
        registry,
        config,
        Vec::new(),
        people,
        admin,
        clock(),
        audit,
    ))
}

/// An assignment to every store publishes at once to each open store of the tenant, and not to an
/// archived one; a store opened afterwards lists the person from its first publish (ADR-0158
/// decision 3).
#[tokio::test]
async fn an_assignment_to_every_store_reaches_each_open_store_and_one_opened_later() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let bao = seed_person(&people, 0xB40, "C02", "Bao").await;
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant(), store_id(), true);
    seed_store(&registry, tenant(), shop(1), true);
    seed_store(&registry, tenant(), shop(2), false);
    let router = people_app(
        provisioned_admin(),
        people,
        registry.clone(),
        config.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;

    let everywhere = [("scope_kind", "ASSIGNMENT_SCOPE_TENANT".to_owned())];
    let created =
        post_assignment(&router, &owner, &assignment_body(bao, cashier, &everywhere)).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        publish_report(created).await,
        vec![applied(store_id()), applied(shop(1))],
        "published at once to each open store, and not to the archived one"
    );
    assert_eq!(codes_at(&config, store_id()).await, vec!["C01", "C02"]);
    assert_eq!(codes_at(&config, shop(1)).await, vec!["C02"]);
    assert_eq!(versions_at(&config, shop(2)).await, 0);

    // A store the tenant opens afterwards: nobody names it, and its first publish lists Bao.
    seed_store(&registry, tenant(), shop(3), true);
    publish_people(&router, &owner, shop(3)).await;
    assert_eq!(codes_at(&config, shop(3)).await, vec!["C02"]);
    let rows = listed(&router, &owner, "scope_kind=ASSIGNMENT_SCOPE_TENANT").await;
    assert_eq!(rows.len(), 1, "the tenant's rows, listed by their kind");
    assert_eq!(rows[0]["scope_kind"], "ASSIGNMENT_SCOPE_TENANT");
    assert!(rows[0].get("store_id").is_none() && rows[0].get("store_group_id").is_none());
}

/// An assignment to a store group publishes at once to exactly the stores the group holds, and
/// each listing shows the scope it is at (ADR-0158 decision 3).
#[tokio::test]
async fn an_assignment_to_a_group_reaches_exactly_its_members() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let bao = seed_person(&people, 0xB40, "C02", "Bao").await;
    let registry = FakeRegistry::default();
    for store in [shop(1), shop(2), shop(3)] {
        seed_store(&registry, tenant(), store, true);
    }
    let airport = seed_group(&people, 0x6A1, &[shop(1), shop(2)]);
    let router = people_app(
        provisioned_admin(),
        people,
        registry,
        config.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;

    let through = [("store_group_id", airport.as_ulid().to_string())];
    let created = post_assignment(&router, &owner, &assignment_body(bao, cashier, &through)).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        publish_report(created).await,
        vec![applied(shop(1)), applied(shop(2))],
        "each member, and nobody else"
    );
    for member in [shop(1), shop(2)] {
        assert_eq!(codes_at(&config, member).await, vec!["C02"]);
    }
    assert_eq!(versions_at(&config, shop(3)).await, 0, "not a member");
    assert_eq!(
        versions_at(&config, store_id()).await,
        0,
        "nor a store outside the group"
    );

    let by_group = listed(
        &router,
        &owner,
        &format!("store_group_id={}", airport.as_ulid()),
    )
    .await;
    assert_eq!(by_group.len(), 1);
    assert_eq!(by_group[0]["scope_kind"], "ASSIGNMENT_SCOPE_STORE_GROUP");
    assert_eq!(by_group[0]["store_group_id"], airport.as_ulid().to_string());
    assert_eq!(by_group[0]["employee_name"], "Bao");
    let at_member = listed(&router, &owner, &format!("store_id={}", shop(1).as_ulid())).await;
    assert_eq!(
        at_member, by_group,
        "a member's listing shows who works there through it"
    );
    assert!(
        listed(&router, &owner, &format!("store_id={}", shop(3).as_ulid()))
            .await
            .is_empty()
    );
    let refused = router
        .clone()
        .oneshot(get_with_cookie(
            &format!(
                "/admin/assignments?tenant_id={}&scope_kind=ASSIGNMENT_SCOPE_STORE",
                tenant().as_ulid()
            ),
            &owner,
        ))
        .await
        .expect("route the listing");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "scope_kind");
    assert_eq!(body["error"]["details"][0]["reason"], "INVALID_ENUM_VALUE");
}

/// A store that joins or leaves a group some assignment names gains or loses those people, and is
/// told at once; a group nothing names changes nobody's node (ADR-0158 decisions 3 and 7).
#[tokio::test]
async fn a_store_that_joins_or_leaves_a_named_group_is_told_at_once() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let bao = seed_person(&people, 0xB40, "C02", "Bao").await;
    let registry = FakeRegistry::default();
    for store in [shop(1), shop(2), shop(3)] {
        seed_store(&registry, tenant(), store, true);
    }
    let airport = seed_group(&people, 0x6A1, &[shop(1)]);
    let quiet = seed_group(&people, 0x6A2, &[shop(1)]);
    seed_assignment(
        &people,
        2,
        bao,
        AssignmentScope::StoreGroup(airport),
        cashier,
    )
    .await;
    let router = people_and_groups_app(
        provisioned_admin(),
        people.clone(),
        registry,
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let owner = admin_cookie(&router).await;
    publish_people(&router, &owner, shop(1)).await;
    assert_eq!(codes_at(&config, shop(1)).await, vec!["C02"]);

    let move_members = |group: StoreGroupId, members: Vec<StoreId>| {
        let router = router.clone();
        let owner = owner.clone();
        let groups = people.groups();
        async move {
            let etag = StoreGroupStore::list_groups(&groups, tenant())
                .await
                .expect("list groups")
                .into_iter()
                .find(|row| row.record.group_id == group)
                .expect("the group")
                .etag;
            let ids: Vec<String> = members.iter().map(|id| id.as_ulid().to_string()).collect();
            router
                .oneshot(put_with_etag(
                    &format!("/admin/store-groups/{}/members", group.as_ulid()),
                    &serde_json::json!({ "tenant_id": tenant().as_ulid().to_string(), "store_ids": ids }),
                    &owner,
                    etag.as_str(),
                ))
                .await
                .expect("route the membership")
        }
    };

    let moved = move_members(airport, vec![shop(2)]).await;
    assert_eq!(moved.status(), StatusCode::OK);
    assert_eq!(
        publish_report(moved).await,
        vec![applied(shop(1)), applied(shop(2))],
        "the store that left and the store that joined"
    );
    assert!(
        codes_at(&config, shop(1)).await.is_empty(),
        "off the store that left"
    );
    assert_eq!(
        codes_at(&config, shop(2)).await,
        vec!["C02"],
        "on the one that joined"
    );
    assert_eq!(
        versions_at(&config, shop(3)).await,
        0,
        "untouched by the change"
    );
    let recorded = audit.list(None, 50).await.expect("list audit");
    assert!(
        recorded
            .iter()
            .any(|entry| entry.action == "permissions.publish"
                && entry.after.as_ref().is_some_and(|after| {
                    after["store_group_id"] == serde_json::json!(airport.to_string())
                })),
        "the publish names the group that caused it"
    );

    let unnamed = move_members(quiet, vec![shop(3)]).await;
    assert_eq!(unnamed.status(), StatusCode::OK);
    assert!(
        publish_report(unnamed).await.is_empty(),
        "no assignment names it"
    );
    assert_eq!(versions_at(&config, shop(3)).await, 0);
}

/// Assigning a person where they already hold an assignment is a conflict, not an outage: `409`
/// `ALREADY_EXISTS` naming the field that names the place, at each scope. The refused one writes
/// nothing, records nothing in the trail and publishes nothing, so a store the first reached is
/// published once (ADR-0158 decision 3).
#[tokio::test]
async fn a_second_assignment_at_the_same_place_is_refused_409_and_changes_nothing() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let bao = seed_person(&people, 0xB40, "C02", "Bao").await;
    let registry = FakeRegistry::default();
    for store in [store_id(), shop(1)] {
        seed_store(&registry, tenant(), store, true);
    }
    let airport = seed_group(&people, 0x6A1, &[shop(1)]);
    let router = people_app(
        provisioned_admin(),
        people.clone(),
        registry,
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let owner = admin_cookie(&router).await;
    let published = async || {
        [
            versions_at(&config, store_id()).await,
            versions_at(&config, shop(1)).await,
        ]
    };
    let recorded = async || audit.list(None, 100).await.expect("list audit").len();

    for (field, value) in [
        ("store_id", store_id().as_ulid().to_string()),
        ("store_group_id", airport.as_ulid().to_string()),
        ("scope_kind", "ASSIGNMENT_SCOPE_TENANT".to_owned()),
    ] {
        let body = assignment_body(bao, cashier, &[(field, value)]);
        let created = post_assignment(&router, &owner, &body).await;
        assert_eq!(created.status(), StatusCode::CREATED, "{field}");
        let (versions, entries) = (published().await, recorded().await);

        let again = post_assignment(&router, &owner, &body).await;
        assert_eq!(again.status(), StatusCode::CONFLICT, "{field}");
        let error = json_body(again).await;
        assert_eq!(error["error"]["status"], "ALREADY_EXISTS", "{error}");
        assert_eq!(error["error"]["details"][0]["field"], field, "{error}");
        assert_eq!(
            error["error"]["details"][0]["reason"], "ALREADY_EXISTS",
            "{error}"
        );
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("change or remove")),
            "it says what resolves it: {error}"
        );
        assert_eq!(published().await, versions, "nothing published ({field})");
        assert_eq!(recorded().await, entries, "nothing recorded ({field})");
    }
    let held = AssignmentStore::list_for_employee(&people, tenant(), bao)
        .await
        .expect("list");
    assert_eq!(
        held.len(),
        3,
        "one at each place, and no refused one written"
    );
}

/// Removing an assignment to a group or to every store publishes at once to every store it
/// reached, and the trail records its scope (ADR-0158 decision 7).
#[tokio::test]
async fn removing_a_wider_assignment_tells_every_store_it_reached() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let bao = seed_person(&people, 0xB40, "C02", "Bao").await;
    let cam = seed_person(&people, 0xCA3, "C03", "Cam").await;
    let registry = FakeRegistry::default();
    for store in [store_id(), shop(1), shop(2)] {
        seed_store(&registry, tenant(), store, true);
    }
    let airport = seed_group(&people, 0x6A1, &[shop(1), shop(2)]);
    let grouped = seed_assignment(
        &people,
        2,
        bao,
        AssignmentScope::StoreGroup(airport),
        cashier,
    )
    .await;
    let everywhere = seed_assignment(&people, 3, cam, AssignmentScope::Tenant, cashier).await;
    let router = people_app(
        provisioned_admin(),
        people,
        registry,
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let owner = admin_cookie(&router).await;
    for store in [store_id(), shop(1), shop(2)] {
        publish_people(&router, &owner, store).await;
    }
    let remove = |assignment: AssignmentId| {
        router.clone().oneshot(delete_with_cookie(
            &format!(
                "/admin/assignments/{}?tenant_id={}",
                assignment.as_ulid(),
                tenant().as_ulid()
            ),
            &owner,
        ))
    };

    let removed = remove(grouped).await.expect("route the removal");
    assert_eq!(removed.status(), StatusCode::OK);
    assert_eq!(
        publish_report(removed).await,
        vec![applied(shop(1)), applied(shop(2))]
    );
    assert_eq!(codes_at(&config, shop(1)).await, vec!["C03"], "Bao is off");
    assert_eq!(
        versions_at(&config, store_id()).await,
        1,
        "not a member: untouched"
    );

    let removed = remove(everywhere).await.expect("route the removal");
    assert_eq!(
        publish_report(removed).await,
        vec![applied(store_id()), applied(shop(1)), applied(shop(2))]
    );
    assert_eq!(
        codes_at(&config, store_id()).await,
        vec!["C01"],
        "Cam is off every store"
    );
    assert!(codes_at(&config, shop(2)).await.is_empty());

    let recorded = audit.list(None, 50).await.expect("list audit");
    let removal = recorded
        .iter()
        .find(|entry| entry.action == "assignment.remove" && entry.entity_id == grouped.to_string())
        .expect("the removal is recorded");
    assert_eq!(
        removal.before,
        Some(serde_json::json!({
            "id": grouped.to_string(),
            "employee_id": bao.to_string(),
            "scope_kind": "ASSIGNMENT_SCOPE_STORE_GROUP",
            "store_group_id": airport.to_string(),
            "role_template_id": cashier.to_string(),
        })),
        "the ids and the scope, and no name or code"
    );
}

/// Archiving a person, or a role, publishes at once to every store their assignments reach through
/// a group or the tenant, not only those named one by one (ADR-0158 decision 7).
#[tokio::test]
async fn an_archive_reaches_the_stores_a_group_or_the_tenant_reaches() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let bao = seed_person(&people, 0xB40, "C02", "Bao").await;
    let cam = seed_person(&people, 0xCA3, "C03", "Cam").await;
    let lead = RoleTemplateId::new(Ulid::from_u128(0x1EAD));
    RoleTemplateStore::create(
        &people,
        &NewRoleTemplate {
            role_template_id: lead,
            tenant_id: tenant(),
            name: "Shift lead".to_owned(),
            permissions: vec!["cash.shift.open".to_owned()],
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor: None,
        },
    )
    .await
    .expect("create role");
    let registry = FakeRegistry::default();
    for store in [store_id(), shop(1), shop(2)] {
        seed_store(&registry, tenant(), store, true);
    }
    let airport = seed_group(&people, 0x6A1, &[shop(1)]);
    seed_assignment(
        &people,
        2,
        bao,
        AssignmentScope::StoreGroup(airport),
        cashier,
    )
    .await;
    seed_assignment(&people, 3, cam, AssignmentScope::Tenant, lead).await;
    let router = people_app(
        provisioned_admin(),
        people,
        registry,
        config,
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let archive = |url: String, body: serde_json::Value| {
        let router = router.clone();
        let owner = owner.clone();
        let tenant_ulid = tenant_ulid.clone();
        async move {
            let read = router
                .clone()
                .oneshot(get_with_cookie(
                    &format!("{url}?tenant_id={tenant_ulid}"),
                    &owner,
                ))
                .await
                .expect("route the read");
            let archived = router
                .oneshot(patch_with_etag(&url, &body, &owner, &etag_of(&read)))
                .await
                .expect("route the archive");
            assert_eq!(archived.status(), StatusCode::OK, "{url}");
            publish_report(archived).await
        }
    };

    let person = archive(
        format!("/admin/employees/{}", bao.as_ulid()),
        serde_json::json!({ "tenant_id": tenant_ulid, "name": "Bao", "status": "archived" }),
    )
    .await;
    assert_eq!(person, vec![applied(shop(1))], "the group's member");
    let role = archive(
        format!("/admin/roles/{}", lead.as_ulid()),
        serde_json::json!({
            "tenant_id": tenant_ulid,
            "name": "Shift lead",
            "permissions": ["cash.shift.open"],
            "status": "archived",
        }),
    )
    .await;
    assert_eq!(
        role,
        vec![applied(store_id()), applied(shop(1)), applied(shop(2))],
        "every open store, through the tenant-wide assignment"
    );
}

/// A refused assignment's status and the `(field, reason)` pairs its refusal names, in order.
async fn refusal_of(
    router: &axum::Router,
    cookie: &str,
    body: &serde_json::Value,
) -> (StatusCode, Vec<(String, String)>) {
    let refused = post_assignment(router, cookie, body).await;
    let status = refused.status();
    let named = json_body(refused).await["error"]["details"]
        .as_array()
        .expect("details")
        .iter()
        .map(|detail| {
            (
                detail["field"].as_str().expect("a field").to_owned(),
                detail["reason"].as_str().expect("a reason").to_owned(),
            )
        })
        .collect();
    (status, named)
}

/// `(field, reason)` pairs, owned, as [`refusal_of`] returns them.
fn named(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(field, reason)| ((*field).to_owned(), (*reason).to_owned()))
        .collect()
}

/// An assignment names exactly one target: its scope's own id, and no other. Anything else is a
/// `400` naming each field at fault, and writes nothing; with no `scope_kind`, or
/// `ASSIGNMENT_SCOPE_UNSPECIFIED`, the id sent names the scope (ADR-0158 decision 3).
#[tokio::test]
async fn an_assignment_names_exactly_its_scopes_own_id() {
    let people = FakePeople::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let bao = seed_person(&people, 0xB40, "C02", "Bao").await;
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant(), store_id(), true);
    let airport = seed_group(&people, 0x6A1, &[store_id()]);
    let router = people_app(
        provisioned_admin(),
        people.clone(),
        registry,
        FakeConfigTrees::default(),
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;
    let store = ("store_id", store_id().as_ulid().to_string());
    let group = ("store_group_id", airport.as_ulid().to_string());
    let kind = |token: &str| ("scope_kind", token.to_owned());
    for (fields, expected) in [
        (
            vec![store.clone(), group.clone()],
            named(&[
                ("store_id", "MUTUALLY_EXCLUSIVE"),
                ("store_group_id", "MUTUALLY_EXCLUSIVE"),
            ]),
        ),
        (vec![], named(&[("store_id", "REQUIRED")])),
        (
            vec![kind("ASSIGNMENT_SCOPE_TENANT"), store.clone()],
            named(&[("store_id", "MUTUALLY_EXCLUSIVE")]),
        ),
        (
            vec![kind("ASSIGNMENT_SCOPE_STORE_GROUP"), store.clone()],
            named(&[
                ("store_id", "MUTUALLY_EXCLUSIVE"),
                ("store_group_id", "REQUIRED"),
            ]),
        ),
        (
            vec![kind("ASSIGNMENT_SCOPE_BRAND"), store.clone()],
            named(&[("scope_kind", "INVALID_ENUM_VALUE")]),
        ),
    ] {
        let body = assignment_body(bao, cashier, &fields);
        assert_eq!(
            refusal_of(&router, &owner, &body).await,
            (StatusCode::BAD_REQUEST, expected),
            "{fields:?}"
        );
    }
    assert!(
        AssignmentStore::list_for_employee(&people, tenant(), bao)
            .await
            .expect("list")
            .is_empty(),
        "no refusal wrote anything"
    );

    // `ASSIGNMENT_SCOPE_UNSPECIFIED` is no scope of its own: the id sent says which, and a store's
    // assignment publishes as before, when someone presses publish.
    let body = assignment_body(bao, cashier, &[kind("ASSIGNMENT_SCOPE_UNSPECIFIED"), store]);
    let created = post_assignment(&router, &owner, &body).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let body = json_body(created).await;
    assert!(body.get("stores").is_none(), "nothing published: {body}");
}

/// An assignment to a group the tenant does not have — another tenant's included — is `404`, and
/// to one it has archived `409`, naming `store_group_id` (ADR-0158 decision 7).
#[tokio::test]
async fn an_assignment_to_a_group_needs_one_of_its_tenants_own_open_groups() {
    let people = FakePeople::default();
    let (_alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let bao = seed_person(&people, 0xB40, "C02", "Bao").await;
    let closed = StoreGroupId::new(Ulid::from_u128(0x6A2));
    let foreign = StoreGroupId::new(Ulid::from_u128(0x6A3));
    for (group_id, tenant_id, status) in [
        (closed, tenant(), EntityStatus::Archived),
        (
            foreign,
            TenantId::new(Ulid::from_u128(0xB0B)),
            EntityStatus::Active,
        ),
    ] {
        let group = StoreGroup {
            group_id,
            tenant_id,
            name: "Elsewhere".to_owned(),
            status,
        };
        people.groups().seed(group, &[store_id()]);
    }
    let router = people_app(
        provisioned_admin(),
        people.clone(),
        FakeRegistry::default(),
        FakeConfigTrees::default(),
        Arc::new(NoopAuditRecorder),
    );
    let owner = admin_cookie(&router).await;
    for (group_id, status, reason) in [
        (
            StoreGroupId::new(Ulid::from_u128(0xDEAD)),
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
        ),
        (foreign, StatusCode::NOT_FOUND, "NOT_FOUND"),
        (closed, StatusCode::CONFLICT, "ARCHIVED"),
    ] {
        let fields = [("store_group_id", group_id.as_ulid().to_string())];
        let body = assignment_body(bao, cashier, &fields);
        assert_eq!(
            refusal_of(&router, &owner, &body).await,
            (status, named(&[("store_group_id", reason)])),
            "{group_id}"
        );
    }
    assert!(
        AssignmentStore::list_for_employee(&people, tenant(), bao)
            .await
            .expect("list")
            .is_empty(),
        "no refusal wrote anything"
    );
}

// --- ADR-0158 Rollout: what each role at a store lacks before the store enforces -----------------

/// The readiness read over the people and settings fakes, with the one store these tests read
/// seeded active.
fn readiness_app(admin: FakeAdmin, people: &FakePeople, settings: FakeSettings) -> axum::Router {
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant(), store_id(), true);
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::permissions_readiness_router(
        people.clone(),
        settings,
        registry,
        people.groups(),
        admin,
        clock(),
    ))
}

/// The readiness read's address for `store`.
fn readiness_url(store: StoreId) -> String {
    format!(
        "/admin/stores/{}/permissions/readiness?tenant_id={}",
        store.as_ulid(),
        tenant().as_ulid()
    )
}

/// Seeds the three people the readiness test reads: Alice as a Cashier at the store, Bao with a
/// tenant-wide role granting everything a store decides, and Cam with only a role since archived.
async fn seed_readiness(people: &FakePeople) {
    let (_alice, _cashier) = seed_a_cashier(people, store_id()).await;
    let everything: Vec<String> = pos_core::permission::Permission::ALL
        .iter()
        .map(|permission| permission.meta())
        .filter(|meta| meta.group != pos_core::permission::PermissionGroup::CloudAdministration)
        .map(|meta| meta.id.to_owned())
        .collect();
    let (lead, old) = (
        RoleTemplateId::new(Ulid::from_u128(0x1EAD)),
        RoleTemplateId::new(Ulid::from_u128(0x01D)),
    );
    for (role_template_id, name, permissions) in [(lead, "Lead", everything), (old, "Old", vec![])]
    {
        let new = NewRoleTemplate {
            role_template_id,
            tenant_id: tenant(),
            name: name.to_owned(),
            permissions,
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor: None,
        };
        RoleTemplateStore::create(people, &new)
            .await
            .expect("create role");
    }
    people
        .roles
        .rows
        .lock()
        .expect("lock")
        .iter_mut()
        .for_each(|row| {
            if row.record.role_template_id == old {
                row.record.status = EntityStatus::Archived;
            }
        });
    let bao = seed_person(people, 0xB40, "C02", "Bao").await;
    let cam = seed_person(people, 0xCA3, "C03", "Cam").await;
    seed_assignment(people, 2, bao, AssignmentScope::Tenant, lead).await;
    seed_assignment(people, 3, cam, AssignmentScope::Store(store_id()), old).await;
}

/// Before a store enforces each person's own permissions, the console reads what each role held
/// there does not grant (ADR-0158, Rollout). It needs `console.people.read`, as every read of the
/// store's staff does; it names roles and counts people, and names nobody.
#[tokio::test]
async fn the_readiness_read_lists_each_roles_gaps_and_names_nobody() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    seed_readiness(&people).await;
    let settings = FakeSettings::default();
    let router = readiness_app(admin.clone(), &people, settings.clone());

    for (role, token) in [
        (AdminRole::Viewer, "viewer-ready"),
        (AdminRole::Ops, "ops-ready"),
    ] {
        let cookie = role_session_cookie(&admin, role, token).await;
        let refused = router
            .clone()
            .oneshot(get_with_cookie(&readiness_url(store_id()), &cookie))
            .await
            .expect("route the read");
        assert_eq!(
            refused.status(),
            StatusCode::FORBIDDEN,
            "{role:?} lacks console.people.read"
        );
    }

    let owner = role_session_cookie(&admin, AdminRole::Owner, "owner-ready").await;
    let read = router
        .clone()
        .oneshot(get_with_cookie(&readiness_url(store_id()), &owner))
        .await
        .expect("route the read");
    assert_eq!(read.status(), StatusCode::OK);
    let body = json_body(read).await;
    assert_eq!(body["store_id"], store_id().as_ulid().to_string());
    assert_eq!(body["enforced"], false, "the default");
    assert!(body.get("enforced_scope").is_none() && body.get("enforced_scope_id").is_none());
    assert_eq!(
        body["people_without_role"], 1,
        "Cam holds only an archived role"
    );
    let roles = body["roles"].as_array().expect("roles");
    let names: Vec<&str> = roles
        .iter()
        .filter_map(|role| role["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Cashier", "Lead"],
        "the archived role is not listed"
    );
    assert_eq!(roles[0]["people"], 1);
    assert_eq!(
        roles[0]["missing"][0],
        serde_json::json!({ "id": "sales.item.mark_unavailable", "group": "SALES", "risk": "LOW" }),
        "an everyday permission first, the catalogue's first"
    );
    assert!(
        !roles[0]["missing"]
            .as_array()
            .expect("missing")
            .iter()
            .any(|permission| permission["id"] == "billing.discount.apply"),
        "what the role grants is not missing"
    );
    assert_eq!(roles[1]["people"], 1, "reached through the tenant");
    assert_eq!(roles[1]["missing"], serde_json::json!([]));
    let text = body.to_string();
    for person in ["Alice", "Bao", "Cam", "C01", "C02", "C03"] {
        assert!(!text.contains(person), "no person is named: {text}");
    }

    // The store's own value, and where it comes from.
    pos_cloud::settings::SettingsStore::put(
        &settings,
        tenant(),
        &pos_cloud::settings::SettingValue {
            setting_key: "permissions.enforced".to_owned(),
            scope: Open::from_known(pos_proto::settings::SettingScope::Store),
            scope_id: store_id().to_string(),
            value: serde_json::json!(true),
            update_time: Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("valid"),
        },
    )
    .await
    .expect("write the value");
    let read = router
        .clone()
        .oneshot(get_with_cookie(&readiness_url(store_id()), &owner))
        .await
        .expect("route the read");
    let body = json_body(read).await;
    assert_eq!(body["enforced"], true);
    assert_eq!(body["enforced_scope"], "SETTING_SCOPE_STORE");
    assert_eq!(body["enforced_scope_id"], store_id().to_string());

    let elsewhere = router
        .oneshot(get_with_cookie(
            &readiness_url(StoreId::new(Ulid::from_u128(0xDEAD))),
            &owner,
        ))
        .await
        .expect("route the read");
    assert_eq!(
        elsewhere.status(),
        StatusCode::NOT_FOUND,
        "a store the tenant does not have"
    );
}
