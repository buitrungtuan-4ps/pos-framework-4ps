// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The OTA rollout levers (ADR-0078): rollouts and placement on their own layers, the lease bump,
//! and the region and placement refusals.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditSink, AuditStore};
use pos_cloud::config_tree::ConfigTreeStore;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeConfigTrees, admin_cookie, bump_with_if_match, get, get_with_cookie, json_body,
    ota_app, ota_app_with_audit, post_with_cookie, provisioned_admin, put_json, put_with_cookie,
    store_id, tenant,
};

#[tokio::test]
async fn a_rollout_and_a_placement_land_on_their_own_config_layers() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let published = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/ota",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "target_version": "1.4.0",
                "min_ring": "fleet",
                "rollout_percent": 40,
                "signing_key_id": "a1a1a1a1a1a1a1a1",
            }),
            &cookie,
        ))
        .await
        .expect("route the rollout publish");
    assert_eq!(published.status(), StatusCode::OK);

    let placed = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/ota/placement",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "ring": "fleet",
                "canary_bucket": 10,
            }),
            &cookie,
        ))
        .await
        .expect("route the placement publish");
    assert_eq!(placed.status(), StatusCode::OK);

    // The two nodes sit on different layers, and publishing the second does not disturb the first —
    // the rollout is a Store-level fact, the placement a Device-level one.
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    assert_eq!(
        state.record.layers[2]["fleet_update"]["target_version"],
        "1.4.0"
    );
    assert_eq!(
        state.record.layers[2]["fleet_update"]["rollout_percent"],
        40
    );
    assert_eq!(state.record.layers[3]["device_ota"]["ring"], "fleet");
    assert_eq!(state.record.layers[3]["device_ota"]["canary_bucket"], 10);

    // And both reach the *effective* document, which is what a store actually pulls — a node authored
    // onto a layer nothing merges would be invisible to the edge that has to read it.
    let effective = &state
        .record
        .history
        .last()
        .expect("a published version")
        .effective;
    assert_eq!(effective["fleet_update"]["min_ring"], "fleet");
    assert_eq!(effective["device_ota"]["canary_bucket"], 10);

    // The read-backs answer from the layer each node was authored onto.
    let rollout = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/ota?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the rollout read");
    assert_eq!(rollout.status(), StatusCode::OK);
    assert_eq!(json_body(rollout).await["target_version"], "1.4.0");

    let placement = router
        .oneshot(get_with_cookie(
            &format!("/admin/config/ota/placement?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the placement read");
    assert_eq!(placement.status(), StatusCode::OK);
    assert_eq!(json_body(placement).await["canary_bucket"], 10);
}

/// A replacement box numbers above what the cloud has seen
/// ([ADR-0149](../../../../docs/adr/0149-a-replacement-box-numbers-above-what-the-cloud-has-seen.md)):
/// the bump publishes the highest receipt number the log holds as `receipt_floor`, in the **same**
/// version as the `lease` node, and a log with no settled bill publishes no floor at all.
#[tokio::test]
async fn a_lease_bump_publishes_the_receipt_floor_beside_the_lease() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
    });

    let first = router
        .clone()
        .oneshot(bump_with_if_match(&body, &cookie, "*"))
        .await
        .expect("route the first bump");
    assert_eq!(first.status(), StatusCode::OK);
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    assert!(
        state.record.layers[2].get("receipt_floor").is_none(),
        "a store the cloud has seen no receipt from gets no floor"
    );

    *config_trees.highest_receipt.lock().expect("lock") = Some(40);
    let second = router
        .clone()
        .oneshot(bump_with_if_match(&body, &cookie, "\"0\""))
        .await
        .expect("route the second bump");
    assert_eq!(second.status(), StatusCode::OK);
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    let effective = &state
        .record
        .history
        .last()
        .expect("a published version")
        .effective;
    assert_eq!(
        effective["receipt_floor"],
        serde_json::json!({ "number": 40 })
    );
    assert_eq!(
        effective["lease"],
        serde_json::json!({ "generation": 1 }),
        "the floor and the lease arrive in the same version"
    );
}

