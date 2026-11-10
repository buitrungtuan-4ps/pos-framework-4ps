// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A version's flows (ADR-0192) and reactions (ADR-0193 decision 5) are published and checked at
//! publish with the evaluator the edge applies them with, and a dry run says which flow, which step
//! or which reaction a refusal is about, so the console can show it in place.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::NoopAuditRecorder;

use super::{package_app, send, version_body};
use crate::{admin_cookie, get_with_cookie, post_with_cookie, provisioned_admin, ulid_text};

/// Seat, ask the guests (an edge guard), then take the order; a reaction opens the menu once an
/// order is opened, on dine-in only.
fn parts() -> (serde_json::Value, serde_json::Value) {
    let flows = serde_json::json!([{
        "body": {
            "flow_id": "seat_and_order",
            "start_action_id": "sales.order.open",
            "steps": [
                { "step_id": "guests", "screen_id": "guest_count", "kind": "STEP_KIND_REQUIRED",
                  "done_when": "has(order.guest_count) && order.guest_count >= 1" },
                { "step_id": "order", "action_id": "sales.line.add", "kind": "STEP_KIND_REQUIRED",
                  "prerequisites": ["guests"] }
            ]
        }
    }]);
    let reactions = serde_json::json!([{
        "body": {
            "reaction_id": "open_menu",
            "when": "REACTION_TRIGGER_ORDER_OPENED",
            "if": "order.channel == \"SALES_CHANNEL_DINE_IN\"",
            "then": { "action_id": "ui.screen.open", "params": { "screen_id": "menu" } }
        }
    }]);
    (flows, reactions)
}

fn body(
    tenant: &str,
    flows: &serde_json::Value,
    reactions: &serde_json::Value,
) -> serde_json::Value {
    let mut body = version_body(tenant, "March", "[]");
    body["flows"] = flows.clone();
    body["reactions"] = reactions.clone();
    body
}

/// Paths for a fresh package of `tenant`: its versions and their dry run.
async fn package(router: &axum::Router, cookie: &str, tenant: &str) -> (String, String) {
    let (_, package) = send(
        router,
        post_with_cookie(
            "/admin/packages",
            &serde_json::json!({ "tenant_id": tenant, "name": "Dine-in" }),
            cookie,
        ),
    )
    .await;
    let versions = format!(
        "/admin/packages/{}/versions",
        package["package_id"].as_str().expect("an id")
    );
    let dry_run = format!("{versions}/dry-run");
    (versions, dry_run)
}

/// A flow and a reaction the edge would apply are published as written, and the version keeps them.
#[tokio::test]
async fn flows_and_reactions_are_published_as_written() {
    let router = package_app(provisioned_admin(), Arc::new(NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let (versions, dry_run) = package(&router, &cookie, &tenant).await;
    let (flows, reactions) = parts();

    let (status, verdict) = send(
        &router,
        post_with_cookie(&dry_run, &body(&tenant, &flows, &reactions), &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verdict}");
    assert_eq!(verdict["accepted"], true, "{verdict}");
    assert_eq!(verdict["min_edge_version"], "0.14.1");

    let (status, published) = send(
        &router,
        post_with_cookie(&versions, &body(&tenant, &flows, &reactions), &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(published["content"]["flows"], flows);
    assert_eq!(published["content"]["reactions"], reactions);
    let (_, listed) = send(
        &router,
        get_with_cookie(&format!("{versions}?tenant_id={tenant}"), &cookie),
    )
    .await;
    assert_eq!(listed[0]["content"]["flows"], flows);
}

/// Each refusal says which flow and step, or which reaction, it is about, and the publish refuses
/// what the dry run does: steps that wait on each other, an edge guard that reads the till's
/// `role`, a condition outside the dialect, and a reaction that would move money.
#[tokio::test]
async fn a_refusal_names_the_flow_and_step_or_the_reaction() {
    let router = package_app(provisioned_admin(), Arc::new(NoopAuditRecorder));
    let cookie = admin_cookie(&router).await;
    let tenant = ulid_text(1);
    let (versions, dry_run) = package(&router, &cookie, &tenant).await;
    let (flows, reactions) = parts();

    let mut cycle = flows.clone();
    cycle[0]["body"]["steps"][0]["prerequisites"] = serde_json::json!(["order"]);
    let mut till_guard = flows.clone();
    till_guard[0]["body"]["steps"][0]["done_when"] = serde_json::json!("role == \"manager\"");
    let mut looping = flows.clone();
    looping[0]["body"]["steps"][0]["applies_when"] =
        serde_json::json!("order.items.all(item, item.qty > 0)");
    let mut second = flows.clone();
    second.as_array_mut().expect("flows").insert(
        0,
        serde_json::json!({ "body": { "flow_id": "tip", "start_action_id": "sales.line.add",
            "steps": [{ "step_id": "ask", "screen_id": "tip", "kind": "STEP_KIND_OPTIONAL" }] } }),
    );
    second[1]["body"]["steps"][1]["prerequisites"] = serde_json::json!(["nowhere"]);

    for (flows, flow_index, step_id, reason, name) in [
        (&cycle, 0, "guests", "CYCLE", None),
        (&till_guard, 0, "guests", "CONDITION", None),
        (&looping, 0, "guests", "CONDITION", None),
        (&second, 1, "order", "UNKNOWN_PREREQUISITE", Some("nowhere")),
    ] {
        let draft = body(&tenant, flows, &reactions);
        let (status, verdict) = send(&router, post_with_cookie(&dry_run, &draft, &cookie)).await;
        assert_eq!(status, StatusCode::OK, "{verdict}");
        assert_eq!(verdict["accepted"], false, "{verdict}");
        let refusal = &verdict["refusals"][0];
        assert_eq!(refusal["field"], "flows", "{verdict}");
        assert_eq!(refusal["reason"], reason, "{verdict}");
        assert_eq!(refusal["flow_index"], flow_index, "{verdict}");
        assert_eq!(refusal["step_id"], step_id, "{verdict}");
        assert_eq!(
            refusal["name"],
            name.map_or(serde_json::Value::Null, Into::into)
        );
        let (status, body) = send(&router, post_with_cookie(&versions, &draft, &cookie)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    let mut money = reactions.clone();
    money.as_array_mut().expect("reactions").push(
        serde_json::json!({ "body": { "reaction_id": "auto_settle",
            "when": "REACTION_TRIGGER_BILL_OPENED",
            "then": { "action_id": "billing.bill.void", "params": {} } } }),
    );
    let draft = body(&tenant, &flows, &money);
    let (_, verdict) = send(&router, post_with_cookie(&dry_run, &draft, &cookie)).await;
    let refusal = &verdict["refusals"][0];
    assert_eq!(refusal["field"], "reactions", "{verdict}");
    assert_eq!(refusal["reason"], "ACT_NOT_ALLOWED", "{verdict}");
    assert_eq!(refusal["reaction_index"], 1, "{verdict}");
    assert_eq!(refusal["name"], "billing.bill.void");
    let (status, _) = send(&router, post_with_cookie(&versions, &draft, &cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, listed) = send(
        &router,
        get_with_cookie(&format!("{versions}?tenant_id={tenant}"), &cookie),
    )
    .await;
    assert_eq!(listed, serde_json::json!([]), "nothing refused was stored");
}
