// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The admitted-device roster (`/admin/stores/{store_id}/devices/admitted`, ADR-0118 §4).

use axum::http::StatusCode;
use pos_cloud::cloud::AdmittedDevice;
use pos_cloud::dashboard::{RollupStore, StoredRollups};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeKeys, FakeRollups, admin_cookie, app_with_admin, get, get_with_cookie, json_body,
    provisioned_admin, store_id, tenant,
};

// --- The admitted-device roster (`GET /admin/stores/{id}/devices/admitted`, ADR-0118 §4) --------

#[tokio::test]
async fn the_console_reads_which_devices_a_store_admitted() {
    // The store's own roster, folded from the two events the edge now publishes. Seeded directly
    // rather than through the projector, because what this test is about is the *read*: that the
    // console can finally answer "which tills does this store have", which before ADR-0118 was
    // always zero rows however many tablets the store had paired.
    let rollups = FakeRollups::default();
    let live = Ulid::from_u128(0x11).to_string();
    let retired = Ulid::from_u128(0x12).to_string();
    let minting_till = Ulid::from_u128(0x07).to_string();
    let seeded = StoredRollups {
        devices: [
            (
                live.clone(),
                AdmittedDevice {
                    local_device_id: live.clone(),
                    admitted_at_ms: 2_000,
                    admitted_by_device_id: Some(minting_till.clone()),
                    revoked_at_ms: None,
                },
            ),
            (
                retired.clone(),
                AdmittedDevice {
                    local_device_id: retired.clone(),
                    admitted_at_ms: 3_000,
                    admitted_by_device_id: None,
                    revoked_at_ms: Some(9_000),
                },
            ),
        ]
        .into_iter()
        .collect(),
        ..StoredRollups::default()
    };
    rollups
        .save(tenant(), store_id(), &seeded)
        .await
        .expect("seed the roster");

    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        rollups,
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let uri = format!("/admin/stores/{store_ulid}/devices/admitted?tenant_id={tenant_ulid}");

    // Closed without a session, like every other `/admin` read.
    let unauth = router
        .clone()
        .oneshot(get(&uri, None))
        .await
        .expect("route the read");
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let cookie = admin_cookie(&router).await;
    let read = router
        .clone()
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the read");
    assert_eq!(read.status(), StatusCode::OK);
    let body = json_body(read).await;
    let rows = body.as_array().expect("an array of devices");
    assert_eq!(
        rows.len(),
        2,
        "both the live till and the retired one: {body}"
    );
    // Still-admitted first, so an operator sees what the store is running.
    assert_eq!(rows[0]["local_device_id"], live);
    assert_eq!(rows[0]["admitted_by_device_id"], minting_till);
    assert!(rows[0]["revoked_at_ms"].is_null());
    assert_eq!(rows[1]["local_device_id"], retired);
    assert_eq!(rows[1]["revoked_at_ms"], 9_000);
    // The load-bearing absence: who admitted a device stays on the store's own disk and reaches no
    // cloud (ADR-0118 §5), so there is no employee anywhere in this response.
    let serialised = body.to_string();
    assert!(
        !serialised.contains("employee"),
        "the roster must carry no employee identity: {serialised}"
    );
}

#[tokio::test]
async fn a_store_that_has_admitted_nothing_reads_as_an_empty_roster() {
    // Not a `404`. A store with no admissions and a store whose rollup has never been projected are
    // both "nothing admitted yet", and the console renders an empty list either way — the same
    // shape the other rollup reads take for a store with no events.
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let cookie = admin_cookie(&router).await;
    let uri = format!(
        "/admin/stores/{}/devices/admitted?tenant_id={}",
        store_id().as_ulid(),
        tenant().as_ulid()
    );
    let read = router
        .oneshot(get_with_cookie(&uri, &cookie))
        .await
        .expect("route the read");
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(json_body(read).await, serde_json::json!([]));
}

#[tokio::test]
async fn the_roster_read_refuses_an_identifier_that_is_not_a_ulid() {
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let cookie = admin_cookie(&router).await;
    for uri in [
        format!(
            "/admin/stores/not-a-ulid/devices/admitted?tenant_id={}",
            tenant().as_ulid()
        ),
        format!(
            "/admin/stores/{}/devices/admitted?tenant_id=not-a-ulid",
            store_id().as_ulid()
        ),
    ] {
        let refused = router
            .clone()
            .oneshot(get_with_cookie(&uri, &cookie))
            .await
            .expect("route the read");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "a malformed identifier is a request fault, not a missing store: {uri}"
        );
    }
}
