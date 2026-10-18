// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The people routes (ADR-0070): employees, role templates and assignments, the roster read, who may
//! read and write them, and the people publish.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditSink, AuditStore, NoopAuditRecorder};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::config_tree::ConfigTreeStore;
use pos_cloud::http;
use pos_cloud::people::EmployeeStore;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeConfigTrees, FakePeople, FakeRegistry, admin_cookie, clock, delete_with_cookie,
    etag_of, get_with_cookie, json_body, patch_with_etag, people_app, people_app_with_audit,
    people_publish_app, post_with_cookie, provisioned_admin, put_with_cookie, role_session_cookie,
    seed_a_cashier, seed_store, store_id, tenant, versions_at,
};

/// A staff code the tenant already uses is a conflict, not an outage: `409` `ALREADY_EXISTS` naming
/// `code`, saying to choose another, and never quoting the code, which identifies a person. The
/// refused one writes nothing, records nothing in the trail and publishes nothing.
#[tokio::test]
async fn a_second_employee_with_a_staff_code_in_use_is_refused_409_and_changes_nothing() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let registry = FakeRegistry::default();
    seed_store(&registry, tenant(), store_id(), true);
    let router = people_app(
        provisioned_admin(),
        people.clone(),
        registry,
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let owner = admin_cookie(&router).await;
    let hire = |name: &str| {
        let body = serde_json::json!({
            "tenant_id": tenant().as_ulid().to_string(),
            "code": "B07",
            "name": name,
        });
        router
            .clone()
            .oneshot(post_with_cookie("/admin/employees", &body, &owner))
    };
    let recorded = async || audit.list(None, 100).await.expect("list audit").len();

    let created = hire("Bao").await.expect("route the create");
    assert_eq!(created.status(), StatusCode::CREATED);
    let (published, entries) = (versions_at(&config, store_id()).await, recorded().await);

    let again = hire("Binh").await.expect("route the create");
    assert_eq!(again.status(), StatusCode::CONFLICT);
    let error = json_body(again).await;
    assert_eq!(error["error"]["status"], "ALREADY_EXISTS", "{error}");
    assert_eq!(error["error"]["details"][0]["field"], "code", "{error}");
    assert_eq!(
        error["error"]["details"][0]["reason"], "ALREADY_EXISTS",
        "{error}"
    );
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("choose another")),
        "it says what resolves it: {error}"
    );
    assert!(
        !error.to_string().contains("B07"),
        "never the code: {error}"
    );
    assert_eq!(
        versions_at(&config, store_id()).await,
        published,
        "nothing published"
    );
    assert_eq!(recorded().await, entries, "nothing recorded");
    let holders: Vec<String> = EmployeeStore::list(&people, tenant())
        .await
        .expect("the roster")
        .into_iter()
        .filter(|row| row.record.code == "B07")
        .map(|row| row.record.name)
        .collect();
    assert_eq!(holders, ["Bao"], "the refused one wrote nothing");
}

