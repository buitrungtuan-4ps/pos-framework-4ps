// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The device publish (`POST /admin/devices/publish`, ADR-0100): the `devices` node, terminals,
//! print agents, cash drawers and paper.

use axum::http::StatusCode;
use pos_cloud::auth::apikey::Scope;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, FakeKeys, admin_cookie, approved_device_version, create_terminal,
    device_publish_app, get_with_cookie, issue_store_key, json_body, post_config_with_etag,
    post_with_cookie, propose_printer, published_device, store_id, tenant,
};

#[tokio::test]
async fn approved_devices_compile_into_the_published_config_node() {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    // Two devices proposed; only one approved.
    let counter =
        propose_printer(&router, &token, &store_ulid, "Counter", "192.168.1.51:9100").await;
    let oven = propose_printer(&router, &token, &store_ulid, "Oven", "192.168.1.52:9100").await;

    let approved = router
        .clone()
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{counter}/approve?tenant_id={tenant_ulid}&connection=usb"
            ),
            &serde_json::json!({}) as &serde_json::Value,
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approved.status(), StatusCode::NO_CONTENT);

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/devices/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid.clone(), "store_id": store_ulid.clone() }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let summary = json_body(published).await;
    assert_eq!(summary["device_count"], 1, "only the approved device");
    assert_eq!(summary["skipped_count"], 0);

    // The store reads the node the way its edge will: through the config pull.
    let effective = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the config read");
    let document = json_body(effective).await;
    let listed = document["devices"]["devices"]
        .as_array()
        .expect("the devices node is an array");
    assert_eq!(listed.len(), 1);
    let device = listed.first().expect("one device");
    assert_eq!(
        device["device_id"], counter,
        "the proposal's id is the device's"
    );
    assert_eq!(device["address"], "192.168.1.51:9100");
    assert_eq!(device["kind"], "DEVICE_KIND_PRINTER");
    assert_eq!(
        device["connection"], "DEVICE_CONNECTION_USB",
        "the node carries the prefixed token, whatever the column spells"
    );
    assert!(
        device["station_id"].is_null(),
        "no station named means the counter's receipt printer"
    );
    assert_ne!(
        device["device_id"], oven,
        "the unapproved one is not published"
    );
}

/// A terminal is created in the console, and the publish carries it rather than skipping it
/// (**ADR-0112**).
///
/// Two claims in one path, because they only matter together. The connectionless-row skip exists to
/// stop a USB printer's cash drawer being published as a network device's; a terminal has neither a
/// drawer nor an address, so the skip must not take it — and if it did, no printer could ever name
/// an agent, because the reference would never resolve.
#[tokio::test]
async fn a_terminal_is_published_with_no_address_and_an_unspecified_connection() {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let till = create_terminal(&router, &cookie, &tenant_ulid, &store_ulid, "Till 1").await;

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/devices/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid.clone(), "store_id": store_ulid.clone() }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let summary = json_body(published).await;
    assert_eq!(
        summary["device_count"], 1,
        "the terminal is published, not skipped for having no connection"
    );
    assert_eq!(summary["skipped_count"], 0);

    let effective = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the config read");
    let document = json_body(effective).await;
    let listed = document["devices"]["devices"]
        .as_array()
        .expect("the devices node is an array");
    let device = listed.first().expect("the terminal");
    assert_eq!(device["device_id"], till);
    assert_eq!(device["kind"], "DEVICE_KIND_TERMINAL");
    assert_eq!(
        device["address"], "",
        "nothing dials a terminal: the agent connects outbound to the edge"
    );
    assert_eq!(
        device["connection"], "DEVICE_CONNECTION_UNSPECIFIED",
        "not a guess — a terminal is not attached to the edge at all"
    );
}

