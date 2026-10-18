// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store-facing configuration surface (`/sync`): config sync, the heartbeat and published
//! events, each confined to the credential's own store and scope.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::auth::apikey::Scope;
use pos_cloud::{Cloud, http};
use pos_contract_tests::fixtures;
use pos_fakes::FakeStore;
use pos_ports::EventStore;
use pos_ports::event_store::EventQuery;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, FakeDevices, FakeKeys, FakeRollups, FakeWebhooks, NOW_MS, admin_cookie,
    app_all, device_app, get, issue_device_credential, issue_key, issue_store_key, json_body,
    post_json, post_json_bearer, provisioned_admin, put_with_etag, store_id, tenant,
};

// --- Store-facing config sync (`GET /sync/stores/{id}/config`, bearer + read_config) ------------

/// Publishes one config version through the admin route, then returns the router, the `read_config`
/// bearer, the store ULID, and the published version id — the fixture the sync tests share.
async fn published_config(keys: &FakeKeys) -> (axum::Router, String, String, String) {
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        provisioned_admin(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let doc = serde_json::json!({ "currency_code": "VND", "tips_enabled": false });
    let published = router
        .clone()
        .oneshot(put_with_etag(
            &format!("/admin/stores/{store_ulid}/config/store?tenant_id={tenant_ulid}"),
            &doc,
            &cookie,
            "*",
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let version = json_body(published).await["config_version_id"]
        .as_str()
        .expect("a version id")
        .to_owned();
    let token = issue_store_key(keys, tenant(), store_id(), &[Scope::ReadConfig]);
    (router, token, store_ulid, version)
}

/// A sibling store's key does not read this store's configuration (production-readiness **S1**).
///
/// The `/sync` routes took the tenant from the verified grant and the store from the *path*, which
/// within one tenant is no check at all: every store in a chain shares a tenant, so any store's key
/// served any sibling's `permissions` node — employee names and PIN hashes. Both halves are asserted
/// here, because the second is what the first one's fix is worth: a key bound to store B is refused,
/// and the key bound to store A still works on store A.
#[tokio::test]
async fn one_stores_key_does_not_read_another_stores_config() {
    let keys = FakeKeys::default();
    let (router, own_token, store_ulid, _version) = published_config(&keys).await;
    let sibling = StoreId::new(Ulid::from_u128(0x0051_5111)); // a second store in the *same* tenant
    let sibling_token = issue_store_key(&keys, tenant(), sibling, &[Scope::ReadConfig]);

    let stolen = router
        .clone()
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config"),
            Some(&sibling_token),
        ))
        .await
        .expect("route");
    assert_eq!(
        stolen.status(),
        StatusCode::FORBIDDEN,
        "a sibling store's key must not read this store's config"
    );

    let own = router
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config"),
            Some(&own_token),
        ))
        .await
        .expect("route");
    assert_eq!(
        own.status(),
        StatusCode::OK,
        "the store's own key still reads its own config"
    );
}

/// A tenant-wide key is not a store's credential, however good its scopes (**S1**).
///
/// `read_config` on an unbound key was the shape every key had before per-store scoping, and
/// treating "names no store" as "may act for any store in the tenant" would leave the finding open
/// under a different name. So `/sync` refuses it and the operator re-issues a store-scoped key.
#[tokio::test]
async fn a_tenant_wide_key_is_refused_on_the_store_facing_surface() {
    let keys = FakeKeys::default();
    let (router, _own_token, store_ulid, _version) = published_config(&keys).await;
    let tenant_wide = issue_key(&keys, tenant(), &[Scope::ReadConfig]);

    let response = router
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config"),
            Some(&tenant_wide),
        ))
        .await
        .expect("route");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// A refusal on the **store-facing** surface is enveloped too, and names the field that was wrong
/// rather than the other one on the same route.
///
/// `GET /sync/stores/{id}/config` can refuse for two different fields — the path's store id or the
/// query's `held_version` — and a store that sent a good id and a bad version needs to be told
/// which. Before this slice both answered the same shape of bare string.
#[tokio::test]
async fn a_store_facing_refusal_names_the_field_that_was_actually_wrong() {
    let keys = FakeKeys::default();
    let (router, token, store_ulid, _version) = published_config(&keys).await;

    let response = router
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config?held_version=not-a-ulid"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT", "got {body}");
    assert_eq!(body["error"]["details"][0]["field"], "held_version");
    assert_eq!(body["error"]["details"][0]["reason"], "NOT_A_ULID");
    assert!(
        body["error"]["details"][1].is_null(),
        "only the field that was wrong is named — the store id in the path was fine: {body}"
    );
}