/// An assignment names the person it grants, so the console can label it without reading the roster.
///
/// This is what has to be true before the People screen's employee table can be paged
/// ([ADR-0098](../../../../docs/adr/0098-paged-admin-reads.md), B3-4): the screen used to turn an
/// assignment's `employee_id` into a name by searching the whole loaded roster, which stops working
/// the moment the roster arrives a page at a time. The resolution moves to the read that needs it.
#[tokio::test]
async fn an_assignment_names_the_person_it_grants_so_the_console_need_not_read_the_roster() {
    let admin = provisioned_admin();
    let router = people_app_with_audit(
        admin,
        FakePeople::default(),
        Arc::new(AuditSink::new(FakeAudit::default())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/employees",
            &serde_json::json!({ "tenant_id": tenant_ulid, "code": "C77", "name": "Mai" }),
            &cookie,
        ))
        .await
        .expect("route create employee");
    assert_eq!(created.status(), StatusCode::CREATED);
    let employee_id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();

    let role = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/roles",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "name": "Server",
                "permissions": ["billing.discount.apply"],
            }),
            &cookie,
        ))
        .await
        .expect("route create role");
    assert_eq!(role.status(), StatusCode::CREATED);
    let role_id = json_body(role).await["id"].as_str().expect("id").to_owned();

    let assigned = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/assignments",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "employee_id": employee_id,
                "store_id": store_ulid,
                "role_template_id": role_id,
            }),
            &cookie,
        ))
        .await
        .expect("route assign");
    assert_eq!(assigned.status(), StatusCode::CREATED);

    // Both directions of the read carry the person, not just their id.
    for query in [
        format!("tenant_id={tenant_ulid}&store_id={store_ulid}"),
        format!("tenant_id={tenant_ulid}&employee_id={employee_id}"),
    ] {
        let listed = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/assignments?{query}"),
                &cookie,
            ))
            .await
            .expect("route list assignments");
        assert_eq!(listed.status(), StatusCode::OK);
        let rows = json_body(listed).await;
        let row = &rows[0];
        assert_eq!(
            row["employee_name"], "Mai",
            "the row names the person, so no roster read is needed to label it: {rows}"
        );
        assert_eq!(row["employee_code"], "C77", "and carries their staff code");
        assert_eq!(
            row["employee_id"], employee_id,
            "the id stays, because that is what the remove call takes"
        );
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end lifecycle over the people routes — create/read/set-PIN/update an \
              employee, create a role and reject an unknown permission, assign/list/remove, and the \
              audit + no-PII-in-the-trail assertions — kept together so the invariant is checked \
              against the same data the whole flow produced"
)]
async fn people_routes_crud_lifecycle_audited_without_pii() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let audit = FakeAudit::default();
    let router = people_app_with_audit(admin, people, Arc::new(AuditSink::new(audit.clone())));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Create an employee (identity only).
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/employees",
            &serde_json::json!({ "tenant_id": tenant_ulid, "code": "C01", "name": "Alice" }),
            &cookie,
        ))
        .await
        .expect("route create employee");
    assert_eq!(created.status(), StatusCode::CREATED);
    let employee_id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();

    // The roster read shows the employee, with no PIN yet.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route list employees");
    assert_eq!(listed.status(), StatusCode::OK);
    let employees = json_body(listed).await;
    assert_eq!(employees.as_array().expect("array").len(), 1);
    assert_eq!(employees[0]["has_pin"], serde_json::json!(false));

    // Set a PIN: non-digits are rejected, four digits accepted.
    let bad_pin = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/employees/{employee_id}/pin"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "pin": "abcd" }),
            &cookie,
        ))
        .await
        .expect("route bad pin");
    assert_eq!(bad_pin.status(), StatusCode::BAD_REQUEST);
    let set_pin = router
        .clone()
        .oneshot(put_with_cookie(
            &format!("/admin/employees/{employee_id}/pin"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "pin": "1234" }),
            &cookie,
        ))
        .await
        .expect("route set pin");
    assert_eq!(set_pin.status(), StatusCode::NO_CONTENT);

    // Now has_pin is true on a read-one.
    let one = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees/{employee_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route get one");
    assert_eq!(one.status(), StatusCode::OK);
    let employee_etag = etag_of(&one);
    let one_body = json_body(one).await;
    assert_eq!(one_body["has_pin"], serde_json::json!(true));
    assert_eq!(
        one_body["etag"], employee_etag,
        "the header and the body name the same version"
    );

    // Rename, at the version the read-one handed out (ADR-0094). Not an archive: an archived person
    // can no longer be assigned to a store below (ADR-0158 decision 7), and archiving publishes at
    // once, which `archiving_a_person_tells_every_store_they_work_at_and_a_rename_tells_none`
    // covers.
    let updated = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/employees/{employee_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Alice Nguyen", "status": "active" }),
            &cookie,
            &employee_etag,
        ))
        .await
        .expect("route update");
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);

    // A second writer still holding the version from before that rename is refused, and the first
    // edit survives.
    let stale = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/employees/{employee_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Someone Else", "status": "active" }),
            &cookie,
            &employee_etag,
        ))
        .await
        .expect("route stale update");
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);

    // The permission catalogue is offered for the role editor.
    let catalogue = router
        .clone()
        .oneshot(get_with_cookie("/admin/people/permissions", &cookie))
        .await
        .expect("route permissions");
    assert_eq!(catalogue.status(), StatusCode::OK);
    assert!(
        !json_body(catalogue)
            .await
            .as_array()
            .expect("array")
            .is_empty()
    );

    // A role naming an unknown permission is refused.
    let bad_role = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/roles",
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Bogus", "permissions": ["not.a.real.permission"] }),
            &cookie,
        ))
        .await
        .expect("route bad role");
    assert_eq!(bad_role.status(), StatusCode::BAD_REQUEST);
    let refusal = json_body(bad_role).await;
    assert_eq!(
        refusal["error"]["details"][0]["field"], "permissions",
        "the field named is the one the caller sent, not a paraphrase: {refusal}"
    );
    assert_eq!(
        refusal["error"]["details"][0]["reason"],
        "INVALID_ENUM_VALUE"
    );

    // A valid role.
    let role = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/roles",
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Cashier", "permissions": ["billing.discount.apply"] }),
            &cookie,
        ))
        .await
        .expect("route create role");
    assert_eq!(role.status(), StatusCode::CREATED);
    let role_id = json_body(role).await["id"].as_str().expect("id").to_owned();

    // Assign the employee to a store with that role.
    let assign = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/assignments",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "employee_id": employee_id,
                "store_id": store_ulid,
                "role_template_id": role_id,
            }),
            &cookie,
        ))
        .await
        .expect("route assign");
    assert_eq!(assign.status(), StatusCode::CREATED);
    let assignment_id = json_body(assign).await["id"]
        .as_str()
        .expect("id")
        .to_owned();

    // Readable by store and by employee.
    let by_store = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/assignments?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route by store");
    assert_eq!(by_store.status(), StatusCode::OK);
    assert_eq!(
        json_body(by_store).await.as_array().expect("array").len(),
        1
    );
    let by_employee = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/assignments?tenant_id={tenant_ulid}&employee_id={employee_id}"),
            &cookie,
        ))
        .await
        .expect("route by employee");
    assert_eq!(by_employee.status(), StatusCode::OK);
    assert_eq!(
        json_body(by_employee)
            .await
            .as_array()
            .expect("array")
            .len(),
        1
    );

    // Remove (offboard). The store is told at once, and the answer says how that went (ADR-0158).
    let remove = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/assignments/{assignment_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route remove");
    assert_eq!(remove.status(), StatusCode::OK);

    // Every write was audited — and the trail never carries the employee's name, PIN, or PIN hash.
    let recorded = audit.list(None, 50).await.expect("list audit");
    let actions: Vec<&str> = recorded.iter().map(|entry| entry.action.as_str()).collect();
    for expected in [
        "employee.create",
        "employee.set_pin",
        "employee.update",
        "role.create",
        "assignment.create",
        "assignment.remove",
    ] {
        assert!(actions.contains(&expected), "recorded {expected}");
    }
    let dump = serde_json::to_string(
        &recorded
            .iter()
            .map(|entry| {
                (
                    entry.action.clone(),
                    entry.before.clone(),
                    entry.after.clone(),
                )
            })
            .collect::<Vec<_>>(),
    )
    .expect("serialize the trail");
    assert!(
        !dump.contains("Alice"),
        "the employee's name never enters the audit trail"
    );
    assert!(
        !dump.contains("1234"),
        "the PIN never enters the audit trail"
    );
    assert!(
        !dump.to_lowercase().contains("argon2"),
        "the PIN hash never enters the audit trail"
    );
    // The employee.create entry does record the code (id/code/status), just not the name.
    assert!(dump.contains("C01"), "the staff code is recorded");
}

