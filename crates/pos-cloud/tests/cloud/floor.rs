// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The floor and kitchen routes (ADR-0072): areas, tables, stations and routing rules, the floor
//! publish, a station's late threshold and ticket language, and table QR tokens.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditRecorder, AuditSink, AuditStore, NoopAuditRecorder};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::catalog::{CatalogStore, Course};
use pos_cloud::floorplan::{NewTable, TableStore, TableUpdate};
use pos_cloud::qr::TableTokenSecret;
use pos_cloud::registry::EntityStatus;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::{AreaId, CourseId, TableId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeAudit, FakeCatalog, FakeConfigTrees, FakeFloor, FakeKeys, FakeRollups,
    FakeWebhooks, admin_cookie, app_all, clock, delete_with_cookie, etag_of, get_with_cookie,
    json_body, patch_with_etag, post_with_cookie, provisioned_admin, role_session_cookie, store_id,
    tenant,
};

/// The main app merged with the floor & kitchen router, wired to a caller-supplied audit recorder
/// (Track M2, ADR-0072).
fn floor_app_with_audit(
    admin: FakeAdmin,
    floor: FakeFloor,
    audit: Arc<dyn AuditRecorder>,
    catalog: FakeCatalog,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::floor_router(floor, admin, clock(), audit, catalog))
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end lifecycle over the floor & kitchen routes — create an area, a table, a \
              station, and a routing rule; reject a rule matching neither item nor course; list each; \
              archive the area; remove the rule — kept together against the same data"
)]
async fn floor_routes_crud_lifecycle_audited() {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let router = floor_app_with_audit(
        admin,
        FakeFloor::default(),
        Arc::new(AuditSink::new(audit.clone())),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Create an area.
    let area = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/areas",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid, "name": "Terrace" }),
            &cookie,
        ))
        .await
        .expect("route create area");
    assert_eq!(area.status(), StatusCode::CREATED);
    let area_etag = etag_of(&area);
    let area_id = json_body(area).await["id"].as_str().expect("id").to_owned();

    // Create a table in that area, placed on the grid.
    let table = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/tables",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "area_id": area_id,
                "name": "T1",
                "seats": 4,
                "grid_column": 0,
                "grid_row": 0,
            }),
            &cookie,
        ))
        .await
        .expect("route create table");
    assert_eq!(table.status(), StatusCode::CREATED);

    // Create a station.
    let station = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/stations",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "name": "Oven",
                "is_default": true,
            }),
            &cookie,
        ))
        .await
        .expect("route create station");
    assert_eq!(station.status(), StatusCode::CREATED);
    let station_id = json_body(station).await["id"]
        .as_str()
        .expect("id")
        .to_owned();

    // A routing rule that matches neither an item nor a course is refused.
    let bad_rule = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/routing",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "station_id": station_id,
            }),
            &cookie,
        ))
        .await
        .expect("route bad rule");
    assert_eq!(bad_rule.status(), StatusCode::BAD_REQUEST);

    // A well-formed item rule is accepted.
    let item_ulid = Ulid::from_u128(0xB0B).to_string();
    let rule = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/routing",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "station_id": station_id,
                "menu_item_id": item_ulid,
            }),
            &cookie,
        ))
        .await
        .expect("route create rule");
    assert_eq!(rule.status(), StatusCode::CREATED);
    let rule_id = json_body(rule).await["id"].as_str().expect("id").to_owned();

    // Each list reads back one row.
    for path in [
        format!("/admin/floor/areas?tenant_id={tenant_ulid}&store_id={store_ulid}"),
        format!("/admin/floor/tables?tenant_id={tenant_ulid}&store_id={store_ulid}"),
        format!("/admin/kitchen/stations?tenant_id={tenant_ulid}&store_id={store_ulid}"),
        format!("/admin/kitchen/routing?tenant_id={tenant_ulid}&store_id={store_ulid}"),
    ] {
        let listed = router
            .clone()
            .oneshot(get_with_cookie(&path, &cookie))
            .await
            .expect("route list");
        assert_eq!(listed.status(), StatusCode::OK, "{path}");
        assert_eq!(
            json_body(listed).await.as_array().expect("array").len(),
            1,
            "{path}"
        );
    }

    // Archive the area, at the version its creation returned (ADR-0094).
    let archived = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/floor/areas/{area_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Terrace", "status": "archived" }),
            &cookie,
            &area_etag,
        ))
        .await
        .expect("route archive area");
    assert_eq!(archived.status(), StatusCode::NO_CONTENT);
    assert_ne!(
        etag_of(&archived),
        area_etag,
        "a write moves the record to a new version, and says which"
    );

    // A second console still holding the version from before that archive is refused, and the first
    // edit stands. Areas, tables and stations share one guard, so proving it once here proves it for
    // the family; the per-entity wiring is what the lifecycle above exercises.
    let stale = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/floor/areas/{area_id}"),
            &serde_json::json!({ "tenant_id": tenant_ulid, "name": "Patio", "status": "active" }),
            &cookie,
            &area_etag,
        ))
        .await
        .expect("route stale area update");
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
    let after = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/floor/areas/{area_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route read area back");
    assert_eq!(
        json_body(after).await["name"],
        serde_json::json!("Terrace"),
        "the refused write changed nothing"
    );

    // Remove the routing rule.
    let removed = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/kitchen/routing/{rule_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route remove rule");
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    // The write path is audited: the actions were recorded (floor data is not PII).
    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let actions: Vec<&str> = recorded.iter().map(|entry| entry.action.as_str()).collect();
    for action in [
        "floor.area.create",
        "floor.table.create",
        "kitchen.station.create",
        "kitchen.routing.create",
        "floor.area.update",
        "kitchen.routing.remove",
    ] {
        assert!(actions.contains(&action), "missing audit {action}");
    }
}

/// A routing rule may only match a course the tenant actually carries
/// ([ADR-0130](../../../../docs/adr/0130-a-course-is-something-the-catalog-names.md) decision 6).
///
/// This was the sharpest form of the course having no entity behind it: the write checked only that
/// `menu_item_id` and `course_id` were not both set, because there was nowhere to look. An operator
/// could send "any line on course X to the pastry station", the rule published, the store honoured
/// it — and it matched nothing, for ever, silently, because no line could carry that id either.
///
/// Refused at the write rather than at publish, the way the menu graph's cycle is, and for the
/// reason the record gives: *a rule that can never match is a rule an operator will never find out
/// about.*
#[tokio::test]
async fn a_routing_rule_may_only_name_a_course_the_tenant_carries() {
    let catalog = FakeCatalog::default();
    let router = floor_app_with_audit(
        provisioned_admin(),
        FakeFloor::default(),
        Arc::new(NoopAuditRecorder),
        catalog.clone(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let station_ulid = Ulid::from_u128(0x57A).to_string();

    let rule = |course: &str| {
        serde_json::json!({
            "tenant_id": tenant_ulid,
            "store_id": store_ulid,
            "station_id": station_ulid,
            "course_id": course,
        })
    };

    // Nothing authored yet, so every course id is one the tenant does not carry.
    let unknown = Ulid::from_u128(0x00C0_FFEE).to_string();
    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/routing",
            &rule(&unknown),
            &cookie,
        ))
        .await
        .expect("route the rule");
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "a rule naming a course nothing carries is refused at the write"
    );
    let body = json_body(refused).await;
    assert_eq!(
        body["error"]["details"][0]["field"], "course_id",
        "the refusal names the field that is wrong: {body}"
    );
    assert_eq!(
        body["error"]["details"][0]["reason"], "NOT_FOUND",
        "and says why, rather than leaving the console to guess: {body}"
    );

    // Author the course, and the same rule is accepted.
    let course_id = CourseId::new(Ulid::from_u128(0x00C0_FFEE));
    catalog
        .create_course(&Course {
            course_id,
            tenant_id: tenant(),
            name: "Starters".to_owned(),
            sort: 10,
            status: EntityStatus::Active,
        })
        .await
        .expect("author the course");
    let accepted = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/routing",
            &rule(&unknown),
            &cookie,
        ))
        .await
        .expect("route the rule");
    assert_eq!(
        accepted.status(),
        StatusCode::CREATED,
        "the same rule is accepted once the course exists"
    );
}

