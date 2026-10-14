// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store's OTA report, on the route that says which store it is (ADR-0078).

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditRecorder, NoopAuditRecorder};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::auth::apikey::Scope;
use pos_cloud::fleet::PrintAgentStanding;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeCatalog, FakeConfigTrees, FakeDevices, FakeKeys, FakeOtaReports, FakePeople, FakeRollups,
    FakeWebhooks, admin_cookie, app_all, catalog_app, device_app, get_with_cookie, issue_key,
    issue_store_key, json_body, people_publish_app, post_json, post_json_bearer, post_with_cookie,
    provisioned_admin, role_session_cookie, seed_a_cashier, store_id, store_report_app, tenant,
    ulid_text,
};

// ---------------------------------------------------------------------------------------------
// R5-a: a store reports on a route that can say who it is
// ---------------------------------------------------------------------------------------------

/// The `/sync` route establishes the tenant from the key instead of believing the body — which is the
/// whole reason ADR-0097 said a shared secret could not make the `/internal` shape honest.
#[tokio::test]
async fn a_store_report_takes_its_tenant_from_the_key_not_the_body() {
    let reports = FakeOtaReports::default();
    let (router, keys) = store_report_app(reports.clone());
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);

    let accepted = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/report", store_id()),
            &serde_json::json!({ "installed": "1.5.0", "self_test_passed": true }),
            &token,
        ))
        .await
        .expect("route the report");
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);

    let recorded = reports.rows.lock().expect("lock").clone();
    assert_eq!(recorded.len(), 1);
    let (recorded_tenant, recorded_store, installed, self_test) = &recorded[0];
    // The body never carried either id.
    assert_eq!(*recorded_tenant, tenant());
    assert_eq!(*recorded_store, store_id());
    assert_eq!(installed, "1.5.0");
    assert_eq!(*self_test, Some(true));
}

/// A box that has never self-tested says so by omission, and the route keeps that distinct from
/// `false` (ADR-0078 Amendment 1) on the new surface too.
#[tokio::test]
async fn a_store_report_can_say_it_has_never_self_tested() {
    let reports = FakeOtaReports::default();
    let (router, keys) = store_report_app(reports.clone());
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);

    let accepted = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/report", store_id()),
            &serde_json::json!({ "installed": "1.5.0" }),
            &token,
        ))
        .await
        .expect("route the report");
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        reports.rows.lock().expect("lock").clone()[0].3,
        None,
        "absent, not false"
    );
}

/// The refusals: no key at all, and a key without the scope the OTA path runs on.
#[tokio::test]
async fn a_store_report_needs_a_scoped_key() {
    let (router, keys) = store_report_app(FakeOtaReports::default());
    let path = format!("/sync/stores/{}/report", store_id());
    let body = serde_json::json!({ "installed": "1.5.0" });

    let unauthenticated = router
        .clone()
        .oneshot(post_json(&path, &body))
        .await
        .expect("route the unauthenticated report");
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let wrong_scope = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let refused = router
        .oneshot(post_json_bearer(&path, &body, &wrong_scope))
        .await
        .expect("route the wrong-scope report");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
}

/// A blank version is refused the same way on both surfaces — the shared write helper is what makes
/// that true rather than merely intended.
#[tokio::test]
async fn a_store_report_with_a_blank_version_is_refused() {
    let (router, keys) = store_report_app(FakeOtaReports::default());
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);

    let refused = router
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/report", store_id()),
            &serde_json::json!({ "installed": "   " }),
            &token,
        ))
        .await
        .expect("route the blank report");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "installed");
}