/// The audit trail shows the staff code an employee write records only to a role holding
/// `console.people.read`
/// ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 9).
///
/// `employee.create` and `employee.update` record the employee's id, code and status (ADR-0070),
/// and every console role reads the trail. A Viewer or an Ops user still sees each entry and who
/// did what to which employee, but not the code; an Owner or Admin reads the entry as written,
/// which is also what shows the trail still records it. The window and the page answer alike, and
/// a role's name, which names no person, stays.
#[tokio::test]
async fn the_audit_trail_shows_a_staff_code_only_to_a_role_that_reads_staff_records() {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let router = people_app_with_audit(
        admin.clone(),
        FakePeople::default(),
        Arc::new(AuditSink::new(audit.clone())),
    )
    .merge(http::audit_router(audit, admin.clone(), clock()));
    let owner = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    // An employee created and then archived, each entry recording the code, and a role.
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/employees",
            &serde_json::json!({ "tenant_id": tenant_ulid, "code": "C01", "name": "Alice" }),
            &owner,
        ))
        .await
        .expect("route create employee");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created_etag = etag_of(&created);
    let employee_id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();
    let archived = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/employees/{employee_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Alice", "status": "archived" }),
            &owner,
            &created_etag,
        ))
        .await
        .expect("route update employee");
    // An archive publishes at once (ADR-0158 decision 7) — here to no store, as she works at none.
    assert_eq!(archived.status(), StatusCode::OK);
    let cashier = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/roles",
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Cashier", "permissions": [] }),
            &owner,
        ))
        .await
        .expect("route create role");
    assert_eq!(cashier.status(), StatusCode::CREATED);

    for (role, token, names_staff) in [
        (AdminRole::Viewer, "viewer-audit", false),
        (AdminRole::Ops, "ops-audit", false),
        (AdminRole::Admin, "admin-audit", true),
        (AdminRole::Owner, "owner-audit", true),
    ] {
        let cookie = role_session_cookie(&admin, role, token).await;
        for url in [
            format!("/admin/audit?tenant_id={tenant_ulid}"),
            format!("/admin/audit?tenant_id={tenant_ulid}&limit=10&offset=0"),
        ] {
            let read = router
                .clone()
                .oneshot(get_with_cookie(&url, &cookie))
                .await
                .expect("route the audit read");
            assert_eq!(read.status(), StatusCode::OK, "{role:?} reads {url}");
            let body = json_body(read).await;
            let entries = body
                .as_array()
                .or_else(|| body["items"].as_array())
                .expect("the window is an array and the page carries one");
            let mut sides = 0;
            for entry in entries
                .iter()
                .filter(|entry| entry["entity_type"] == "employee")
            {
                for side in ["before", "after"] {
                    let Some(fields) = entry[side].as_object() else {
                        continue; // a create has no before
                    };
                    sides += 1;
                    assert_eq!(fields["id"], employee_id.as_str(), "{role:?}: {url}");
                    assert!(fields.contains_key("status"), "{role:?}: the status stays");
                    assert!(!fields.contains_key("name"), "{role:?}: never a name");
                    let code = names_staff.then(|| serde_json::json!("C01"));
                    assert_eq!(
                        fields.get("code"),
                        code.as_ref(),
                        "{role:?}: a staff code only with console.people.read: {url} {fields:?}"
                    );
                }
            }
            assert_eq!(sides, 3, "{role:?} sees both employee entries: {url}");
            let role_entry = entries
                .iter()
                .find(|entry| entry["entity_type"] == "role")
                .expect("the role entry is read too");
            assert_eq!(
                role_entry["after"]["name"], "Cashier",
                "{role:?}: a role's name names no person"
            );
        }
    }
}