/// A refusal about two fields names **only the ones actually missing.**
///
/// `propose_device` refuses when either `name` or `address` is blank, and its message says "name
/// and address are required" for both cases. A store that sent a name and forgot the address was
/// being told to check both. The condition is unchanged; the answer got specific.
#[tokio::test]
async fn a_refusal_about_two_fields_names_only_the_ones_missing() {
    let keys = FakeKeys::default();
    let router = device_app(provisioned_admin(), keys.clone(), FakeDevices::default());
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let devices_uri = format!("/sync/stores/{store_ulid}/devices");

    // A name, and a blank address.
    let response = router
        .clone()
        .oneshot(post_json_bearer(
            &devices_uri,
            &serde_json::json!({ "kind": "printer", "name": "Kitchen 1", "address": "   " }),
            &token,
        ))
        .await
        .expect("route the proposal");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(
        body["error"]["details"][0]["field"], "address",
        "got {body}"
    );
    assert_eq!(body["error"]["details"][0]["reason"], "REQUIRED");
    assert!(
        body["error"]["details"][1].is_null(),
        "the name was fine, so it is not named: {body}"
    );

    // Both blank: both named, in the order the fields are declared.
    let response = router
        .oneshot(post_json_bearer(
            &devices_uri,
            &serde_json::json!({ "kind": "printer", "name": "", "address": "" }),
            &token,
        ))
        .await
        .expect("route the proposal");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["details"][0]["field"], "name", "got {body}");
    assert_eq!(body["error"]["details"][1]["field"], "address");
}

#[tokio::test]
async fn config_sync_serves_an_update_then_reports_up_to_date() {
    let keys = FakeKeys::default();
    let (router, token, store_ulid, version) = published_config(&keys).await;

    // A store holding nothing gets an update to apply (a full snapshot for a first sync).
    let fresh = router
        .clone()
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(fresh.status(), StatusCode::OK);
    let body = json_body(fresh).await;
    assert_eq!(
        body["status"], "update",
        "a store with nothing gets an update"
    );
    assert!(
        !body["update"].is_null(),
        "the update carries a snapshot/delta"
    );

    // A store already holding the current version is told it is up to date.
    let current = router
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config?held_version={version}"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(current.status(), StatusCode::OK);
    assert_eq!(
        json_body(current).await["status"],
        "up_to_date",
        "holding the current version, the store applies nothing"
    );
}

#[tokio::test]
async fn config_sync_records_store_liveness() {
    // The config pull is the fleet-liveness signal (ADR-0068): each pull records the store's contact
    // and the version it reported holding, without altering the sync response.
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
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let doc = serde_json::json!({ "currency_code": "VND", "tips_enabled": false });
    let published = router
        .clone()
        .oneshot(put_with_etag(
            &format!("/admin/stores/{store_ulid}/config/store?tenant_id={tenant_ulid}"),
            &doc,
            &cookie,
            "*",
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let version = json_body(published).await["config_version_id"]
        .as_str()
        .expect("a version id")
        .to_owned();
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);

    // Nothing is recorded until the store actually pulls.
    assert!(
        config_trees.recorded_seen(tenant(), store_id()).is_none(),
        "no liveness before any pull"
    );

    // A pull holding nothing records the contact at the clock's instant, with a null held version.
    let fresh = router
        .clone()
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(fresh.status(), StatusCode::OK);
    let (held, seen_at) = config_trees
        .recorded_seen(tenant(), store_id())
        .expect("the pull recorded liveness");
    assert_eq!(
        held, None,
        "a store holding nothing records a null held version"
    );
    assert_eq!(
        seen_at, NOW_MS,
        "the contact instant is the server clock's now"
    );

    // A pull holding the current version records exactly that version.
    let current = router
        .oneshot(get(
            &format!("/sync/stores/{store_ulid}/config?held_version={version}"),
            Some(&token),
        ))
        .await
        .expect("route the sync");
    assert_eq!(current.status(), StatusCode::OK);
    let (held, _) = config_trees
        .recorded_seen(tenant(), store_id())
        .expect("liveness recorded");
    assert_eq!(
        held.map(|version| version.to_string()),
        Some(version),
        "the held version the edge reported is recorded verbatim"
    );
}

/// A box publishes its own events with nothing but its device credential (ADR-0143): the hello is
/// negotiated on the cloud's side, the whole batch lands, and a replay lands nothing new.
#[tokio::test]
async fn a_box_publishes_its_events_with_its_device_credential() {
    let keys = FakeKeys::default();
    let store = FakeStore::new();
    let router = http::router(app_all(
        Cloud::new(store.clone()),
        FakeRollups::default(),
        keys.clone(),
        provisioned_admin(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ));
    let token = issue_device_credential(&keys, tenant(), store_id());
    let store_ulid = store_id().as_ulid().to_string();

    let hello =
        pos_proto::protocol::Hello::current(store_id(), pos_proto::text::ReleaseTag::new("v1.0.0"));
    let answered = router
        .clone()
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{store_ulid}/events/hello"),
            &serde_json::to_value(&hello).expect("a hello encodes"),
            &token,
        ))
        .await
        .expect("route the hello");
    assert_eq!(answered.status(), StatusCode::OK);
    let outcome: pos_proto::protocol::HelloOutcome =
        serde_json::from_value(json_body(answered).await).expect("an outcome");
    assert_eq!(
        outcome,
        pos_proto::protocol::HelloOutcome::Accepted {
            protocol_version: pos_proto::PROTOCOL_VERSION
        }
    );

    let uri = format!("/sync/stores/{store_ulid}/events");
    let batch = serde_json::json!({ "events": fixtures::activations(store_id(), 1, 5) });
    for attempt in ["first", "replay"] {
        let published = router
            .clone()
            .oneshot(post_json_bearer(&uri, &batch, &token))
            .await
            .expect("route the publish");
        assert_eq!(published.status(), StatusCode::OK, "{attempt}");
        assert_eq!(
            json_body(published).await,
            serde_json::json!({ "accepted": 5 }),
            "{attempt}: the whole batch is held"
        );
    }
    let held = store
        .read(&EventQuery::first(
            store_id(),
            core::num::NonZeroU32::new(100).expect("positive"),
        ))
        .await
        .expect("read the log");
    assert_eq!(held.len(), 5, "the replay stored nothing twice");
}