#[tokio::test]
async fn a_lease_bump_advances_the_authority_and_publishes_the_node_the_store_reads() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // A store nobody has issued a lease to reads `null` — "no lease in force", which the edge treats
    // as `Active` so the fleet behaves exactly as it did before the mechanism existed. It is
    // deliberately not generation `0`, which is a real first lease (ADR-0108, ADR-0049).
    let unissued = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/lease?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the lease read");
    assert_eq!(unissued.status(), StatusCode::OK);
    assert!(json_body(unissued).await["generation"].is_null());

    // The first bump establishes the counter at generation 0 and supersedes nobody.
    let first = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the first bump");
    assert_eq!(first.status(), StatusCode::OK);
    assert!(
        !json_body(first).await["config_version_id"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "a bump publishes a config version, because the node is how the store learns"
    );

    // The second bump is the act of replacing the machine: the counter moves, and the Store layer
    // now carries a `lease` node the edge will weigh its held generation against.
    let second = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
            "\"0\"",
        ))
        .await
        .expect("route the second bump");
    assert_eq!(second.status(), StatusCode::OK);

    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/lease?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the lease read");
    assert_eq!(
        json_body(read).await["generation"],
        1,
        "the only write is a bump: 0, then 1, and there is no way to set a value"
    );

    // The published node is what the edge parses, on the **Store** layer — not the Device layer the
    // placement uses. `device_ota` decides which release a store takes; `lease` decides whether this
    // box is the store, and merging them would let a placement edit touch the lease.
    let state = config_trees
        .load(tenant(), store_id())
        .await
        .expect("load")
        .expect("a published tree");
    assert_eq!(
        state.record.layers[2]["lease"],
        serde_json::json!({ "generation": 1 }),
        "the node is derived from the counter, so the two cannot drift"
    );
    // And it reaches the *effective* document, which is what a store actually pulls: a node authored
    // onto a layer nothing merges would be invisible to the edge that has to weigh it.
    let effective = &state
        .record
        .history
        .last()
        .expect("a published version")
        .effective;
    let parsed: pos_core::lease::LeaseConfig = serde_json::from_value(effective["lease"].clone())
        .expect("the edge's own type reads the effective node back");
    assert_eq!(
        parsed.generation(),
        pos_core::lease::LeaseGeneration::new(1)
    );
}

/// The `edge_placement` recorded by the most recent `config.lease.bump`, and whether that bump chose
/// it. Read from the trail rather than from the fake's own map, because the trail is what an
/// operator answering "when did this store become hosted, and who moved it?" actually has.
async fn last_bump_placement(audit: &FakeAudit) -> (String, bool) {
    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let after = recorded
        .iter()
        .find(|entry| entry.action == "config.lease.bump")
        .expect("a bump was audited")
        .after
        .clone()
        .expect("a bump records what it wrote");
    (
        after["edge_placement"]
            .as_str()
            .expect("the trail names the placement")
            .to_owned(),
        after["edge_placement_moved"]
            .as_bool()
            .expect("the trail says whether this bump moved the store"),
    )
}

#[tokio::test]
async fn a_bump_is_the_only_thing_that_writes_where_a_store_runs() {
    let audit = FakeAudit::default();
    let config_trees = FakeConfigTrees::default();
    let router = ota_app_with_audit(
        provisioned_admin(),
        config_trees.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // A first bump that names nothing puts the store under the lease where every store in the fleet
    // already is. `IN_STORE` is the column's default rather than something the route invents, so a
    // console that never learned the field keeps working and keeps telling the truth.
    let first = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the first bump");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        last_bump_placement(&audit).await,
        ("EDGE_PLACEMENT_IN_STORE".to_owned(), false),
        "a bump that named no placement did not move the store"
    );

    // Naming one is a move, and it is the same act as issuing the generation — one request, one
    // write. The trail records both halves, so "who moved this store off the shop LAN, and when"
    // has an answer that does not require reading a second table at a second time (ADR-0110).
    //
    // The region rides the same request, and is required here rather than optional: a hosted
    // placement without one is the state ADR-0114 exists to prevent, so the route refuses this body
    // without it. Which is why this test names a region and the swap below does not.
    let moved = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "edge_placement": "EDGE_PLACEMENT_HOSTED_BY_PLATFORM",
                "region_country": "SG",
                "region_label": "ap-southeast-1",
            }),
            &cookie,
            "\"0\"",
        ))
        .await
        .expect("route the move");
    assert_eq!(moved.status(), StatusCode::OK);
    assert_eq!(
        last_bump_placement(&audit).await,
        ("EDGE_PLACEMENT_HOSTED_BY_PLATFORM".to_owned(), true),
        "naming a placement moves the store, and the trail says so"
    );

    // ADR-0003's swap: the hosted box is replaced by another hosted box. The generation moves and
    // the placement does not, because the request named none. This is the case that would break if
    // a bump silently reset the column — a store quietly readvertised as offline-capable when it is
    // not, which is the one direction ADR-0110 calls dangerous.
    let swapped = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                // Generation 0 was left undrained by the move above, so this swap has to say it is
                // abandoning it. Without the acknowledgement the route refuses — which is the whole
                // point, and is asserted on its own elsewhere.
                "acknowledge_undrained": 0,
            }),
            &cookie,
            "\"1\"",
        ))
        .await
        .expect("route the swap");
    assert_eq!(swapped.status(), StatusCode::OK);
    assert_eq!(
        last_bump_placement(&audit).await,
        ("EDGE_PLACEMENT_HOSTED_BY_PLATFORM".to_owned(), false),
        "a swap in place keeps the placement it had"
    );

    // The generation kept moving throughout: 0, 1, 2. The placement rode along inside those writes
    // rather than beside them.
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/lease?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the lease read");
    assert_eq!(json_body(read).await["generation"], 2);
}