#[tokio::test]
async fn the_roster_read_returns_the_whole_tenant_unpaged_and_a_page_when_a_limit_is_named() {
    let router = people_app_with_audit(
        provisioned_admin(),
        FakePeople::default(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    for index in 1..=3_u32 {
        let created = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/employees",
                &serde_json::json!({
                    "tenant_id": tenant_ulid,
                    "code": format!("C{index:02}"),
                    "name": format!("Person {index}"),
                }),
                &cookie,
            ))
            .await
            .expect("route create employee");
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    // No limit is the whole roster as a bare array — the read the permission-node publish makes,
    // which a page would leave incomplete (ADR-0098).
    let whole = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the whole roster");
    assert_eq!(whole.status(), StatusCode::OK);
    let whole = json_body(whole).await;
    assert_eq!(
        whole
            .as_array()
            .expect("a bare array, not an envelope")
            .len(),
        3,
    );

    // Naming a limit asks for a page: the window, plus the tenant's headcount.
    let paged = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}&limit=2"),
            &cookie,
        ))
        .await
        .expect("route the paged roster");
    assert_eq!(paged.status(), StatusCode::OK);
    let paged = json_body(paged).await;
    assert_eq!(paged["items"].as_array().expect("the window").len(), 2);
    assert_eq!(paged["total"], 3, "the headcount, not the window's size");
    assert_eq!(paged["limit"], 2, "the bounds used are echoed back");
    assert_eq!(paged["offset"], 0);
    assert!(
        paged["items"][0]["etag"].is_string(),
        "a paged row keeps the per-row etag ADR-0094 put on it",
    );
    assert_eq!(
        paged["items"][0]["has_pin"],
        serde_json::json!(false),
        "the page carries whether a PIN exists — never the hash (ADR-0070)",
    );

    // The second page holds the remainder, and the two windows do not overlap.
    let rest = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}&limit=2&offset=2"),
            &cookie,
        ))
        .await
        .expect("route the second page");
    let rest = json_body(rest).await;
    assert_eq!(rest["items"].as_array().expect("the window").len(), 1);
    assert_eq!(rest["total"], 3);
    assert_ne!(
        rest["items"][0]["employee_id"], paged["items"][0]["employee_id"],
        "the pages partition the roster rather than repeating it"
    );
}