/// An archived course is as gone as one that never existed.
///
/// The compiler does not publish an archived course, so a rule naming one would be exactly as dead
/// as a rule naming an id nobody ever authored. One answer for both, so they cannot drift into
/// different behaviour later — and an operator who retires "Dessert" and then writes a rule against
/// it finds out now rather than at the next service.
#[tokio::test]
async fn a_routing_rule_may_not_name_an_archived_course() {
    let catalog = FakeCatalog::default();
    let router = floor_app_with_audit(
        provisioned_admin(),
        FakeFloor::default(),
        Arc::new(NoopAuditRecorder),
        catalog.clone(),
    );
    let cookie = admin_cookie(&router).await;
    let course_id = CourseId::new(Ulid::from_u128(0xDEAD));
    catalog
        .create_course(&Course {
            course_id,
            tenant_id: tenant(),
            name: "Dessert".to_owned(),
            sort: 30,
            status: EntityStatus::Archived,
        })
        .await
        .expect("author the retired course");

    let refused = router
        .oneshot(post_with_cookie(
            "/admin/kitchen/routing",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
                "station_id": Ulid::from_u128(0x57A).to_string(),
                "course_id": course_id.as_ulid().to_string(),
            }),
            &cookie,
        ))
        .await
        .expect("route the rule");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
}