/// A printer names the terminal whose transport reaches it, and the node carries the pointer
/// (**ADR-0112**).
///
/// Asserted end to end — create the terminal, approve the printer, pick the agent, publish, read the
/// tree — because the value of this slice is that the four sit on one path. The pick alone proves
/// nothing about what a store receives.
#[tokio::test]
async fn a_printer_names_its_agent_and_the_published_node_carries_it() {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let till = create_terminal(&router, &cookie, &tenant_ulid, &store_ulid, "Till 1").await;
    let counter = propose_printer(&router, &token, &store_ulid, "Counter", "/dev/usb/lp0").await;
    let approved = router
        .clone()
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{counter}/approve?tenant_id={tenant_ulid}&connection=usb"
            ),
            &serde_json::json!({}) as &serde_json::Value,
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approved.status(), StatusCode::NO_CONTENT);

    let version = approved_device_version(&router, &token, &store_ulid, &counter).await;
    let picked = router
        .clone()
        .oneshot(post_config_with_etag(
            &format!("/admin/devices/proposals/{counter}/agent"),
            &serde_json::json!({ "tenant_id": tenant_ulid.clone(), "agent_device_id": till.clone() }),
            &cookie,
            &version,
        ))
        .await
        .expect("route the agent pick");
    assert_eq!(picked.status(), StatusCode::NO_CONTENT);

    let published = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/devices/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid.clone(), "store_id": store_ulid.clone() }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);

    let effective = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the config read");
    let document = json_body(effective).await;
    let listed = document["devices"]["devices"]
        .as_array()
        .expect("the devices node is an array");
    let printer = listed
        .iter()
        .find(|device| device["device_id"] == counter)
        .expect("the printer is published");
    assert_eq!(
        printer["agent_device_id"], till,
        "the printer names the terminal that holds its transport"
    );
    let terminal = listed
        .iter()
        .find(|device| device["device_id"] == till)
        .expect("the terminal is published beside it");
    assert!(
        terminal["agent_device_id"].is_null(),
        "a terminal has no agent of its own"
    );
}

