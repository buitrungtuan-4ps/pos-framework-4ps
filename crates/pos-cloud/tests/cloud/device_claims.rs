// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Claiming a box installed with no store (`/claim`, `/admin/claims/bind`, ADR-0148).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pos_cloud::activation::DeviceCredential;
use pos_cloud::audit::NoopAuditRecorder;
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::claim::{BindOutcome, ClaimStore, ClaimStoreError, CollectOutcome};
use pos_cloud::{Cloud, http};
use pos_core::activation::ActivationCode;
use pos_fakes::FakeStore;
use pos_proto::ids::{DeviceId, StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeActivations, FakeAdmin, FakeKeys, FakeRollups, NOW_MS, app, clock, json_body, post_json,
    post_with_cookie, provisioned_admin, role_session_cookie, store_id, tenant,
};

// --- Claiming a box installed with no store (ADR-0148) ------------------------------------------

/// One claim as the table keeps it: hashes, expiry, the slot once bound, and whether collected.
#[derive(Clone)]
struct FakeClaim {
    code_hash: [u8; 32],
    secret_hash: [u8; 32],
    expires_at: Timestamp,
    slot: Option<(TenantId, StoreId, DeviceId)>,
    collected: bool,
}

/// The claim store, keyed by claim id, with the table's once-only rules.
#[derive(Clone, Default)]
struct FakeClaims {
    rows: Arc<Mutex<HashMap<Ulid, FakeClaim>>>,
}

impl ClaimStore for FakeClaims {
    async fn open(
        &self,
        claim_id: Ulid,
        user_code_hash: [u8; 32],
        secret_hash: [u8; 32],
        expires_at: Timestamp,
    ) -> Result<(), ClaimStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        if rows.values().any(|row| row.code_hash == user_code_hash) {
            return Err(ClaimStoreError::new("the code is taken"));
        }
        rows.insert(
            claim_id,
            FakeClaim {
                code_hash: user_code_hash,
                secret_hash,
                expires_at,
                slot: None,
                collected: false,
            },
        );
        Ok(())
    }

    async fn bind(
        &self,
        user_code_hash: [u8; 32],
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
        now: Timestamp,
    ) -> Result<BindOutcome, ClaimStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows
            .values_mut()
            .find(|row| row.code_hash == user_code_hash)
        else {
            return Ok(BindOutcome::Unknown);
        };
        Ok(if row.slot.is_some() {
            BindOutcome::AlreadyBound
        } else if row.expires_at <= now {
            BindOutcome::Expired
        } else {
            row.slot = Some((tenant_id, store_id, device_id));
            BindOutcome::Bound
        })
    }

    async fn collect(
        &self,
        claim_id: Ulid,
        secret_hash: [u8; 32],
        _credential: &DeviceCredential,
        now: Timestamp,
    ) -> Result<CollectOutcome, ClaimStoreError> {
        let mut rows = self.rows.lock().expect("lock");
        let Some(row) = rows
            .get_mut(&claim_id)
            .filter(|row| row.secret_hash == secret_hash)
        else {
            return Ok(CollectOutcome::Refused);
        };
        Ok(match row.slot {
            _ if row.collected => CollectOutcome::Refused,
            _ if row.expires_at <= now => CollectOutcome::Expired,
            None => CollectOutcome::Pending,
            Some((tenant_id, store_id, device_id)) => {
                row.collected = true;
                CollectOutcome::Collected {
                    tenant_id,
                    store_id,
                    device_id,
                }
            }
        })
    }
}

/// What `POST /claim` handed the box: its claim id, the code it shows, and its secret.
async fn open_a_claim(router: &axum::Router) -> (String, String, String) {
    let opened = router
        .clone()
        .oneshot(post_json("/claim", &serde_json::json!({})))
        .await
        .expect("route the open");
    assert_eq!(opened.status(), StatusCode::CREATED);
    let body = json_body(opened).await;
    assert_eq!(body["expires_in_secs"], 3600);
    assert_eq!(body["poll_interval_secs"], 5);
    let field = |name: &str| body[name].as_str().expect("a string").to_owned();
    (field("claim_id"), field("user_code"), field("secret"))
}

fn bind_body(user_code: &str, device: DeviceId) -> serde_json::Value {
    serde_json::json!({
        "user_code": user_code,
        "tenant_id": tenant().to_string(),
        "store_id": store_id().to_string(),
        "device_id": device.to_string(),
    })
}