/// Bounds outside the accepted range are refused by field, and an offset without a limit is refused
/// rather than read as a page of some default size — there is no default size (ADR-0098).
///
/// Its own test rather than a tail on the read above, because a refusal happens before any read: it
/// needs no roster at all, and asserting it against an empty tenant is what proves that.
#[tokio::test]
async fn the_paged_roster_refuses_a_bound_outside_the_range_and_an_offset_without_a_limit() {
    let router = people_app_with_audit(
        provisioned_admin(),
        FakePeople::default(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();

    for (bound, field) in [
        ("limit=100000", "limit"),
        ("limit=0", "limit"),
        ("offset=1", "offset"),
    ] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/employees?tenant_id={tenant_ulid}&{bound}"),
                &cookie,
            ))
            .await
            .expect("route a bad bound");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "{bound} is refused"
        );
        let body = json_body(refused).await;
        assert_eq!(
            body["error"]["details"][0]["field"], field,
            "{bound} names {field}"
        );
    }
}

/// `?q=` narrows the roster page to whoever a name or a staff code matches, and narrows `total` too.
///
/// This is what an assign picker needs before the People screen's table can be paged
/// ([ADR-0098](../../../../docs/adr/0098-paged-admin-reads.md), B3-4): a picker that offers matches for
/// what the operator typed does not need the whole roster loaded, and one that does cannot survive
/// the table paging out from under it.
#[tokio::test]
async fn the_roster_search_narrows_the_page_and_its_total_by_name_or_code() {
    let router = people_app_with_audit(
        provisioned_admin(),
        FakePeople::default(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    for (code, name) in [("C01", "Mai Anh"), ("C02", "Bao"), ("MAI99", "Linh")] {
        let created = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/employees",
                &serde_json::json!({ "tenant_id": tenant_ulid, "code": code, "name": name }),
                &cookie,
            ))
            .await
            .expect("route create employee");
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    // "mai" matches one person by name and a different one by code — case-insensitively, and the
    // total counts the match rather than the roster.
    let matched = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}&limit=10&q=mai"),
            &cookie,
        ))
        .await
        .expect("route search");
    assert_eq!(matched.status(), StatusCode::OK);
    let body = json_body(matched).await;
    assert_eq!(
        body["total"], 2,
        "the total counts the match, not the roster: {body}"
    );
    let mut codes: Vec<String> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|row| row["code"].as_str().expect("a code").to_owned())
        .collect();
    codes.sort();
    assert_eq!(
        codes,
        vec!["C01".to_owned(), "MAI99".to_owned()],
        "one matched on name, the other on code"
    );

    // No match is an empty page and a zero total, not an error — the one true zero.
    let none = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}&limit=10&q=nobody"),
            &cookie,
        ))
        .await
        .expect("route search with no match");
    assert_eq!(none.status(), StatusCode::OK);
    let body = json_body(none).await;
    assert_eq!(body["total"], 0);
    assert!(body["items"].as_array().expect("items").is_empty());

    // `q` shapes a page, so it is refused on the unpaged read rather than silently ignored.
    let unpaged = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}&q=mai"),
            &cookie,
        ))
        .await
        .expect("route search without a limit");
    assert_eq!(
        unpaged.status(),
        StatusCode::BAD_REQUEST,
        "a search with no limit is refused, not answered with the whole roster"
    );
    let body = json_body(unpaged).await;
    assert_eq!(body["error"]["details"][0]["field"], "q");
}

