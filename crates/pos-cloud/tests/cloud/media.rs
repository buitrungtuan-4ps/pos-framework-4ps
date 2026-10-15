// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The media routes (`/admin/media`, ADR-0075): upload, serve, list and delete.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::audit::{AuditSink, NoopAuditRecorder};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeAudit, FakeConfigTrees, FakeKeys, FakeMedia, FakeRollups, FakeWebhooks,
    admin_cookie, app_all, clock, delete_with_cookie, get_with_cookie, json_body,
    post_bytes_with_cookie, provisioned_admin, role_session_cookie, ulid_text,
};

fn media_app(admin: FakeAdmin, media: FakeMedia) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::media_router(
        media,
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    ))
}

/// A small valid PNG for the upload path — a 4×4 solid image the ADR-0042 pipeline decodes and
/// re-encodes to two JPEG renditions.
fn tiny_png() -> Vec<u8> {
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        4,
        4,
        image::Rgb([200, 40, 40]),
    ));
    let mut buffer = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buffer, image::ImageFormat::Png)
        .expect("encode a test png");
    buffer.into_inner()
}

#[tokio::test]
async fn media_uploads_serves_lists_and_deletes_and_rejects_a_non_image() {
    let router = media_app(provisioned_admin(), FakeMedia::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    // A non-image body is refused before anything is stored.
    let bad = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/media?tenant_id={tenant}"),
            b"this is not an image".to_vec(),
            "application/octet-stream",
            &cookie,
        ))
        .await
        .expect("route the bad upload");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // A valid image is re-encoded and stored; the reply carries the new id.
    let created = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/media?tenant_id={tenant}"),
            tiny_png(),
            "image/png",
            &cookie,
        ))
        .await
        .expect("route the upload");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let media_id = created["media_id"].as_str().expect("a media id").to_owned();
    assert!(created["detail_bytes"].as_u64().expect("size") > 0);

    // The thumbnail serves as JPEG.
    let thumb = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media/{media_id}/thumbnail?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the thumbnail");
    assert_eq!(thumb.status(), StatusCode::OK);
    assert_eq!(
        thumb
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("image/jpeg"),
    );

    // The listing shows the one asset.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the list");
    assert_eq!(listed.status(), StatusCode::OK);
    let items = json_body(listed).await;
    assert_eq!(items.as_array().expect("array").len(), 1);
    assert_eq!(items[0]["media_id"], media_id);

    // Delete removes it; a second delete is a 404.
    let deleted = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/media/{media_id}?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the delete");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let gone = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media/{media_id}/thumbnail?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the gone thumbnail");
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_media_list_returns_a_bare_array_unpaged_and_an_envelope_when_a_limit_is_named() {
    // The two shapes of the same route (ADR-0098). The image picker sends no `limit` and must keep
    // getting a plain array of the whole library; the Media screen's table sends one and gets the
    // window plus the library's size. A route that answered one shape for both requests would
    // either break the picker's `.map` or leave the table with no count to page by.
    let router = media_app(provisioned_admin(), FakeMedia::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    for _upload in 0..3 {
        let created = router
            .clone()
            .oneshot(post_bytes_with_cookie(
                &format!("/admin/media?tenant_id={tenant}"),
                tiny_png(),
                "image/png",
                &cookie,
            ))
            .await
            .expect("route the upload");
        assert_eq!(created.status(), StatusCode::CREATED);
    }

    let unpaged = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the unpaged list");
    assert_eq!(unpaged.status(), StatusCode::OK);
    let unpaged = json_body(unpaged).await;
    assert_eq!(
        unpaged
            .as_array()
            .expect("a bare array, not an envelope")
            .len(),
        3,
    );

    let paged = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media?tenant_id={tenant}&limit=2"),
            &cookie,
        ))
        .await
        .expect("route the paged list");
    assert_eq!(paged.status(), StatusCode::OK);
    let paged = json_body(paged).await;
    assert_eq!(paged["items"].as_array().expect("the window").len(), 2);
    assert_eq!(paged["total"], 3, "the total is the library, not the page");
    assert_eq!(paged["limit"], 2, "the bounds used are echoed back");
    assert_eq!(paged["offset"], 0);

    // A second page carries the remaining asset and the same total.
    let tail = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/media?tenant_id={tenant}&limit=2&offset=2"),
            &cookie,
        ))
        .await
        .expect("route the second page");
    let tail = json_body(tail).await;
    assert_eq!(tail["items"].as_array().expect("the window").len(), 1);
    assert_eq!(tail["total"], 3);
    assert_eq!(tail["offset"], 2);
}