/// A box shows its code, a console user binds it, and the box collects its credential exactly once
/// (ADR-0148). Nobody else collects it: a wrong secret and a second collection are one refusal.
#[tokio::test]
async fn a_box_is_claimed_by_its_code_and_collects_its_credential_once() {
    let admin = provisioned_admin();
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let viewer = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let router = http::claim_router(
        FakeClaims::default(),
        admin,
        clock(),
        Arc::new(NoopAuditRecorder),
    );
    let (claim_id, user_code, secret) = open_a_claim(&router).await;
    assert_eq!(user_code.len(), 9, "shown as XXXX-XXXX");
    assert_eq!(secret.len(), 64, "256 bits of hex");
    let device = DeviceId::new(Ulid::from_u128(0x00C1_A1ED));
    let collect = |secret: String| {
        let router = router.clone();
        let uri = format!("/claim/{claim_id}/collect");
        async move {
            router
                .oneshot(post_json(&uri, &serde_json::json!({ "secret": secret })))
                .await
                .expect("route the collection")
        }
    };

    assert_eq!(
        collect(secret.clone()).await.status(),
        StatusCode::ACCEPTED,
        "nobody has bound it yet"
    );
    assert_eq!(
        collect("0".repeat(64)).await.status(),
        StatusCode::FORBIDDEN
    );

    let bind = |body: serde_json::Value, cookie: Option<String>| {
        let router = router.clone();
        async move {
            let request = match cookie {
                Some(cookie) => post_with_cookie("/admin/claims/bind", &body, &cookie),
                None => post_json("/admin/claims/bind", &body),
            };
            router.oneshot(request).await.expect("route the bind")
        }
    };
    assert_eq!(
        bind(bind_body(&user_code, device), None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        bind(bind_body(&user_code, device), Some(viewer))
            .await
            .status(),
        StatusCode::FORBIDDEN,
        "binding needs console.devices.manage"
    );
    assert_eq!(
        bind(bind_body("NOT-A-CODE!", device), Some(ops.clone()))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        bind(bind_body("ZZZZ-ZZZZ", device), Some(ops.clone()))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    // Typed the way a person types it: lower case, no dash.
    let typed = user_code.replace('-', "").to_lowercase();
    assert_eq!(
        bind(bind_body(&typed, device), Some(ops.clone()))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let again = bind(bind_body(&user_code, device), Some(ops)).await;
    assert_eq!(again.status(), StatusCode::CONFLICT, "a code binds once");

    let collected = collect(secret.clone()).await;
    assert_eq!(collected.status(), StatusCode::OK);
    let body = json_body(collected).await;
    assert_eq!(body["tenant_id"], tenant().to_string());
    assert_eq!(body["store_id"], store_id().to_string());
    assert_eq!(body["device_id"], device.to_string());
    assert!(
        body["credential"]
            .as_str()
            .expect("a credential")
            .starts_with("posdev_")
    );

    assert_eq!(
        collect(secret).await.status(),
        StatusCode::FORBIDDEN,
        "collected once"
    );
}

/// After its hour a claim can be neither bound nor collected, and each says so.
#[tokio::test]
async fn an_expired_claim_can_neither_be_bound_nor_collected() {
    let admin = provisioned_admin();
    let ops = role_session_cookie(&admin, AdminRole::Ops, "ops-token").await;
    let clock = clock();
    let router = http::claim_router(
        FakeClaims::default(),
        admin,
        clock.clone(),
        Arc::new(NoopAuditRecorder),
    );
    // Opened a second before the admin's session began, so the claim's hour ends while the session
    // (seeded for the hour from `NOW_MS`) is still live.
    clock.set(Timestamp::from_milliseconds_since_epoch(NOW_MS - 1_000).expect("valid"));
    let (claim_id, user_code, secret) = open_a_claim(&router).await;
    clock.set(Timestamp::from_milliseconds_since_epoch(NOW_MS + 3_599_000).expect("valid"));

    let bound = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/claims/bind",
            &bind_body(&user_code, DeviceId::new(Ulid::from_u128(7))),
            &ops,
        ))
        .await
        .expect("route the bind");
    assert_eq!(bound.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(bound).await["error"]["details"][0]["reason"],
        "EXPIRED"
    );

    let collected = router
        .oneshot(post_json(
            &format!("/claim/{claim_id}/collect"),
            &serde_json::json!({ "secret": secret }),
        ))
        .await
        .expect("route the collection");
    assert_eq!(collected.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(collected).await["error"]["details"][0]["reason"],
        "EXPIRED"
    );
}

/// The unauthenticated code exchange has a budget per client (`docs/pos-spec.md` §16). It had
/// none, so nothing but the size of the code space stood between a script and a guessing run.
#[tokio::test]
async fn the_code_exchange_is_refused_once_a_client_is_over_its_budget() {
    let state = app(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
    );
    let throttle = state.sync_throttle();
    let router =
        http::activation_router(FakeActivations::default(), FakeAdmin::default(), clock()).layer(
            axum::middleware::from_fn_with_state(throttle, http::throttle_sync),
        );
    let guess = |n: u8| {
        let router = router.clone();
        async move {
            let code = ActivationCode::from_entropy([n; pos_core::activation::PAYLOAD_LEN]);
            router
                .oneshot(post_json(
                    "/activate",
                    &serde_json::json!({ "code": code.as_str() }),
                ))
                .await
                .expect("route the guess")
                .status()
        }
    };
    for n in 0..10 {
        assert_eq!(
            guess(n).await,
            StatusCode::FORBIDDEN,
            "guess {n} is looked up"
        );
    }
    assert_eq!(
        guess(10).await,
        StatusCode::TOO_MANY_REQUESTS,
        "the eleventh is not"
    );
}
