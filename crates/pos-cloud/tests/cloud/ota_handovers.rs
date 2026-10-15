// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Store handovers on the lease: settling, retiring, an acknowledged bump and a hosted move.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditEntry, AuditSink, AuditStore};
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeConfigTrees, admin_cookie, bump_with_if_match, json_body, ota_app,
    ota_app_with_audit, post_with_cookie, provisioned_admin, store_id, tenant,
};

/// A handover that does not exist cannot be settled or retired, and neither route invents one.
#[tokio::test]
async fn neither_hand_made_act_works_on_a_store_with_no_handover() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let named = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "superseded_generation": 0,
    });

    // No lease at all: both are 404, rather than a handover conjured on a store that has none.
    for uri in ["/admin/config/lease/settle", "/admin/config/lease/retire"] {
        let refused = router
            .clone()
            .oneshot(post_with_cookie(uri, &named, &cookie))
            .await
            .expect("route the request");
        assert_eq!(
            refused.status(),
            StatusCode::NOT_FOUND,
            "{uri} on a store with no lease is a 404, not a handover"
        );
    }

    // A first lease supersedes nobody, so there is still nothing to settle — and the refusal says
    // so rather than succeeding quietly and putting an attestation about nothing in the trail.
    let issued = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the first issue");
    assert_eq!(issued.status(), StatusCode::OK);

    let nothing_to_settle = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/lease/settle",
            &named,
            &cookie,
        ))
        .await
        .expect("route the settle");
    assert_eq!(nothing_to_settle.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(nothing_to_settle).await["error"]["details"][0]["reason"],
        "NO_HANDOVER_IN_FLIGHT",
        "a settle against a store with no handover says so rather than succeeding quietly"
    );
}

/// The handover's two hand-made steps over the routes, in the order an operator lives them.
///
/// One test rather than four, because the states only mean anything in sequence: `retired` is
/// reachable only from `settled`, and `settled` only from an in-flight handover. Split apart, each
/// assertion would have to fabricate the state before it — which is the fabrication the
/// preconditions exist to prevent.
#[tokio::test]
async fn a_handover_settles_then_retires_and_each_step_refuses_out_of_order() {
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
    let store = serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid });
    let named = |generation: u64| {
        serde_json::json!({
            "tenant_id": tenant_ulid,
            "store_id": store_ulid,
            "superseded_generation": generation,
        })
    };

    // Generation 0, then 1 — so generation 0's machine is now the one being displaced.
    for if_match in ["*", "\"0\""] {
        let bumped = router
            .clone()
            .oneshot(bump_with_if_match(&store, &cookie, if_match))
            .await
            .expect("route the bump");
        assert_eq!(bumped.status(), StatusCode::OK);
    }

    // Retiring now is the refusal that matters most: the outgoing machine may hold the only copy of
    // a night's sales, and nobody gets to call it unnecessary until that is settled.
    let too_early = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/lease/retire",
            &store,
            &cookie,
        ))
        .await
        .expect("route the early retire");
    assert_eq!(too_early.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(too_early).await["error"]["details"][0]["reason"],
        "0",
        "the refusal names the generation still owing events"
    );

    // Attesting about the wrong machine is refused, and the body says which one is actually in
    // flight. This is the guard against a stale console closing a handover nobody looked at.
    let wrong_machine = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/lease/settle",
            &named(7),
            &cookie,
        ))
        .await
        .expect("route the mis-aimed settle");
    assert_eq!(wrong_machine.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(wrong_machine).await["error"]["details"][0]["reason"],
        "0",
        "the refusal names the handover that is actually in flight"
    );

    // The operator has read the powered-off box and attests it is empty; now the machine can be
    // decided unnecessary. A second retirement is refused rather than overwriting the first
    // decision's who and when — the row's entire job is to hold the first.
    for (uri, body, expected) in [
        (
            "/admin/config/lease/settle",
            named(0),
            StatusCode::NO_CONTENT,
        ),
        (
            "/admin/config/lease/retire",
            store.clone(),
            StatusCode::NO_CONTENT,
        ),
        (
            "/admin/config/lease/retire",
            store.clone(),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(post_with_cookie(uri, &body, &cookie))
            .await
            .expect("route the request");
        assert_eq!(response.status(), expected, "{uri}");
    }

    assert_both_decisions_are_in_the_trail(&audit).await;
}