/// The main app (sharing one config-tree store) merged with the floor CRUD and floor-publish routers,
/// so a test can author master data and then publish it, reading the effective config back (ADR-0072).
fn floor_publish_app(
    admin: FakeAdmin,
    floor: FakeFloor,
    config: FakeConfigTrees,
    audit: Arc<dyn AuditRecorder>,
    catalog: FakeCatalog,
) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        config.clone(),
        FakeWebhooks::default(),
    );
    http::router(app)
        .merge(http::floor_router(
            floor.clone(),
            admin.clone(),
            clock(),
            Arc::clone(&audit),
            catalog,
        ))
        .merge(http::floor_publish_router(
            floor,
            config,
            admin,
            clock(),
            audit,
        ))
}

#[tokio::test]
async fn floor_publish_compiles_and_writes_the_floor_and_stations_nodes() {
    let admin = provisioned_admin();
    let config = FakeConfigTrees::default();
    let router = floor_publish_app(
        admin,
        FakeFloor::default(),
        config,
        Arc::new(NoopAuditRecorder),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Author one area with a table and one station via the CRUD routes.
    let area = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/areas",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid, "name": "Terrace" }),
            &cookie,
        ))
        .await
        .expect("create area");
    let area_id = json_body(area).await["id"].as_str().expect("id").to_owned();
    let table = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/tables",
            &serde_json::json!({
                "tenant_id": tenant_ulid, "store_id": store_ulid, "area_id": area_id,
                "name": "T1", "seats": 4,
            }),
            &cookie,
        ))
        .await
        .expect("create table");
    assert_eq!(table.status(), StatusCode::CREATED);
    let station = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/stations",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid, "name": "Oven", "is_default": true }),
            &cookie,
        ))
        .await
        .expect("create station");
    assert_eq!(station.status(), StatusCode::CREATED);

    // Publish: the two nodes compile and version through the config tree.
    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
        ))
        .await
        .expect("publish floor");
    assert_eq!(published.status(), StatusCode::OK);

    // The effective config now carries top-level `floor` and `stations` nodes with the authored data.
    let effective = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("read effective config");
    assert_eq!(effective.status(), StatusCode::OK);
    let doc = json_body(effective).await;
    assert_eq!(
        doc["floor"]["areas"][0]["name"],
        serde_json::json!("Terrace")
    );
    assert_eq!(
        doc["floor"]["areas"][0]["tables"][0]["label"],
        serde_json::json!("T1")
    );
    assert_eq!(
        doc["stations"]["stations"][0]["name"],
        serde_json::json!("Oven")
    );
    assert!(
        doc["stations"]["default_station_id"].as_str().is_some(),
        "the default station rode into the node"
    );
}

