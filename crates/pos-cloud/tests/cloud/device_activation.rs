// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The activation-code exchange (`/activate`, ADR-0050).

use axum::http::StatusCode;
use pos_cloud::activation::hash_code;
use pos_cloud::http;
use pos_core::activation::ActivationCode;
use pos_proto::ids::{DeviceId, StoreId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeActivations, FakeAdmin, body_bytes, clock, json_body, post_json, store_id, tenant,
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