/// Five refusals, each naming the one field an admin can fix, and none of them issuing a
/// generation on the way to refusing ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
#[tokio::test]
async fn a_region_that_does_not_belong_or_does_not_parse_is_refused_by_field() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let hosted = "EDGE_PLACEMENT_HOSTED_BY_PLATFORM";

    // (what the body adds, which field the refusal must name, what the case is)
    let cases: [(serde_json::Value, &str, &str); 7] = [
        (
            serde_json::json!({ "edge_placement": hosted }),
            "region_country",
            "a hosted placement with no region at all",
        ),
        (
            serde_json::json!({ "edge_placement": hosted, "region_country": "SG" }),
            "region_country",
            "a country with no label names a place nobody can picture",
        ),
        (
            serde_json::json!({ "edge_placement": hosted, "region_label": "a rack" }),
            "region_country",
            "a label with no country is missing the one field the framework reads",
        ),
        (
            serde_json::json!({
                "edge_placement": hosted,
                "region_country": "Singapore",
                "region_label": "a rack",
            }),
            "region_country",
            "the country is an alpha-2 code, not a name",
        ),
        (
            serde_json::json!({
                "edge_placement": hosted,
                "region_country": "SG",
                "region_label": "   ",
            }),
            "region_label",
            "whitespace is not a place",
        ),
        (
            serde_json::json!({
                "edge_placement": "EDGE_PLACEMENT_IN_STORE",
                "region_country": "SG",
                "region_label": "a rack",
            }),
            "region_country",
            "an in-store machine is in the shop and has no region",
        ),
        (
            serde_json::json!({ "region_country": "JP", "region_label": "Sakai" }),
            "region_country",
            "a bump naming no placement is a swap in place, so this would edit the region alone",
        ),
    ];

    for (extra, field, why) in cases {
        let mut body = serde_json::json!({
            "tenant_id": tenant_ulid,
            "store_id": store_ulid,
        });
        for (key, value) in extra.as_object().expect("an object of extra fields") {
            body[key] = value.clone();
        }
        let refused = router
            .clone()
            .oneshot(post_with_cookie("/admin/config/lease/bump", &body, &cookie))
            .await
            .expect("route the bump");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{why}");
        let payload = json_body(refused).await;
        assert_eq!(payload["error"]["details"][0]["field"], field, "{why}");
    }

    // And nothing moved. Every refusal above ran before the counter did, so this store is still one
    // nobody has leased — a refusal that had already bumped would supersede a box over a typo.
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/lease?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the lease read");
    assert!(json_body(read).await["generation"].is_null());
}

/// The label's ceiling is 64 *characters*, not bytes
/// ([ADR-0114](../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// A byte limit would give a fork writing Japanese a third of the field a fork writing English
/// gets, for a value neither of them parses. 64 Japanese characters is 192 bytes, so a byte limit
/// refuses the case this asserts must pass.
#[tokio::test]
async fn the_region_labels_ceiling_counts_characters_rather_than_bytes() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let hosted = "EDGE_PLACEMENT_HOSTED_BY_PLATFORM";

    for (label, expected) in [
        ("堺".repeat(64), StatusCode::OK),
        ("堺".repeat(65), StatusCode::BAD_REQUEST),
    ] {
        let response = router
            .clone()
            .oneshot(bump_with_if_match(
                &serde_json::json!({
                    "tenant_id": tenant_ulid,
                    "store_id": store_ulid,
                    "edge_placement": hosted,
                    "region_country": "JP",
                    "region_label": label,
                }),
                &cookie,
                "*",
            ))
            .await
            .expect("route the bump");
        assert_eq!(
            response.status(),
            expected,
            "{} characters",
            label.len() / 3
        );
    }

    // The accepted label issued exactly one generation; the refused one issued none.
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/lease?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the lease read");
    assert_eq!(
        json_body(read).await["generation"],
        0,
        "a refusal that had already bumped would supersede a box over a typo in a label"
    );
}