#[tokio::test]
async fn the_media_list_refuses_a_page_bound_it_cannot_serve() {
    // The refusals `parse_page` builds, reached through a real route so the wiring is covered too:
    // a limit past the cap, a limit that is not a number, and an offset with no limit to skip into.
    let router = media_app(provisioned_admin(), FakeMedia::default());
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    for (query, field) in [
        ("limit=100000", "limit"),
        ("limit=0", "limit"),
        ("limit=lots", "limit"),
        ("offset=25", "offset"),
    ] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(
                &format!("/admin/media?tenant_id={tenant}&{query}"),
                &cookie,
            ))
            .await
            .expect("route the bad bound");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "`{query}` is a client mistake, not a page",
        );
        let body = json_body(refused).await;
        assert_eq!(
            body["error"]["details"][0]["field"], field,
            "`{query}` names the parameter that was wrong",
        );
    }
}

#[tokio::test]
async fn media_routes_require_a_session() {
    let router = media_app(provisioned_admin(), FakeMedia::default());
    let tenant = ulid_text(1);
    let anonymous = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/admin/media?tenant_id={tenant}"))
                .body(Body::empty())
                .expect("build"),
        )
        .await
        .expect("route without a cookie");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

// --- pinned before `admin_upload_media` is split (ADR-0170 decision 7) ---------------------------

/// An upload stores the asset, answers its id and size, and records one audit entry naming them.
#[tokio::test]
async fn an_upload_is_stored_answered_and_audited_by_its_id_and_size() {
    let audit = FakeAudit::default();
    let media = FakeMedia::default();
    let admin = provisioned_admin();
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    let router = http::router(app).merge(http::media_router(
        media,
        admin,
        clock(),
        Arc::new(AuditSink::new(audit.clone())),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);

    let created = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/media?tenant_id={tenant}"),
            tiny_png(),
            "image/png",
            &cookie,
        ))
        .await
        .expect("route the upload");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let media_id = created["media_id"].as_str().expect("a media id").to_owned();
    let detail_bytes = created["detail_bytes"].as_u64().expect("a size");

    let entries = audit.entries.lock().expect("lock").clone();
    let [entry] = entries.as_slice() else {
        panic!("one audit entry: {entries:?}");
    };
    assert_eq!(entry.action, "media.upload");
    assert_eq!(entry.entity_type, "media_asset");
    assert_eq!(entry.entity_id, media_id);
    assert_eq!(
        entry
            .tenant_id
            .map(|tenant_id| tenant_id.as_ulid().to_string()),
        Some(tenant.clone())
    );
    assert_eq!(
        entry.after,
        Some(serde_json::json!({ "content_type": "image/jpeg", "detail_bytes": detail_bytes }))
    );

    // What was stored serves at the id the answer named.
    let detail = router
        .oneshot(get_with_cookie(
            &format!("/admin/media/{media_id}/detail?tenant_id={tenant}"),
            &cookie,
        ))
        .await
        .expect("route the detail");
    assert_eq!(detail.status(), StatusCode::OK);
}

/// An upload refuses a role that may not manage media and an owner that is not a ULID, and a
/// refused upload records nothing.
#[tokio::test]
async fn an_upload_refuses_a_role_without_media_and_a_bad_tenant() {
    let audit = FakeAudit::default();
    let admin = provisioned_admin();
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    let router = http::router(app).merge(http::media_router(
        FakeMedia::default(),
        admin.clone(),
        clock(),
        Arc::new(AuditSink::new(audit.clone())),
    ));

    // The owner logs in first: the login names no email, so it is the only admin when it does.
    let cookie = admin_cookie(&router).await;
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let by_ops = router
        .clone()
        .oneshot(post_bytes_with_cookie(
            &format!("/admin/media?tenant_id={}", ulid_text(1)),
            tiny_png(),
            "image/png",
            &ops,
        ))
        .await
        .expect("route the upload");
    assert_eq!(by_ops.status(), StatusCode::FORBIDDEN);

    let bad = router
        .oneshot(post_bytes_with_cookie(
            "/admin/media?tenant_id=not-a-ulid",
            tiny_png(),
            "image/png",
            &cookie,
        ))
        .await
        .expect("route the upload");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(bad).await["error"]["details"][0]["field"],
        "tenant_id"
    );
    assert!(audit.entries.lock().expect("lock").is_empty());
}