/// No credential writes another shop's history (ADR-0143). A batch with any other store in it is
/// refused whole; so is a path naming another store, a store key without `publish_events`, and a
/// batch over the limit. A store key that holds the scope publishes, for a fork that prefers keys.
#[tokio::test]
async fn publishing_is_confined_to_the_credential_s_own_store() {
    let keys = FakeKeys::default();
    let store = FakeStore::new();
    let router = http::router(app_all(
        Cloud::new(store.clone()),
        FakeRollups::default(),
        keys.clone(),
        provisioned_admin(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ));
    let token = issue_device_credential(&keys, tenant(), store_id());
    let store_ulid = store_id().as_ulid().to_string();
    let uri = format!("/sync/stores/{store_ulid}/events");
    let elsewhere = StoreId::new(Ulid::from_u128(0x000E_15E1)); // another shop

    let mut mixed = fixtures::activations(store_id(), 1, 3);
    mixed.extend(fixtures::activations(elsewhere, 10, 1));
    let refused = router
        .clone()
        .oneshot(post_json_bearer(
            &uri,
            &serde_json::json!({ "events": mixed }),
            &token,
        ))
        .await
        .expect("route");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    let other_path = router
        .clone()
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{}/events", elsewhere.as_ulid()),
            &serde_json::json!({ "events": fixtures::activations(elsewhere, 20, 1) }),
            &token,
        ))
        .await
        .expect("route");
    assert_eq!(other_path.status(), StatusCode::FORBIDDEN);

    let legacy = issue_store_key(
        &keys,
        tenant(),
        store_id(),
        &[Scope::ReadConfig, Scope::RelayOrders],
    );
    let batch = serde_json::json!({ "events": fixtures::activations(store_id(), 30, 2) });
    let unscoped = router
        .clone()
        .oneshot(post_json_bearer(&uri, &batch, &legacy))
        .await
        .expect("route");
    assert_eq!(unscoped.status(), StatusCode::FORBIDDEN);

    let too_many = serde_json::json!({ "events": fixtures::activations(store_id(), 100, 257) });
    let oversized = router
        .clone()
        .oneshot(post_json_bearer(&uri, &too_many, &token))
        .await
        .expect("route");
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);

    let held = store
        .read(&EventQuery::first(
            store_id(),
            core::num::NonZeroU32::new(1_000).expect("positive"),
        ))
        .await
        .expect("read the log");
    assert!(held.is_empty(), "nothing refused was stored");

    let keyed = issue_store_key(&keys, tenant(), store_id(), &[Scope::PublishEvents]);
    let published = router
        .oneshot(post_json_bearer(&uri, &batch, &keyed))
        .await
        .expect("route");
    assert_eq!(published.status(), StatusCode::OK);
}