/// `?sort=` and `?order=` reorder the roster page, and an unknown token is refused by name.
///
/// The table's headers are what need this: they sort `name` and `code` client-side today, and a
/// server-paged table whose headers reordered only the visible twelve rows would be telling the
/// operator something false. A refusal that lists the tokens beats silently handing back the
/// default order (ADR-0096).
#[tokio::test]
async fn the_roster_page_sorts_by_name_or_code_and_refuses_an_unknown_token() {
    let router = people_app_with_audit(
        provisioned_admin(),
        FakePeople::default(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    // Created in an order that matches neither sort, so a page that came back in insertion order
    // would fail both.
    for (code, name) in [("C03", "Bao"), ("C01", "Mai"), ("C02", "An")] {
        let created = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/employees",
                &serde_json::json!({ "tenant_id": tenant_ulid, "code": code, "name": name }),
                &cookie,
            ))
            .await
            .expect("route create employee");
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    // The URL is built outside the async block so the closure borrows `tenant_ulid` rather than
    // moving it, leaving it usable by the refusal cases below.
    let page_of = |query: &str| {
        let router = router.clone();
        let cookie = cookie.clone();
        let url = format!("/admin/employees?tenant_id={tenant_ulid}&limit=10&{query}");
        async move {
            let response = router
                .oneshot(get_with_cookie(&url, &cookie))
                .await
                .expect("route a sorted page");
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await["items"]
                .as_array()
                .expect("items")
                .iter()
                .map(|row| row["name"].as_str().expect("a name").to_owned())
                .collect::<Vec<_>>()
        }
    };

    assert_eq!(
        page_of("sort=name").await,
        vec!["An".to_owned(), "Bao".to_owned(), "Mai".to_owned()],
        "by name, ascending"
    );
    assert_eq!(
        page_of("sort=name&order=desc").await,
        vec!["Mai".to_owned(), "Bao".to_owned(), "An".to_owned()],
        "and `desc` is the exact reverse, not a different order sharing a first column"
    );
    assert_eq!(
        page_of("sort=code").await,
        vec!["Mai".to_owned(), "An".to_owned(), "Bao".to_owned()],
        "by staff code — a different sequence from the name order, so one cannot pass for the other"
    );
    assert_eq!(
        page_of("sort=newest&order=asc").await.len(),
        3,
        "the default order names a token too, so a caller can ask for it explicitly"
    );

    // An unknown token is refused, naming the field and what it accepts.
    for (query, field) in [
        ("sort=hired", "sort"),
        ("sort=name&order=sideways", "order"),
    ] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/employees?tenant_id={tenant_ulid}&limit=10&{query}"),
                &cookie,
            ))
            .await
            .expect("route a bad token");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "{query} is refused"
        );
        let body = json_body(refused).await;
        assert_eq!(body["error"]["details"][0]["field"], field);
        assert_eq!(body["error"]["details"][0]["reason"], "INVALID_ENUM_VALUE");
    }

    // Both shape a page, so both are refused on the unpaged read rather than ignored.
    for (query, field) in [("sort=name", "sort"), ("order=desc", "order")] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/employees?tenant_id={tenant_ulid}&{query}"),
                &cookie,
            ))
            .await
            .expect("route an unpaged shaping param");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            json_body(refused).await["error"]["details"][0]["field"],
            field
        );
    }
}

