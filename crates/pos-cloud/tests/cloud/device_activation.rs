// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The activation-code exchange (`/activate`, ADR-0050), and the cell prefix a named cell issues and
//! answers for (ADR-0181).

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::activation::hash_code;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::http;
use pos_core::activation::ActivationCode;
use pos_proto::ids::{DeviceId, StoreId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeActivations, FakeAdmin, body_bytes, clock, json_body, post_json, post_with_cookie,
    role_session_cookie, store_id, tenant,
};

#[tokio::test]
async fn the_activation_exchange_is_single_use_and_gives_no_oracle() {
    let code = ActivationCode::from_entropy([13; pos_core::activation::PAYLOAD_LEN]);
    let device = DeviceId::new(Ulid::from_u128(0xDECAF));
    let activations = FakeActivations::with_issued(hash_code(&code), tenant(), store_id(), device);
    let router = http::activation_router(activations.clone(), FakeAdmin::default(), clock());

    // A device presents its valid, unredeemed code and receives a minted credential, shown once.
    let first = router
        .clone()
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": code.as_str() }),
        ))
        .await
        .expect("route the exchange");
    assert_eq!(first.status(), StatusCode::CREATED);
    let body = json_body(first).await;
    assert!(
        body["credential"]
            .as_str()
            .expect("a credential")
            .starts_with("posdev_"),
        "the credential is the real value, shown once"
    );
    assert_eq!(
        body["device_id"].as_str().expect("a device id"),
        device.to_string()
    );
    assert_eq!(activations.minted(), 1);

    // The same code again is refused — activation is single-use.
    let replay = router
        .clone()
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": code.as_str() }),
        ))
        .await
        .expect("route the replay");
    assert_eq!(
        replay.status(),
        StatusCode::FORBIDDEN,
        "a spent code is refused"
    );
    assert_eq!(activations.minted(), 1, "no second credential is minted");

    // An unknown but well-formed code is refused identically — no oracle tells them apart.
    let unknown = ActivationCode::from_entropy([200; pos_core::activation::PAYLOAD_LEN]);
    let miss = router
        .clone()
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": unknown.as_str() }),
        ))
        .await
        .expect("route the unknown code");
    assert_eq!(
        miss.status(),
        StatusCode::FORBIDDEN,
        "an unknown code is refused exactly as a spent one"
    );

    // A malformed code is a plain client error, not a refusal — it never named a real code.
    let malformed = router
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": "not-a-valid-code" }),
        ))
        .await
        .expect("route the malformed code");
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
}

/// A code issued for another store is refused **unspent**, with the generic refusal.
///
/// A technician who types the wrong shop's code into a box got that shop's device, and nothing said
/// so until `/sync` refused every call, by which time the code was spent. The edge now names the
/// store it was installed for; the cloud refuses a mismatch before consuming anything.
#[tokio::test]
async fn a_code_for_another_store_is_refused_and_left_unspent() {
    let code = ActivationCode::from_entropy([17; pos_core::activation::PAYLOAD_LEN]);
    let device = DeviceId::new(Ulid::from_u128(0xBEEF));
    let activations = FakeActivations::with_issued(hash_code(&code), tenant(), store_id(), device);
    let router = http::activation_router(activations.clone(), FakeAdmin::default(), clock());
    let exchange = |store: String| {
        let router = router.clone();
        let code = code.as_str().to_owned();
        async move {
            router
                .oneshot(post_json(
                    "/activate",
                    &serde_json::json!({ "code": code, "store_id": store }),
                ))
                .await
                .expect("route the exchange")
        }
    };

    let elsewhere = StoreId::new(Ulid::from_u128(0x0E15_E1E4)).to_string();
    let refused = exchange(elsewhere).await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes(refused).await).expect("the envelope");
    assert_eq!(
        body["error"]["message"], "activation refused",
        "the generic refusal"
    );
    assert_eq!(activations.minted(), 0, "nothing was minted");

    let granted = exchange(store_id().to_string()).await;
    assert_eq!(
        granted.status(),
        StatusCode::CREATED,
        "the code was not spent"
    );
    assert_eq!(activations.minted(), 1);

    let malformed = exchange("not-a-ulid".to_owned()).await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
}

/// A named cell, `vn1`, whose directory lists `jp1` and no other cell.
struct Vn1;

impl pos_cloud_devices::routes::ActivationCell for Vn1 {
    fn issuing_cell_id(&self) -> Option<String> {
        Some("vn1".to_owned())
    }

    fn is_this_cell(&self, cell_id: &str) -> bool {
        cell_id == "vn1"
    }

    fn cell_base_url(&self, cell_id: &str) -> Option<String> {
        (cell_id == "jp1").then(|| "https://jp1.example.com".to_owned())
    }
}