/// A publish refuses a node whose agent does not resolve to a terminal **in that node**
/// (**ADR-0112**, the console's half of the two gates).
///
/// The pick itself cannot decide this: the terminal may be created after it, or archived before the
/// publish. Only the compile sees the whole set at once, which is why the check lives there — and
/// refusing there is what makes "a paired phone can never be a print agent" true at the store, since
/// an unresolvable reference never leaves the cloud.
#[tokio::test]
async fn a_publish_refuses_an_agent_that_is_not_a_terminal_in_the_node() {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    // Two printers, and one is pointed at the other. Both are real, approved devices — which is the
    // point: existing is not the same as being a terminal.
    let counter = propose_printer(&router, &token, &store_ulid, "Counter", "/dev/usb/lp0").await;
    let oven = propose_printer(&router, &token, &store_ulid, "Oven", "192.168.1.52:9100").await;
    for id in [&counter, &oven] {
        let approved = router
            .clone()
            .oneshot(post_with_cookie(
                &format!(
                    "/admin/devices/proposals/{id}/approve?tenant_id={tenant_ulid}&connection=usb"
                ),
                &serde_json::json!({}) as &serde_json::Value,
                &cookie,
            ))
            .await
            .expect("route the approve");
        assert_eq!(approved.status(), StatusCode::NO_CONTENT);
    }
    let version = approved_device_version(&router, &token, &store_ulid, &counter).await;
    let picked = router
        .clone()
        .oneshot(post_config_with_etag(
            &format!("/admin/devices/proposals/{counter}/agent"),
            &serde_json::json!({ "tenant_id": tenant_ulid.clone(), "agent_device_id": oven.clone() }),
            &cookie,
            &version,
        ))
        .await
        .expect("route the agent pick");
    assert_eq!(
        picked.status(),
        StatusCode::NO_CONTENT,
        "the pick is stored; resolvability is the publish's question, not this route's"
    );

    let published = router
        .oneshot(post_with_cookie(
            "/admin/devices/publish",
            &serde_json::json!({ "tenant_id": tenant_ulid.clone(), "store_id": store_ulid.clone() }),
            &cookie,
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(published).await;
    assert_eq!(body["error"]["status"], "UNPROCESSABLE");
    let details = body["error"]["details"]
        .as_array()
        .expect("the refusal names what to fix");
    assert_eq!(details[0]["field"], "agent_device_id");
    assert_eq!(
        details[0]["reason"], oven,
        "which reference did not resolve"
    );
    assert_eq!(details[1]["field"], "device_id");
    assert_eq!(details[1]["reason"], counter, "and which device names it");
}

/// Picking an agent is a conditional write (**ADR-0094**, **ADR-0112**).
///
/// Two managers picking different agents for one printer is the ordinary race here, and
/// last-write-wins would leave the loser believing a decision that is not in the database.
#[tokio::test]
async fn picking_an_agent_needs_the_version_the_device_was_read_at() {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let till = create_terminal(&router, &cookie, &tenant_ulid, &store_ulid, "Till 1").await;
    let counter = propose_printer(&router, &token, &store_ulid, "Counter", "/dev/usb/lp0").await;
    let approved = router
        .clone()
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{counter}/approve?tenant_id={tenant_ulid}&connection=usb"
            ),
            &serde_json::json!({}) as &serde_json::Value,
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approved.status(), StatusCode::NO_CONTENT);
    let body = serde_json::json!({
        "tenant_id": tenant_ulid.clone(),
        "agent_device_id": till.clone(),
    });

    // No `If-Match` at all is an ordinary missing-field refusal, not a silent last-write-wins.
    let unconditional = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/devices/proposals/{counter}/agent"),
            &body,
            &cookie,
        ))
        .await
        .expect("route the unconditional pick");
    assert_eq!(unconditional.status(), StatusCode::BAD_REQUEST);
    let refusal = json_body(unconditional).await;
    assert_eq!(refusal["error"]["details"][0]["field"], "if-match");

    // The first pick succeeds and moves the row on; the same token replayed is now stale.
    let version = approved_device_version(&router, &token, &store_ulid, &counter).await;
    let first = router
        .clone()
        .oneshot(post_config_with_etag(
            &format!("/admin/devices/proposals/{counter}/agent"),
            &body,
            &cookie,
            &version,
        ))
        .await
        .expect("route the first pick");
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    let replayed = router
        .oneshot(post_config_with_etag(
            &format!("/admin/devices/proposals/{counter}/agent"),
            &serde_json::json!({ "tenant_id": tenant_ulid.clone(), "agent_device_id": null }),
            &cookie,
            &version,
        ))
        .await
        .expect("route the stale pick");
    assert_eq!(
        replayed.status(),
        StatusCode::PRECONDITION_FAILED,
        "the second manager is told to re-read rather than silently overwriting the first"
    );
}

/// A printer is marked **Cash drawer attached**, and the published node carries the mark
/// (**ADR-0165** decision 4), under the agent pick's conditional write.
///
/// End to end for the reason the agent test is: the mark alone proves nothing about what a store
/// receives. And the default is asserted first, because it is the claim a fleet upgrade rests on: a
/// printer nobody marked publishes no drawer.
#[tokio::test]
async fn a_printer_marked_with_a_drawer_publishes_the_mark_and_the_mark_needs_its_version() {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let counter = propose_printer(&router, &token, &store_ulid, "Counter", "/dev/usb/lp0").await;
    let approved = router
        .clone()
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{counter}/approve?tenant_id={tenant_ulid}&connection=usb"
            ),
            &serde_json::json!({}) as &serde_json::Value,
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approved.status(), StatusCode::NO_CONTENT);

    let unmarked = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &counter).await;
    assert!(
        unmarked["drawer_attached"].is_null(),
        "a printer nobody marked publishes no drawer, not even a false one: {unmarked}"
    );

    let body = serde_json::json!({ "tenant_id": tenant_ulid.clone(), "drawer_attached": true });
    let unconditional = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/devices/proposals/{counter}/drawer"),
            &body,
            &cookie,
        ))
        .await
        .expect("route the unconditional mark");
    assert_eq!(
        unconditional.status(),
        StatusCode::BAD_REQUEST,
        "no If-Match is refused, not a silent last-write-wins"
    );

    let version = approved_device_version(&router, &token, &store_ulid, &counter).await;
    let marked = router
        .clone()
        .oneshot(post_config_with_etag(
            &format!("/admin/devices/proposals/{counter}/drawer"),
            &body,
            &cookie,
            &version,
        ))
        .await
        .expect("route the mark");
    assert_eq!(marked.status(), StatusCode::NO_CONTENT);
    let replayed = router
        .clone()
        .oneshot(post_config_with_etag(
            &format!("/admin/devices/proposals/{counter}/drawer"),
            &serde_json::json!({ "tenant_id": tenant_ulid.clone(), "drawer_attached": false }),
            &cookie,
            &version,
        ))
        .await
        .expect("route the stale clear");
    assert_eq!(
        replayed.status(),
        StatusCode::PRECONDITION_FAILED,
        "a second manager holding the old version is told to re-read"
    );

    let published = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &counter).await;
    assert_eq!(
        published["drawer_attached"], true,
        "the store hears that a drawer is wired to its counter printer"
    );
}