/// A store on its first lease has nothing to retire, and says so rather than recording a decision
/// ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
///
/// Generation `0` is a store's first-ever lease: it supersedes nobody, so there is no previous
/// machine and no handover. Before the `generation > 0` precondition the other two conditions both
/// held on such a store, and a retire recorded that its *only* machine — the one selling in the shop
/// — was no longer needed. The console never offered it (a gen-0 store derives no handover state, so
/// there is no button), which is exactly why the route needed the guard: nothing an operator can see
/// was stopping them.
///
/// `422` and not `404`: the store is there, and the act does not apply to it.
#[tokio::test]
async fn a_store_on_its_first_lease_has_no_previous_machine_to_retire() {
    let config_trees = FakeConfigTrees::default();
    let router = ota_app_with_audit(
        provisioned_admin(),
        config_trees.clone(),
        Arc::new(AuditSink::new(FakeAudit::default())),
    );
    let cookie = admin_cookie(&router).await;
    let store = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
    });

    // One bump: the store now holds generation 0 and has never handed over.
    let bumped = router
        .clone()
        .oneshot(bump_with_if_match(&store, &cookie, "*"))
        .await
        .expect("route the bump");
    assert_eq!(bumped.status(), StatusCode::OK);

    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/config/lease/retire",
            &store,
            &cookie,
        ))
        .await
        .expect("route the retire");
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(refused).await;
    assert_eq!(
        body["error"]["details"][0]["field"], "generation",
        "the refusal names the generation that makes the act inapplicable: {body}"
    );
    assert_eq!(body["error"]["details"][0]["reason"], "0");

    // And nothing was written: a second attempt gets the same refusal, not "already retired". That
    // is the assertion that would have caught the original defect — a recorded decision would make
    // this one `AlreadyRetired`.
    let again = router
        .oneshot(post_with_cookie(
            "/admin/config/lease/retire",
            &store,
            &cookie,
        ))
        .await
        .expect("route the second retire");
    assert_eq!(again.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json_body(again).await["error"]["details"][0]["field"],
        "generation",
        "the first refusal recorded nothing, so the second is the same refusal"
    );
}

/// Both hand-made decisions reached the trail, and the retirement names its decider **by id**.
///
/// The identifier matters: the row carries the admin's ULID and the trail carries the email as it
/// stood, so a reader with the standing to resolve one gets a person and a reader without one gets
/// an opaque id. Asserting both halves here is what keeps an address from drifting into the
/// operational row later.
async fn assert_both_decisions_are_in_the_trail(audit: &FakeAudit) {
    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let actions: Vec<&str> = recorded.iter().map(|entry| entry.action.as_str()).collect();
    assert!(
        actions.contains(&"config.lease.settle") && actions.contains(&"config.lease.retire"),
        "both hand-made decisions are recorded, found {actions:?}"
    );
    let retire = recorded
        .iter()
        .find(|entry| entry.action == "config.lease.retire")
        .expect("the retirement is recorded");
    let after = retire
        .after
        .clone()
        .expect("a retirement records what it wrote");
    assert_eq!(
        after["retired_by"], retire.actor.admin_id,
        "the row's decider and the trail's actor are the same admin, by id"
    );
    assert!(
        after["retired_by"] != *retire.actor.email,
        "the operational row carries the id, never the address — the trail is where an email belongs"
    );
}