/// A named cell issues its codes with its id as their prefix, and the code redeems by its twelve
/// symbols whether the box presents the prefix or not.
#[tokio::test]
async fn a_named_cell_issues_its_codes_with_its_prefix() {
    let admin = FakeAdmin::default();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let activations = FakeActivations::default();
    let router = pos_cloud_devices::routes::activation_router_in_cell(
        activations.clone(),
        admin,
        clock(),
        Arc::new(Vn1),
    );
    let slot = serde_json::json!({
        "tenant_id": tenant().to_string(),
        "store_id": store_id().to_string(),
        "device_id": DeviceId::new(Ulid::from_u128(0xF00D)).to_string(),
    });
    let issue = || {
        let router = router.clone();
        let request = post_with_cookie("/admin/activation-codes", &slot, &cookie);
        async move { router.oneshot(request).await.expect("route the issue") }
    };

    let issued = issue().await;
    assert_eq!(issued.status(), StatusCode::CREATED);
    let printed = json_body(issued).await["activation_code"]
        .as_str()
        .expect("a code")
        .to_owned();
    assert!(
        printed.starts_with("VN1-"),
        "the prefix is printed: {printed}"
    );
    let code = ActivationCode::parse(&printed).expect("the printed code parses");
    assert_eq!(code.cell_id(), Some("vn1"));

    let granted = router
        .clone()
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": printed }),
        ))
        .await
        .expect("route the exchange");
    assert_eq!(
        granted.status(),
        StatusCode::CREATED,
        "the prefixed code redeems"
    );
    assert_eq!(activations.minted(), 1);

    // The same code without its prefix is the same code, and it is spent.
    let replayed = router
        .clone()
        .oneshot(post_json(
            "/activate",
            &serde_json::json!({ "code": code.as_str() }),
        ))
        .await
        .expect("route the replay");
    assert_eq!(replayed.status(), StatusCode::FORBIDDEN);

    // A single cell issues codes with no prefix, as before.
    let admin = FakeAdmin::default();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let single = http::activation_router(FakeActivations::default(), admin, clock());
    let plain = single
        .oneshot(post_with_cookie("/admin/activation-codes", &slot, &cookie))
        .await
        .expect("route the issue");
    assert_eq!(plain.status(), StatusCode::CREATED);
    let plain = json_body(plain).await["activation_code"]
        .as_str()
        .expect("a code")
        .to_owned();
    assert_eq!(
        ActivationCode::parse(&plain).expect("a code").cell_id(),
        None,
        "{plain}"
    );
}

/// A refused code whose prefix names another cell gets the same `403`, plus where the directory puts
/// that cell. A code issued here is still redeemed whatever prefix it is typed with: the prefix is a
/// hint, never the credential.
#[tokio::test]
async fn a_prefix_naming_another_cell_is_answered_from_the_directory() {
    let code = ActivationCode::from_entropy([23; pos_core::activation::PAYLOAD_LEN]);
    let unknown = ActivationCode::from_entropy([201; pos_core::activation::PAYLOAD_LEN]);
    let device = DeviceId::new(Ulid::from_u128(0xCE11));
    let activations = FakeActivations::with_issued(hash_code(&code), tenant(), store_id(), device);
    let router = pos_cloud_devices::routes::activation_router_in_cell(
        activations.clone(),
        FakeAdmin::default(),
        clock(),
        Arc::new(Vn1),
    );
    let exchange = |typed: String| {
        let router = router.clone();
        async move {
            let response = router
                .oneshot(post_json(
                    "/activate",
                    &serde_json::json!({ "code": typed }),
                ))
                .await
                .expect("route the exchange");
            let status = response.status();
            let body: serde_json::Value =
                serde_json::from_slice(&body_bytes(response).await).expect("json");
            (status, body)
        }
    };

    let (status, body) = exchange(format!("JP1-{}", unknown.as_str())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["details"][0]["field"], "code");
    assert_eq!(body["error"]["details"][0]["reason"], "OTHER_CELL");
    let message = body["error"]["message"].as_str().expect("a message");
    assert!(
        message.contains("jp1") && message.contains("https://jp1.example.com"),
        "{message}"
    );

    let (status, body) = exchange(format!("kr9-{}", unknown.as_str())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["details"][0]["reason"], "UNKNOWN_CELL");

    // This cell's own prefix, or none, on an unknown code: the bare refusal, unchanged.
    for typed in [
        format!("vn1-{}", unknown.as_str()),
        unknown.as_str().to_owned(),
    ] {
        let (status, body) = exchange(typed).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["message"], "activation refused");
        assert!(body["error"].get("details").is_none(), "{body}");
    }

    // A code issued here, typed with another cell's prefix, is still this cell's code.
    let (status, _) = exchange(format!("jp1-{}", code.as_str())).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(activations.minted(), 1);
}