#[tokio::test]
async fn a_placement_the_framework_does_not_define_is_refused_by_name() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Three refusals that all have to be refusals, and would each be a different silent bug:
    //
    //   * a token from nowhere — a typo, or a console built against a fork that added a mode;
    //   * the empty string — a form field the operator opened and left blank, which must not read
    //     as "keep what you had", because that hides the mistake;
    //   * an explicit UNSPECIFIED — the wire's "this message did not say", which looks like an
    //     answer and is not. Omitting the field is how you mean that.
    for token in [
        "EDGE_PLACEMENT_ON_THE_MOON",
        "",
        "EDGE_PLACEMENT_UNSPECIFIED",
    ] {
        let refused = router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/config/lease/bump",
                &serde_json::json!({
                    "tenant_id": tenant_ulid,
                    "store_id": store_ulid,
                    "edge_placement": token,
                }),
                &cookie,
            ))
            .await
            .expect("route the bump");
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "{token} is not a placement this framework defines"
        );
        let body = json_body(refused).await;
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("EDGE_PLACEMENT_HOSTED_BY_OPERATOR"),
            "the refusal names what it accepts, not only what it rejected: {message}"
        );
        assert_eq!(body["error"]["details"][0]["field"], "edge_placement");
    }

    // And nothing moved. A refused bump must not have issued a generation on the way to refusing:
    // the store is still one nobody has leased.
    let read = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/lease?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the lease read");
    assert!(
        json_body(read).await["generation"].is_null(),
        "a refusal that had already bumped the counter would supersede a box for a typo"
    );
}

#[tokio::test]
async fn an_unplaced_store_reads_null_and_an_illegal_placement_is_refused_with_reasons() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // A store nobody has placed reads `null`, not a fabricated ring. The edge treats that as "no
    // placement" and installs nothing, which is the safe end of the trade (ADR-0048).
    let unplaced = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/config/ota/placement?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the placement read");
    assert_eq!(unplaced.status(), StatusCode::OK);
    assert!(json_body(unplaced).await.is_null());

    // A ring that names no ring and a bucket past 99 are refused by the shared `pos-core` rules the
    // edge also runs, with every violation at once rather than one per attempt.
    let refused = router
        .clone()
        .oneshot(put_with_cookie(
            "/admin/config/ota/placement",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "ring": "everyone",
                "canary_bucket": 200,
            }),
            &cookie,
        ))
        .await
        .expect("route the placement publish");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let violations = json_body(refused).await;
    assert_eq!(violations["error"]["status"], "UNPROCESSABLE");
    let listed = violations["error"]["message"]
        .as_str()
        .expect("the violations join into the message");
    assert!(
        listed.contains("device_ota.ring"),
        "the bad ring is named: {listed:?}"
    );
    assert!(
        listed.contains("device_ota.canary_bucket"),
        "the bad bucket is named: {listed:?}"
    );

    // The refusal changed nothing: the store is still unplaced, so a rejected publish cannot leave a
    // half-applied placement behind.
    assert!(
        config_trees
            .load(tenant(), store_id())
            .await
            .expect("load")
            .is_none(),
        "a refused publish persists no tree"
    );
}

#[tokio::test]
async fn the_placement_routes_require_a_session() {
    let router = ota_app(provisioned_admin(), FakeConfigTrees::default());
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let write = router
        .clone()
        .oneshot(put_json(
            "/admin/config/ota/placement",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "ring": "fleet",
                "canary_bucket": 10,
            }),
        ))
        .await
        .expect("route the placement publish");
    assert_eq!(write.status(), StatusCode::UNAUTHORIZED);

    let read = router
        .oneshot(get(
            &format!("/admin/config/ota/placement?tenant_id={tenant_ulid}&store_id={store_ulid}"),
            None,
        ))
        .await
        .expect("route the placement read");
    assert_eq!(read.status(), StatusCode::UNAUTHORIZED);
}