/// Each kitchen station says when its tickets are late
/// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 2): a threshold outside a minute to an hour is refused naming the field, a write holds to
/// the version it read and is audited with the threshold, and the publish carries a station's
/// threshold and leaves it off a station that sets none, so the edge reads its ten minutes there.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one station's threshold from the refused writes through the guarded update to the \
              published node, against the same data"
)]
async fn a_stations_late_threshold_is_checked_guarded_audited_and_published() {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let router = floor_publish_app(
        admin,
        FakeFloor::default(),
        FakeConfigTrees::default(),
        Arc::new(AuditSink::new(audit.clone())),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let station = |name: &str, late: serde_json::Value| {
        serde_json::json!({
            "tenant_id": tenant_ulid,
            "store_id": store_ulid,
            "name": name,
            "late_after_seconds": late,
        })
    };

    for refused in [
        serde_json::json!(59),
        serde_json::json!(3601),
        serde_json::json!(-60),
    ] {
        let response = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/kitchen/stations",
                &station("Oven", refused.clone()),
                &cookie,
            ))
            .await
            .expect("route the create");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{refused}");
        let body = json_body(response).await;
        assert!(
            body.to_string().contains("late_after_seconds"),
            "the refusal names the field: {body}"
        );
    }

    let oven = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/stations",
            &station("Oven", serde_json::json!(240)),
            &cookie,
        ))
        .await
        .expect("create the oven");
    assert_eq!(oven.status(), StatusCode::CREATED);
    let oven_etag = etag_of(&oven);
    let oven_id = json_body(oven).await["id"].as_str().expect("id").to_owned();
    let bar = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/stations",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid, "name": "Bar" }),
            &cookie,
        ))
        .await
        .expect("create the bar");
    assert_eq!(bar.status(), StatusCode::CREATED);

    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/kitchen/stations/{oven_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("read the oven");
    assert_eq!(json_body(read).await["late_after_seconds"], 240);

    // An update out of bounds is refused before anything is compared or written.
    let update = |late: serde_json::Value| {
        serde_json::json!({
            "tenant_id": tenant_ulid,
            "name": "Oven",
            "late_after_seconds": late,
            "status": "active",
        })
    };
    let out_of_bounds = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/kitchen/stations/{oven_id}"),
            &update(serde_json::json!(3601)),
            &cookie,
            &oven_etag,
        ))
        .await
        .expect("route the update");
    assert_eq!(out_of_bounds.status(), StatusCode::BAD_REQUEST);

    let saved = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/kitchen/stations/{oven_id}"),
            &update(serde_json::json!(300)),
            &cookie,
            &oven_etag,
        ))
        .await
        .expect("route the update");
    assert_eq!(saved.status(), StatusCode::NO_CONTENT);

    // A console still holding the version from before that save is refused, and the save stands.
    let stale = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/kitchen/stations/{oven_id}"),
            &update(serde_json::json!(900)),
            &cookie,
            &oven_etag,
        ))
        .await
        .expect("route the stale update");
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);

    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let updated = recorded
        .iter()
        .find(|entry| entry.action == "kitchen.station.update")
        .expect("the save is audited");
    assert_eq!(
        updated
            .after
            .as_ref()
            .map(|after| after["late_after_seconds"].clone()),
        Some(serde_json::json!(300))
    );

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
        ))
        .await
        .expect("publish the floor");
    assert_eq!(published.status(), StatusCode::OK);
    let effective = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("read the effective config");
    let doc = json_body(effective).await;
    let stations = doc["stations"]["stations"]
        .as_array()
        .expect("the stations node");
    let named = |name: &str| {
        stations
            .iter()
            .find(|station| station["name"] == name)
            .cloned()
            .expect("the station is published")
    };
    assert_eq!(named("Oven")["late_after_seconds"], 300);
    assert!(
        named("Bar").get("late_after_seconds").is_none(),
        "a station that sets no threshold publishes none"
    );
}