#[tokio::test]
async fn people_writes_require_manage_people_and_the_roster_needs_people_read() {
    let admin = provisioned_admin();
    let router = people_app_with_audit(
        admin.clone(),
        FakePeople::default(),
        Arc::new(NoopAuditRecorder),
    );
    let tenant_ulid = tenant().as_ulid().to_string();

    // A Viewer may not read the roster — a staff record needs console.people.read (ADR-0158)…
    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/employees?tenant_id={tenant_ulid}"),
            &viewer,
        ))
        .await
        .expect("route read");
    assert_eq!(
        read.status(),
        StatusCode::FORBIDDEN,
        "the roster needs console.people.read, not console.data.read"
    );

    // …still reads the role templates, which name nobody…
    let roles = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/roles?tenant_id={tenant_ulid}"),
            &viewer,
        ))
        .await
        .expect("route list roles");
    assert_eq!(
        roles.status(),
        StatusCode::OK,
        "role templates need only console.data.read"
    );

    // …and may not create an employee (that needs console.people.manage).
    let forbidden = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/employees",
            &serde_json::json!({ "tenant_id": tenant_ulid, "code": "C01", "name": "Alice" }),
            &viewer,
        ))
        .await
        .expect("route create");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    // Ops cannot manage people either.
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let ops_forbidden = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/roles",
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Cashier", "permissions": [] }),
            &ops,
        ))
        .await
        .expect("route create role");
    assert_eq!(ops_forbidden.status(), StatusCode::FORBIDDEN);
}