#[tokio::test]
async fn a_menus_prices_need_the_revenue_permission_not_plain_read() {
    // production-readiness S5. `ReadRevenue` was carved out of `Read` because prices are T2, and the
    // placements read — which carries every item's price on every channel of the menu — sat behind
    // plain `Read`, which Ops and Viewer both hold. The console refused a Viewer the revenue report
    // and handed them the price book.
    let admin = provisioned_admin();
    let router = catalog_app(admin.clone(), FakeCatalog::default());
    let tenant = ulid_text(1);
    let menu = ulid_text(10);
    let list = format!("/admin/catalog/menus/{menu}/placements?tenant_id={tenant}");

    for (role, token) in [
        (AdminRole::Viewer, "viewer-prices"),
        (AdminRole::Ops, "ops-prices"),
    ] {
        let cookie = role_session_cookie(&admin, role, token).await;
        let refused = router
            .clone()
            .oneshot(get_with_cookie(&list, &cookie))
            .await
            .expect("route list placements");
        assert_eq!(
            refused.status(),
            StatusCode::FORBIDDEN,
            "{role:?} holds console.data.read but not console.reports.revenue"
        );
    }

    // An Admin holds `ReadRevenue` and still reads the menu.
    let cookie = role_session_cookie(&admin, AdminRole::Admin, "admin-prices").await;
    let allowed = router
        .oneshot(get_with_cookie(&list, &cookie))
        .await
        .expect("route list placements");
    assert_eq!(allowed.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_console_config_read_never_carries_a_staff_pin_hash() {
    // production-readiness S7, found while verifying S5. `GET /admin/stores/{id}/config` returns the
    // whole effective document under plain `Read`, and the document's `permissions` node carries each
    // member of staff's Argon2id PIN hash — T1, the credential the edge verifies a sign-in against.
    // A read-only console account could lift the PIN hash of every member of staff in the fleet.
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let mine = tenant();
    let store = StoreId::new(Ulid::from_u128(77));
    seed_a_cashier(&people, store).await;

    let router = people_publish_app(
        admin,
        people,
        config,
        Arc::new(NoopAuditRecorder) as Arc<dyn AuditRecorder>,
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = mine.as_ulid().to_string();
    let store_ulid = store.as_ulid().to_string();

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/people/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
        ))
        .await
        .expect("route publish");
    assert_eq!(published.status(), StatusCode::OK);

    let effective = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("read effective config");
    assert_eq!(effective.status(), StatusCode::OK);
    let doc = json_body(effective).await;

    let staff = doc["permissions"]["staff"]
        .as_array()
        .expect("the staff array is still there");
    assert_eq!(staff.len(), 1);
    assert_eq!(
        staff[0]["code"],
        serde_json::json!("C01"),
        "an owner still sees who is on the node — only the credential is withheld"
    );
    assert!(
        staff[0].get("pin_phc").is_none(),
        "the field is removed, not blanked: an empty string would read as 'no PIN set', which is a \
         different and real state"
    );
    assert!(
        !serde_json::to_string(&doc)
            .expect("serialize")
            .contains("argon2id"),
        "no hash anywhere in the document"
    );
}

/// A store's config names its staff only to a role holding `console.people.read`
/// ([ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 9).
///
/// The `permissions` node lists every member of staff the store authorises, with their name and
/// staff code. The config read needs only `console.data.read`, so without this a Viewer refused the
/// roster could read every name in the fleet through the config screen, one store at a time. Only
/// the two fields go — the member, what they may do and the rest of the document stay — and both
/// reads take them out, because a past version holds past names.
#[tokio::test]
async fn a_config_read_names_staff_only_to_a_role_that_reads_staff_records() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let mine = tenant();
    let store = store_id();
    let (alice, _cashier) = seed_a_cashier(&people, store).await;

    let router = people_publish_app(
        admin.clone(),
        people,
        config,
        Arc::new(NoopAuditRecorder) as Arc<dyn AuditRecorder>,
    );
    let owner = admin_cookie(&router).await;
    let tenant_ulid = mine.as_ulid().to_string();
    let store_ulid = store.as_ulid().to_string();

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/people/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &owner,
        ))
        .await
        .expect("route publish");
    assert_eq!(published.status(), StatusCode::OK);
    let version_ulid = json_body(published).await["config_version_id"]
        .as_str()
        .expect("a version id")
        .to_owned();
    let reads = [
        format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
        format!(
            "/admin/stores/{store_ulid}/config/versions/{version_ulid}?tenant_id={tenant_ulid}"
        ),
    ];

    for (role, token, names_staff) in [
        (AdminRole::Viewer, "viewer-config", false),
        (AdminRole::Ops, "ops-config", false),
        (AdminRole::Admin, "admin-config", true),
        (AdminRole::Owner, "owner-config", true),
    ] {
        let cookie = role_session_cookie(&admin, role, token).await;
        for url in &reads {
            let response = router
                .clone()
                .oneshot(get_with_cookie(url, &cookie))
                .await
                .expect("route a config read");
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{role:?} reads {url}: the config read needs only console.data.read"
            );
            let doc = json_body(response).await;
            let member = doc["permissions"]["staff"][0]
                .as_object()
                .expect("the member is still listed");
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
                !member.contains_key("pin_phc"),
                "{role:?}: never the credential (S7)"
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
                    "{role:?} lacks console.people.read, so the name and code are removed: \
                     {member:?}"
                );
                assert!(
                    !doc.to_string().contains("Alice"),
                    "{role:?}: the name is nowhere in the document"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_heartbeat_reports_its_print_agents_and_an_empty_list_clears_them() {
    // ADR-0112: for a store whose edge is not in the shop, the print agent is the one moving part
    // with no rail to the cloud of its own. The heartbeat is where its standing arrives, and the
    // three cases below are the three answers a store can give — each of which the console renders
    // differently, and two of which look identical if the wire cannot tell them apart.
    let keys = FakeKeys::default();
    let config_trees = FakeConfigTrees::default();
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        provisioned_admin(),
        config_trees.clone(),
        FakeWebhooks::default(),
    ));
    let store_ulid = store_id().as_ulid().to_string();
    let uri = format!("/sync/stores/{store_ulid}/heartbeat");
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);

    // One agent, holding a ticket. Two opaque ids and a duration — no document, no line, no name.
    let beat = router
        .clone()
        .oneshot(post_json_bearer(
            &uri,
            &serde_json::json!({
                "print_agents": [{
                    "agent_device_id": "TILL-KITCHEN",
                    "paired_device_id": "PAIR-A",
                    "oldest_unacknowledged_secs": 420,
                }],
            }),
            &token,
        ))
        .await
        .expect("route the heartbeat");
    assert_eq!(beat.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        config_trees.recorded_print_agents(tenant(), store_id()),
        Some(vec![PrintAgentStanding {
            agent_device_id: "TILL-KITCHEN".to_owned(),
            paired_device_id: "PAIR-A".to_owned(),
            oldest_unacknowledged_secs: Some(420),
        }]),
        "the standing the store reported is the standing the cloud recorded"
    );

    // A beat that says nothing about agents leaves the recorded standing alone — an older edge must
    // not silently erase what a newer one reported.
    let quiet = router
        .clone()
        .oneshot(post_json_bearer(&uri, &serde_json::json!({}), &token))
        .await
        .expect("route the heartbeat");
    assert_eq!(quiet.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        config_trees
            .recorded_print_agents(tenant(), store_id())
            .map(|agents| agents.len()),
        Some(1),
        "an absent key is \"did not say\", not \"has none\""
    );

    // An *empty* list is a report, and it replaces. This is the manager-released-the-terminal case:
    // a console still showing an agent nobody is bound to is the stale answer the field exists to
    // prevent, and it is the one an absent-vs-empty conflation would produce.
    let released = router
        .oneshot(post_json_bearer(
            &uri,
            &serde_json::json!({ "print_agents": [] }),
            &token,
        ))
        .await
        .expect("route the heartbeat");
    assert_eq!(released.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        config_trees.recorded_print_agents(tenant(), store_id()),
        Some(Vec::new()),
        "a store that says it has no agent is recorded as having none, not left as it was"
    );
}