/// Each kitchen station says which language its tickets print in
/// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 2), from the receipt language's own tokens: one this release does not know, or
/// `RECEIPT_LANGUAGE_UNSPECIFIED`, is refused `400 INVALID_ENUM_VALUE` naming the field; a write holds
/// to the version it read and is audited with the language; and the publish carries a station's
/// language and leaves it off a station that sets none, so the edge prints there as it always did.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one station's language from the refused writes through the guarded update to the \
              published node, against the same data"
)]
async fn a_stations_ticket_language_is_checked_guarded_audited_and_published() {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let router = floor_publish_app(
        admin,
        FakeFloor::default(),
        FakeConfigTrees::default(),
        Arc::new(AuditSink::new(audit.clone())),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let station = |name: &str, language: &str| {
        serde_json::json!({
            "tenant_id": tenant_ulid,
            "store_id": store_ulid,
            "name": name,
            "ticket_language": language,
        })
    };

    for refused in ["RECEIPT_LANGUAGE_JA", "RECEIPT_LANGUAGE_UNSPECIFIED", "EN"] {
        let response = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/kitchen/stations",
                &station("Oven", refused),
                &cookie,
            ))
            .await
            .expect("route the create");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{refused}");
        let body = json_body(response).await.to_string();
        assert!(
            body.contains("ticket_language") && body.contains("INVALID_ENUM_VALUE"),
            "the refusal names the field: {body}"
        );
    }

    let oven = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/stations",
            &station("Oven", "RECEIPT_LANGUAGE_EN"),
            &cookie,
        ))
        .await
        .expect("create the oven");
    assert_eq!(oven.status(), StatusCode::CREATED);
    let oven_etag = etag_of(&oven);
    let oven_id = json_body(oven).await["id"].as_str().expect("id").to_owned();
    let bar = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/stations",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid, "name": "Bar" }),
            &cookie,
        ))
        .await
        .expect("create the bar");
    assert_eq!(bar.status(), StatusCode::CREATED);

    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/kitchen/stations/{oven_id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("read the oven");
    assert_eq!(
        json_body(read).await["ticket_language"],
        "RECEIPT_LANGUAGE_EN"
    );

    let update = |language: &str| {
        serde_json::json!({
            "tenant_id": tenant_ulid,
            "name": "Oven",
            "ticket_language": language,
            "status": "active",
        })
    };
    let unknown = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/kitchen/stations/{oven_id}"),
            &update("RECEIPT_LANGUAGE_JA"),
            &cookie,
            &oven_etag,
        ))
        .await
        .expect("route the update");
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

    let saved = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/kitchen/stations/{oven_id}"),
            &update("RECEIPT_LANGUAGE_VI"),
            &cookie,
            &oven_etag,
        ))
        .await
        .expect("route the update");
    assert_eq!(saved.status(), StatusCode::NO_CONTENT);

    // A console still holding the version from before that save is refused, and the save stands.
    let stale = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/kitchen/stations/{oven_id}"),
            &update("RECEIPT_LANGUAGE_COUNTRY"),
            &cookie,
            &oven_etag,
        ))
        .await
        .expect("route the stale update");
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);

    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let updated = recorded
        .iter()
        .find(|entry| entry.action == "kitchen.station.update")
        .expect("the save is audited");
    assert_eq!(
        updated
            .after
            .as_ref()
            .map(|after| after["ticket_language"].clone()),
        Some(serde_json::json!("RECEIPT_LANGUAGE_VI"))
    );

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
        ))
        .await
        .expect("publish the floor");
    assert_eq!(published.status(), StatusCode::OK);
    let effective = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("read the effective config");
    let doc = json_body(effective).await;
    let stations = doc["stations"]["stations"]
        .as_array()
        .expect("the stations node");
    let named = |name: &str| {
        stations
            .iter()
            .find(|station| station["name"] == name)
            .cloned()
            .expect("the station is published")
    };
    assert_eq!(named("Oven")["ticket_language"], "RECEIPT_LANGUAGE_VI");
    assert!(
        named("Bar").get("ticket_language").is_none(),
        "a station that sets no language publishes none"
    );
}

