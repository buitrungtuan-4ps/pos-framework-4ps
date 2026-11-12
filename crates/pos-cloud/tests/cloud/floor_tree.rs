// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An area inside another area (ADR-0211): the parent `/admin/floor/areas` takes, the tree rule's
//! refusals, and the `floor` node published resolved.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;
use tower::ServiceExt;

use crate::floor::floor_publish_app;
use crate::{
    FakeCatalog, FakeConfigTrees, FakeFloor, admin_cookie, etag_of, get_with_cookie, json_body,
    patch_with_etag, post_with_cookie, provisioned_admin, store_id, tenant,
};

/// Creates an area, inside `parent` when given, and answers its id and etag.
async fn create_area(
    router: &Router,
    cookie: &str,
    name: &str,
    parent: Option<&str>,
) -> (String, String) {
    let created = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/areas",
            &serde_json::json!({
                "tenant_id": tenant().as_ulid().to_string(),
                "store_id": store_id().as_ulid().to_string(),
                "name": name,
                "parent_area_id": parent,
            }),
            cookie,
        ))
        .await
        .expect("create the area");
    assert_eq!(created.status(), StatusCode::CREATED, "{name}");
    let etag = etag_of(&created);
    let id = json_body(created).await["id"]
        .as_str()
        .expect("id")
        .to_owned();
    (id, etag)
}

/// Patches an area and answers the response's status and body.
async fn patch_area(
    router: &Router,
    cookie: &str,
    id: &str,
    etag: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut body = body;
    body["tenant_id"] = serde_json::json!(tenant().as_ulid().to_string());
    let response = router
        .clone()
        .oneshot(patch_with_etag(
            &format!("/admin/floor/areas/{id}"),
            &body,
            cookie,
            etag,
        ))
        .await
        .expect("patch the area");
    let status = response.status();
    let body = if status == StatusCode::NO_CONTENT {
        serde_json::Value::Null
    } else {
        json_body(response).await
    };
    (status, body)
}

/// The reason a refusal's first detail carries.
fn reason(body: &serde_json::Value) -> &str {
    body["error"]["details"][0]["reason"]
        .as_str()
        .unwrap_or_default()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one store's tree through create, every refusal, a move, and the publish, against the \
              same data"
)]
async fn an_area_sits_inside_another_and_the_floor_is_published_resolved() {
    let router = floor_publish_app(
        provisioned_admin(),
        FakeFloor::default(),
        FakeConfigTrees::default(),
        Arc::new(NoopAuditRecorder),
        FakeCatalog::default(),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let (floor, floor_etag) = create_area(&router, &cookie, "Tầng 2", None).await;
    let (vip, vip_etag) = create_area(&router, &cookie, "VIP", Some(&floor)).await;
    let (room, _) = create_area(&router, &cookie, "Phòng 1", Some(&vip)).await;
    let (ground, _) = create_area(&router, &cookie, "Tầng 1", None).await;

    // A parent the store does not have is refused, in place, on the field.
    let stranger = pos_proto::ulid::Ulid::from_u128(0xDEAD).to_string();
    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/floor/areas",
            &serde_json::json!({
                "tenant_id": tenant_ulid, "store_id": store_ulid, "name": "Lost",
                "parent_area_id": stranger,
            }),
            &cookie,
        ))
        .await
        .expect("create under a stranger");
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let body = json_body(refused).await;
    assert_eq!(reason(&body), "AREA_PARENT_NOT_FOUND");
    assert_eq!(body["error"]["details"][0]["field"], "parent_area_id");

    // The floor cannot move inside the room that sits inside it.
    let (status, body) = patch_area(
        &router,
        &cookie,
        &floor,
        &floor_etag,
        serde_json::json!({ "name": "Tầng 2", "status": "active", "parent_area_id": room }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reason(&body), "AREA_CYCLE");

    // VIP cannot be archived while the room inside it is live.
    let (status, body) = patch_area(
        &router,
        &cookie,
        &vip,
        &vip_etag,
        serde_json::json!({ "name": "VIP", "status": "archived" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reason(&body), "AREA_HAS_LIVE_CHILDREN");
    assert_eq!(body["error"]["details"][0]["field"], "status");

    // A rename that names no parent leaves VIP where it is.
    let (status, _) = patch_area(
        &router,
        &cookie,
        &vip,
        &vip_etag,
        serde_json::json!({ "name": "VIP lounge", "status": "active" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/floor/areas/{vip}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("read VIP");
    let vip_etag = etag_of(&read);
    assert_eq!(
        json_body(read).await["parent_area_id"],
        serde_json::json!(floor)
    );

    // Moving VIP (with its room) to the ground floor is one write.
    let (status, _) = patch_area(
        &router,
        &cookie,
        &vip,
        &vip_etag,
        serde_json::json!({ "name": "VIP lounge", "status": "active", "parent_area_id": ground }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

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
    let areas = doc["floor"]["areas"].as_array().expect("the areas");
    let mut tops = [floor.clone(), ground.clone()];
    tops.sort();
    let mut expected = Vec::new();
    for top in &tops {
        expected.push((top.clone(), 0, Vec::<String>::new()));
        if *top == ground {
            expected.push((vip.clone(), 1, vec![ground.clone()]));
            expected.push((room.clone(), 2, vec![ground.clone(), vip.clone()]));
        }
    }
    let published: Vec<(String, u64, Vec<String>)> = areas
        .iter()
        .map(|area| {
            (
                area["area_id"].as_str().expect("id").to_owned(),
                area["depth"].as_u64().unwrap_or(0),
                area["path"]
                    .as_array()
                    .map(|path| {
                        path.iter()
                            .map(|id| id.as_str().expect("id").to_owned())
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(published, expected, "tree order, the top level by id");
    // A top-level area carries none of the tree fields, as before areas nested.
    let top = areas
        .iter()
        .find(|area| area["area_id"] == serde_json::json!(floor))
        .expect("the floor");
    assert!(top.get("parent_area_id").is_none() && top.get("path").is_none());
}