#[tokio::test]
async fn the_admin_device_read_lists_approved_devices_for_one_store_and_still_defaults_to_pending()
{
    // ADR-0112's agent picker acts on a printer already in service and names a terminal already
    // created. Until this read could show either, `set_print_agent` had no `/admin` read a console
    // could be written against at all — the same shape of gap as a leased job that carried no
    // printer address. The seam always supported both filters; only the route did not.
    //
    // Defaulting to `pending` is the other half: the onboarding queue is an existing caller that
    // passes neither parameter, and it must keep seeing exactly what it saw.
    let keys = FakeKeys::default();
    let devices = FakeDevices::default();
    let router = device_app(provisioned_admin(), keys.clone(), devices);
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let devices_uri = format!("/sync/stores/{store_ulid}/devices");

    let created = router
        .clone()
        .oneshot(post_json_bearer(
            &devices_uri,
            &serde_json::json!({
                "kind": "printer",
                "name": "Kitchen 1",
                "address": "192.168.1.50:9100",
            }),
            &token,
        ))
        .await
        .expect("route the proposal");
    let id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();

    // Still pending: an `approved` listing is empty, and the default listing is not.
    let approved = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!(
                    "/admin/devices/proposals?tenant_id={tenant_ulid}&store_id={store_ulid}&status=approved"
                ),
                &cookie,
            ))
            .await
            .expect("route the approved listing"),
    )
    .await;
    assert_eq!(
        approved.as_array().expect("array").len(),
        0,
        "a pending printer is not an approved one"
    );

    let approve = router
        .clone()
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{id}/approve?tenant_id={tenant_ulid}&connection=network"
            ),
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the approval");
    assert_eq!(approve.status(), StatusCode::NO_CONTENT);

    // Now the picker can see it — which is the whole point of the parameter.
    let approved = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!(
                    "/admin/devices/proposals?tenant_id={tenant_ulid}&store_id={store_ulid}&status=approved"
                ),
                &cookie,
            ))
            .await
            .expect("route the approved listing"),
    )
    .await;
    assert_eq!(approved.as_array().expect("array").len(), 1);
    assert_eq!(approved[0]["id"], id);
    assert!(
        approved[0]["version"].is_string(),
        "and carries the version the picker echoes back in If-Match (ADR-0094): {approved:?}"
    );

    // The onboarding queue passes neither parameter and must be unchanged: the printer it was
    // watching has been approved, so the pending queue is now empty.
    let pending = json_body(
        router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/devices/proposals?tenant_id={tenant_ulid}"),
                &cookie,
            ))
            .await
            .expect("route the default listing"),
    )
    .await;
    assert_eq!(
        pending.as_array().expect("array").len(),
        0,
        "the default is still pending-only, so an approved row does not leak into the queue"
    );

    // A status nobody defined is refused by name rather than silently narrowed to pending — being
    // handed the pending queue for `aproved` reads as \"this store has no printers\".
    let mistyped = router
        .oneshot(get_with_cookie(
            &format!("/admin/devices/proposals?tenant_id={tenant_ulid}&status=aproved"),
            &cookie,
        ))
        .await
        .expect("route the mistyped listing");
    assert_eq!(mistyped.status(), StatusCode::BAD_REQUEST);
}