/// A bump that abandons an undrained machine records *which* generation it abandoned.
///
/// The number is nowhere else. An acknowledged bump does not clear `superseded_generation`; it rolls
/// it forward to the machine being displaced now. So without this the fact that an earlier
/// machine's sales were written off, and by whom, would exist in no record at all.
#[tokio::test]
async fn an_acknowledged_bump_records_the_generation_it_abandoned() {
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
    let store = serde_json::json!({ "tenant_id": tenant_ulid, "store_id": store_ulid });

    for if_match in ["*", "\"0\""] {
        let response = router
            .clone()
            .oneshot(bump_with_if_match(&store, &cookie, if_match))
            .await
            .expect("route the bump");
        assert_eq!(response.status(), StatusCode::OK);
    }

    // Generation 0 is still in flight, so this third bump has to say it is abandoning it.
    let abandoning = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "acknowledge_undrained": 0,
            }),
            &cookie,
            "\"1\"",
        ))
        .await
        .expect("route the abandoning bump");
    assert_eq!(abandoning.status(), StatusCode::OK);

    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let latest = recorded
        .iter()
        .find(|entry| entry.action == "config.lease.bump")
        .expect("a bump was audited")
        .after
        .clone()
        .expect("a bump records what it wrote");
    assert_eq!(
        latest["abandoned_generation"], 0,
        "the trail names the generation whose events were written off"
    );
    assert_eq!(
        latest["generation"], 2,
        "and the generation the store moved to, which is a different number"
    );
}

/// ADR-0114: a hosted edge placement moves a whole store's operational data — three parts of it
/// personal — onto a machine somebody chose, and until this landed nothing recorded *where*.
///
/// The move and the record are one request, because they are one act. A second route that could
/// edit the region on its own would let the record say a store rests in Singapore while the machine
/// holding its lease sits in Tokyo.
#[tokio::test]
async fn a_hosted_move_records_where_the_store_now_rests_and_the_trail_names_it() {
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

    let moved = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "edge_placement": "EDGE_PLACEMENT_HOSTED_BY_OPERATOR",
                "region_country": "sg",
                "region_label": "  ap-southeast-1  ",
            }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the hosted move");
    assert_eq!(moved.status(), StatusCode::OK);

    let entry = |audit: &[AuditEntry]| {
        audit
            .iter()
            .find(|entry| entry.action == "config.lease.bump")
            .expect("a bump was audited")
            .after
            .clone()
            .expect("a bump records what it wrote")
    };
    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let after = entry(&recorded);
    assert_eq!(
        after["region_country"], "SG",
        "the code is normalised on the way in, so the console's comparison against a store's \
         published locale is an equality test rather than a case fold"
    );
    assert_eq!(
        after["region_label"], "ap-southeast-1",
        "the label is trimmed, because the emptiness and length checks ran on the trimmed value"
    );
    assert_eq!(after["edge_placement"], "EDGE_PLACEMENT_HOSTED_BY_OPERATOR");

    // A swap in place: the machine is replaced, the *place* is not. The trail has to keep naming
    // the region, because a reader asking "where has this store been running since March" is asking
    // about the store, not about what this particular request happened to mention.
    let swapped = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "acknowledge_undrained": 0,
            }),
            &cookie,
            "\"0\"",
        ))
        .await
        .expect("route the swap");
    assert_eq!(swapped.status(), StatusCode::OK);
    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let after = entry(&recorded);
    assert_eq!(after["region_country"], "SG");
    assert_eq!(after["region_label"], "ap-southeast-1");
    assert_eq!(
        after["edge_placement_moved"], false,
        "and the trail still distinguishes a move from a swap, which is the flag ADR-0110 added"
    );

    // Moving the store back into its shop ends the region: an in-store machine is in the shop, and
    // a stale `SG` on the row would answer the transfer question with a lie.
    let home = router
        .clone()
        .oneshot(bump_with_if_match(
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "edge_placement": "EDGE_PLACEMENT_IN_STORE",
                "acknowledge_undrained": 0,
            }),
            &cookie,
            "\"1\"",
        ))
        .await
        .expect("route the move home");
    assert_eq!(home.status(), StatusCode::OK);
    let recorded = audit.list(None, 20).await.expect("list audit entries");
    let after = entry(&recorded);
    assert!(
        after["region_country"].is_null() && after["region_label"].is_null(),
        "an in-store placement has no region, and the columns are cleared by the same statement"
    );
}