/// A printer is set to 58 mm paper with no cutter, and the published node carries both
/// (**ADR-0160** decision 2), under the drawer mark's conditional write.
///
/// The default is asserted first, because a fleet upgrade rests on it: a printer nobody set
/// publishes neither field, so its store prints as it always has. And a paper this release does not
/// know is refused by name before anything is written.
#[tokio::test]
async fn a_printers_paper_is_validated_by_name_and_published_as_set() {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let counter = propose_printer(&router, &token, &store_ulid, "Counter", "192.0.2.10:9100").await;
    let approved = router
        .clone()
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{counter}/approve?tenant_id={tenant_ulid}&connection=network"
            ),
            &serde_json::json!({}) as &serde_json::Value,
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approved.status(), StatusCode::NO_CONTENT);

    let unset = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &counter).await;
    assert!(
        unset["paper_width"].is_null() && unset["cuts_paper"].is_null(),
        "a printer nobody set publishes neither field: {unset}"
    );

    let path = format!("/admin/devices/proposals/{counter}/paper");
    let version = approved_device_version(&router, &token, &store_ulid, &counter).await;
    for refused in [
        "PAPER_WIDTH_MILLIMETRES_112",
        "PAPER_WIDTH_UNSPECIFIED",
        "58",
    ] {
        let response = router
            .clone()
            .oneshot(post_config_with_etag(
                &path,
                &serde_json::json!({
                    "tenant_id": tenant_ulid.clone(),
                    "paper_width": refused,
                    "cuts_paper": true,
                }),
                &cookie,
                &version,
            ))
            .await
            .expect("route the refused paper");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{refused}");
        let body = json_body(response).await;
        assert_eq!(
            body["error"]["details"][0]["field"], "paper_width",
            "{body}"
        );
        assert_eq!(
            body["error"]["details"][0]["reason"], "INVALID_ENUM_VALUE",
            "{body}"
        );
    }

    let body = serde_json::json!({
        "tenant_id": tenant_ulid.clone(),
        "paper_width": "PAPER_WIDTH_MILLIMETRES_58",
        "cuts_paper": false,
    });
    let unconditional = router
        .clone()
        .oneshot(post_with_cookie(&path, &body, &cookie))
        .await
        .expect("route the unconditional write");
    assert_eq!(
        unconditional.status(),
        StatusCode::BAD_REQUEST,
        "no If-Match is refused, not a silent last-write-wins"
    );
    let saved = router
        .clone()
        .oneshot(post_config_with_etag(&path, &body, &cookie, &version))
        .await
        .expect("route the paper");
    assert_eq!(saved.status(), StatusCode::NO_CONTENT);
    let replayed = router
        .clone()
        .oneshot(post_config_with_etag(&path, &body, &cookie, &version))
        .await
        .expect("route the stale write");
    assert_eq!(
        replayed.status(),
        StatusCode::PRECONDITION_FAILED,
        "a second manager holding the old version is told to re-read"
    );

    let published = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &counter).await;
    assert_eq!(
        (&published["paper_width"], &published["cuts_paper"]),
        (
            &serde_json::json!("PAPER_WIDTH_MILLIMETRES_58"),
            &serde_json::json!(false)
        ),
        "the store hears its counter printer takes 58 mm paper and has no cutter: {published}"
    );
}