/// Every read that hands back a person — the roster, one employee, and the assignment list, whose
/// rows name who they grant — needs `console.people.read`, which only Owner and Admin hold
/// ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 9).
///
/// A name and a staff code are T1 personal data (ADR-0070), and these reads sat behind
/// `console.data.read`, which Ops and Viewer hold too. The permission catalogue and the role
/// templates name nobody, so every role keeps them.
#[tokio::test]
async fn staff_records_need_people_read_which_ops_and_viewer_do_not_hold() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let (alice, cashier) = seed_a_cashier(&people, store_id()).await;
    let router = people_app_with_audit(admin.clone(), people, Arc::new(NoopAuditRecorder));

    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let alice_ulid = alice.as_ulid().to_string();
    let staff_records = [
        format!("/admin/employees?tenant_id={tenant_ulid}"),
        format!("/admin/employees?tenant_id={tenant_ulid}&limit=10"),
        format!("/admin/employees/{alice_ulid}?tenant_id={tenant_ulid}"),
        format!("/admin/assignments?tenant_id={tenant_ulid}&store_id={store_ulid}"),
        format!("/admin/assignments?tenant_id={tenant_ulid}&employee_id={alice_ulid}"),
    ];
    let naming_nobody = [
        "/admin/people/permissions".to_owned(),
        format!("/admin/roles?tenant_id={tenant_ulid}"),
        format!("/admin/roles/{}?tenant_id={tenant_ulid}", cashier.as_ulid()),
    ];

    for (role, token, reads_staff) in [
        (AdminRole::Viewer, "viewer-staff", false),
        (AdminRole::Ops, "ops-staff", false),
        (AdminRole::Admin, "admin-staff", true),
        (AdminRole::Owner, "owner-staff", true),
    ] {
        let cookie = role_session_cookie(&admin, role, token).await;
        for url in &staff_records {
            let response = router
                .clone()
                .oneshot(get_with_cookie(url, &cookie))
                .await
                .expect("route a staff read");
            if reads_staff {
                assert_eq!(response.status(), StatusCode::OK, "{role:?} reads {url}");
                let body = json_body(response).await.to_string();
                assert!(
                    body.contains("Alice"),
                    "{role:?} is shown the person: {body}"
                );
            } else {
                assert_eq!(
                    response.status(),
                    StatusCode::FORBIDDEN,
                    "{role:?} lacks console.people.read, so {url} is refused"
                );
            }
        }
        for url in &naming_nobody {
            let response = router
                .clone()
                .oneshot(get_with_cookie(url, &cookie))
                .await
                .expect("route a read that names nobody");
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{role:?} reads {url}, which needs only console.data.read"
            );
        }
    }
}

#[tokio::test]
async fn publishing_permissions_writes_the_config_node_without_pii_in_the_audit() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let mine = tenant();
    let store = store_id();

    // Seed a store's people directly through the seam: an employee with a PIN, a role, an assignment.
    seed_a_cashier(&people, store).await;

    let router = people_publish_app(
        admin,
        people.clone(),
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/people/publish",
            &serde_json::json!({
                "tenant_id": mine.as_ulid().to_string(),
                "store_id": store.as_ulid().to_string(),
            }),
            &cookie,
        ))
        .await
        .expect("route publish");
    assert_eq!(published.status(), StatusCode::OK);
    let version = json_body(published).await["config_version_id"]
        .as_str()
        .expect("a version id")
        .to_owned();
    assert!(!version.is_empty());

    // The compiled node landed on the store layer, flattening the role and carrying the PIN hash.
    let state = config
        .load(mine, store)
        .await
        .expect("load")
        .expect("a published tree");
    let node = &state.record.layers[2]["permissions"];
    assert_eq!(
        node["store_id"],
        serde_json::json!(store.as_ulid().to_string())
    );
    let staff = node["staff"].as_array().expect("staff array");
    assert_eq!(staff.len(), 1);
    assert_eq!(staff[0]["code"], serde_json::json!("C01"));
    assert_eq!(
        staff[0]["permissions"],
        serde_json::json!(["billing.discount.apply"])
    );
    assert_eq!(
        staff[0]["pin_phc"],
        serde_json::json!("argon2id$phc$alice"),
        "the PIN hash rides to the store for offline verification"
    );

    // The audit records the publish — the config version and staff count, never a name or PIN.
    let recorded = audit.list(None, 10).await.expect("list audit");
    let entry = recorded
        .iter()
        .find(|entry| entry.action == "permissions.publish")
        .expect("the publish was recorded");
    let dump = serde_json::to_string(&entry.after).expect("serialize");
    assert!(dump.contains("staff_count"), "the staff count is recorded");
    assert!(
        !dump.contains("Alice"),
        "no employee name in the audit trail"
    );
    assert!(
        !dump.to_lowercase().contains("argon2"),
        "no PIN hash in the audit trail"
    );
}