#[tokio::test]
async fn table_qr_mints_a_signed_token_per_active_table() {
    let admin = provisioned_admin();
    let floor = FakeFloor::default();
    let mine = tenant();
    let store = store_id();
    let area = AreaId::new(Ulid::from_u128(1));
    let active = TableId::new(Ulid::from_u128(0xA1));
    let archived = TableId::new(Ulid::from_u128(0xA2));
    let mut archived_version = None;
    for (table_id, label) in [(active, "T1"), (archived, "T2")] {
        let version = TableStore::create(
            &floor,
            &NewTable {
                table_id,
                tenant_id: mine,
                store_id: store,
                area_id: area,
                label: label.to_owned(),
                seats: 4,
                position: None,
            },
        )
        .await
        .expect("seed table");
        if table_id == archived {
            archived_version = Some(version);
        }
    }
    // Archive T2 — an archived table is not printed on the QR sheet. The write is conditional on the
    // version the create returned (ADR-0094), the same token a reader would have been handed.
    TableStore::update(
        &floor,
        &TableUpdate {
            table_id: archived,
            tenant_id: mine,
            area_id: area,
            label: "T2".to_owned(),
            seats: 4,
            position: None,
            status: EntityStatus::Archived,
        },
        &archived_version.expect("T2 was seeded"),
    )
    .await
    .expect("archive T2");

    let secret = TableTokenSecret::new("a-test-secret");
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ))
    .merge(http::table_qr_router(floor, admin, clock(), secret.clone()));
    let cookie = admin_cookie(&router).await;

    let response = router
        .oneshot(get_with_cookie(
            &format!(
                "/admin/floor/qr?tenant_id={}&store_id={}",
                mine.as_ulid(),
                store.as_ulid()
            ),
            &cookie,
        ))
        .await
        .expect("route table qr");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let tokens = body["tokens"].as_array().expect("tokens array");
    assert_eq!(tokens.len(), 1, "only the active table is printed");
    let token = tokens[0]["token"].as_str().expect("a token");
    // The minted token verifies and names the active table — the same value the guest QR carries.
    let table_ref = pos_cloud::qr::verify_table_token(&secret, token).expect("verifies");
    assert_eq!(table_ref.table_id, active);
    assert_eq!(table_ref.store_id, store);
}

// --- pinned before `admin_create_routing` is split (ADR-0170 decision 7) -------------------------

/// A routing rule body for the test store and a station, with `target` merged over it.
fn routing_body(target: &serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "station_id": Ulid::from_u128(0x57A).to_string(),
    });
    if let (Some(fields), Some(target)) = (body.as_object_mut(), target.as_object()) {
        for (key, value) in target {
            fields.insert(key.clone(), value.clone());
        }
    }
    body
}