#[tokio::test]
async fn heartbeat_records_liveness_and_needs_the_read_config_scope() {
    // A lightweight heartbeat (ADR-0068 slice 2) records the store's contact without a config pull,
    // gated by the same read_config scope, and preserves any version a prior pull recorded.
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

    // No bearer → 401; a key scoped elsewhere → 403 (closed, not merely empty).
    let anon = router
        .clone()
        .oneshot(post_json(&uri, &serde_json::json!({})))
        .await
        .expect("route");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    let rollups_only = issue_key(&keys, tenant(), &[Scope::ReadRollups]);
    let wrong_scope = router
        .clone()
        .oneshot(post_json_bearer(
            &uri,
            &serde_json::json!({}),
            &rollups_only,
        ))
        .await
        .expect("route");
    assert_eq!(wrong_scope.status(), StatusCode::FORBIDDEN);

    // A read_config key records the contact and answers 204.
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);
    let beat = router
        .oneshot(post_json_bearer(&uri, &serde_json::json!({}), &token))
        .await
        .expect("route the heartbeat");
    assert_eq!(beat.status(), StatusCode::NO_CONTENT);
    let (held, seen_at) = config_trees
        .recorded_seen(tenant(), store_id())
        .expect("the heartbeat recorded liveness");
    assert_eq!(held, None, "a heartbeat carries no held version");
    assert_eq!(
        seen_at, NOW_MS,
        "the contact instant is the server clock's now"
    );
    assert_eq!(
        config_trees.recorded_outbox_depth(tenant(), store_id()),
        None,
        "a heartbeat that reports no depth records none — an empty object is not a reported zero"
    );
}

#[tokio::test]
async fn a_heartbeat_reports_the_stores_own_publish_backlog() {
    // Track O6: `EventStore::outbox_depth` had no production caller, so a store whose events were
    // piling up behind a down link looked identical to one perfectly in sync. The heartbeat is the
    // one rail that runs on a fixed interval whether or not the store has anything else to say, so
    // it carries the count — and only a count, never an event body (`docs/pos-spec.md` §13).
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

    let beat = router
        .clone()
        .oneshot(post_json_bearer(
            &uri,
            &serde_json::json!({ "outbox_depth": 128 }),
            &token,
        ))
        .await
        .expect("route the heartbeat");
    assert_eq!(beat.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        config_trees.recorded_outbox_depth(tenant(), store_id()),
        Some(128),
        "the depth the store reported is the depth the cloud recorded"
    );

    // An edge older than the body posts nothing at all, and must keep being recorded as alive
    // rather than refused for sending the empty body it has always sent — and its silence must not
    // erase the depth a newer report left standing.
    let bodyless = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("build the request");
    let beat = router
        .clone()
        .oneshot(bodyless)
        .await
        .expect("route the bodyless heartbeat");
    assert_eq!(beat.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        config_trees.recorded_outbox_depth(tenant(), store_id()),
        Some(128),
        "silence leaves the last reported depth standing rather than zeroing it"
    );

    // A body that is present and will not parse is the caller's mistake: swallowing it would let a
    // store report nothing forever while believing it reported.
    let malformed = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from("{\"outbox_depth\": \"lots\"}"))
        .expect("build the request");
    let refused = router
        .oneshot(malformed)
        .await
        .expect("route the malformed heartbeat");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn config_sync_is_closed_without_the_read_config_scope() {
    let keys = FakeKeys::default();
    let (router, config_token, store_ulid, _version) = published_config(&keys).await;
    let uri = format!("/sync/stores/{store_ulid}/config");

    // No bearer at all.
    let anon = router
        .clone()
        .oneshot(get(&uri, None))
        .await
        .expect("route");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    // A key scoped to something else is forbidden, not merely empty.
    let rollups_only = issue_key(&keys, tenant(), &[Scope::ReadRollups]);
    let wrong_scope = router
        .clone()
        .oneshot(get(&uri, Some(&rollups_only)))
        .await
        .expect("route");
    assert_eq!(
        wrong_scope.status(),
        StatusCode::FORBIDDEN,
        "read_rollups does not authorise config pull"
    );

    // The right scope, aimed at a *different* store, is forbidden — and the refusal happens before
    // the tree is read, so nothing about whether that store exists or has published leaks either
    // (S1). This used to be a `404`, which was the honest answer only because the sibling happened
    // to have nothing published; a sibling that *had* published was served.
    let other_store = Ulid::from_u128(0xBEEF).to_string();
    let unknown = router
        .oneshot(get(
            &format!("/sync/stores/{other_store}/config"),
            Some(&config_token),
        ))
        .await
        .expect("route");
    assert_eq!(unknown.status(), StatusCode::FORBIDDEN);
}