/// A routing rule is refused, field by field, before anything is read or written: a role that may
/// not author the floor, an id that is not a ULID, an item or course id that is not one, and a rule
/// naming both an item and a course. None of them records anything.
#[tokio::test]
async fn a_routing_rule_is_refused_field_by_field_and_records_nothing() {
    let admin = provisioned_admin();
    let audit = FakeAudit::default();
    let router = floor_app_with_audit(
        admin.clone(),
        FakeFloor::default(),
        Arc::new(AuditSink::new(audit.clone())),
        FakeCatalog::default(),
    );
    // The owner logs in first: the login names no email, so it is the only admin when it does.
    let cookie = admin_cookie(&router).await;
    let item = Ulid::from_u128(0xB0B).to_string();

    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let by_ops = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/routing",
            &routing_body(&serde_json::json!({ "menu_item_id": item })),
            &ops,
        ))
        .await
        .expect("route the rule");
    assert_eq!(by_ops.status(), StatusCode::FORBIDDEN);

    for (target, fields) in [
        (
            serde_json::json!({ "station_id": "not-a-ulid", "menu_item_id": "nor-this" }),
            vec!["station_id"],
        ),
        (
            serde_json::json!({ "menu_item_id": "not-a-ulid", "course_id": "nor-this" }),
            vec!["menu_item_id"],
        ),
        (
            serde_json::json!({ "course_id": "not-a-ulid" }),
            vec!["course_id"],
        ),
        (
            serde_json::json!({ "menu_item_id": item, "course_id": Ulid::from_u128(0xC0).to_string() }),
            vec!["menu_item_id", "course_id"],
        ),
    ] {
        let refused = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/kitchen/routing",
                &routing_body(&target),
                &cookie,
            ))
            .await
            .expect("route the rule");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{target}");
        let body = json_body(refused).await;
        let named: Vec<&str> = body["error"]["details"]
            .as_array()
            .expect("details")
            .iter()
            .filter_map(|detail| detail["field"].as_str())
            .collect();
        assert_eq!(named, fields, "{target}: {body}");
    }
    assert!(audit.entries.lock().expect("lock").is_empty());
}

/// A routing rule is answered with its id and recorded with what it routes: the station, the item
/// or the course, and its sort.
#[tokio::test]
async fn a_routing_rule_is_answered_and_recorded_with_what_it_routes() {
    let audit = FakeAudit::default();
    let router = floor_app_with_audit(
        provisioned_admin(),
        FakeFloor::default(),
        Arc::new(AuditSink::new(audit.clone())),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let item = Ulid::from_u128(0xB0B).to_string();
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/kitchen/routing",
            &routing_body(&serde_json::json!({ "menu_item_id": item, "sort": 7 })),
            &cookie,
        ))
        .await
        .expect("route the rule");
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();

    let entries = audit.entries.lock().expect("lock").clone();
    let [entry] = entries.as_slice() else {
        panic!("one audit entry: {entries:?}");
    };
    assert_eq!(entry.action, "kitchen.routing.create");
    assert_eq!(entry.entity_type, "station_routing_rule");
    assert_eq!(entry.entity_id, id);
    assert_eq!(entry.tenant_id, Some(tenant()));
    assert_eq!(entry.before, None);
    assert_eq!(
        entry.after,
        Some(serde_json::json!({
            "id": id,
            "station_id": Ulid::from_u128(0x57A).to_string(),
            "menu_item_id": item,
            "course_id": null,
            "sort": 7,
        }))
    );

    let listed = json_body(
        router
            .oneshot(get_with_cookie(
                &format!(
                    "/admin/kitchen/routing?tenant_id={}&store_id={}",
                    tenant().as_ulid(),
                    store_id().as_ulid()
                ),
                &cookie,
            ))
            .await
            .expect("route the list"),
    )
    .await;
    assert_eq!(listed[0]["rule_id"], id.as_str());
    assert_eq!(listed[0]["menu_item_id"], item.as_str());
    assert_eq!(listed[0]["sort"], 7);
}
