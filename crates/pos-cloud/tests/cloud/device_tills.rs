// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A till's receipt printer, receipt languages and float, and the X/Z report's drawer names.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditSink, AuditStore, NoopAuditRecorder};
use pos_cloud::auth::apikey::Scope;
use pos_cloud::cloud::{CashTotals, DailyCash, STORE_DRAWER};
use pos_cloud::dashboard::{RollupStore, StoredRollups};
use pos_proto::ids::{DeviceId, StationId, StoreId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeConfigTrees, FakeKeys, FakeRollups, admin_cookie, approved_device_version,
    body_bytes, create_terminal, device_publish_app, device_publish_app_with, get_with_cookie,
    issue_store_key, json_body, post_config_with_etag, post_with_cookie, propose_printer,
    published_device, store_id, tenant,
};

/// Approves the proposal `id` as a network printer serving `station`, or the counter with `None`.
async fn approve_on_network(
    router: &axum::Router,
    cookie: &str,
    tenant_ulid: &str,
    id: &str,
    station: Option<&str>,
) {
    let station = station.map_or_else(String::new, |station| format!("&station_id={station}"));
    let approved = router
        .clone()
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{id}/approve?tenant_id={tenant_ulid}&connection=network{station}"
            ),
            &serde_json::json!({}) as &serde_json::Value,
            cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approved.status(), StatusCode::NO_CONTENT);
}

/// Says where the terminal `id`'s receipts go, at `version`, and answers the status and the body,
/// `null` for a `204`.
async fn post_till_receipt(
    router: &axum::Router,
    cookie: &str,
    id: &str,
    version: &str,
    body: &serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(post_config_with_etag(
            &format!("/admin/devices/proposals/{id}/receipt"),
            body,
            cookie,
            version,
        ))
        .await
        .expect("route the till's receipts");
    let status = response.status();
    let body = body_bytes(response).await;
    (
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    )
}

/// A store's devices for the till tests: its counter's printer, a station's (`oven`), one still
/// pending, another store's approved counter printer (`theirs`), and two terminals, `bar` and
/// `door`, each by id.
struct TillStore {
    router: axum::Router,
    cookie: String,
    token: String,
    tenant_ulid: String,
    store_ulid: String,
    counter: String,
    oven: String,
    pending: String,
    theirs: String,
    bar: String,
    door: String,
}

async fn till_store() -> TillStore {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let counter = propose_printer(&router, &token, &store_ulid, "Counter", "192.0.2.10:9100").await;
    approve_on_network(&router, &cookie, &tenant_ulid, &counter, None).await;
    let oven = propose_printer(&router, &token, &store_ulid, "Oven", "192.0.2.11:9100").await;
    let station = StationId::new(Ulid::from_u128(0x5A)).to_string();
    approve_on_network(&router, &cookie, &tenant_ulid, &oven, Some(&station)).await;
    let pending = propose_printer(&router, &token, &store_ulid, "Spare", "192.0.2.12:9100").await;
    let elsewhere = StoreId::new(Ulid::from_u128(0xB0FF));
    let neighbour = issue_store_key(&keys, tenant(), elsewhere, &[Scope::ManageDevices]);
    let theirs = propose_printer(
        &router,
        &neighbour,
        &elsewhere.as_ulid().to_string(),
        "Their counter",
        "192.0.2.13:9100",
    )
    .await;
    approve_on_network(&router, &cookie, &tenant_ulid, &theirs, None).await;
    let bar = create_terminal(&router, &cookie, &tenant_ulid, &store_ulid, "Bar till").await;
    let door = create_terminal(&router, &cookie, &tenant_ulid, &store_ulid, "Door till").await;
    TillStore {
        router,
        cookie,
        token,
        tenant_ulid,
        store_ulid,
        counter,
        oven,
        pending,
        theirs,
        bar,
        door,
    }
}

/// Each way a till's receipts can be said wrongly is refused `400` naming the field
/// (**ADR-0160** decision 4), and a device the tenant has not approved is a `404`.
///
/// The printer has to be one the till's own store prints a bill on: approved, a printer, serving no
/// station, in the same store. Each refusal leaves the terminal as it was, so the store goes on
/// printing that till's receipts as it does.
#[tokio::test]
async fn a_tills_receipts_are_refused_by_name_unless_they_name_its_stores_receipt_printer() {
    let TillStore {
        router,
        cookie,
        token,
        tenant_ulid,
        store_ulid,
        counter,
        oven,
        pending,
        theirs,
        bar,
        door,
    } = till_store().await;
    let version = approved_device_version(&router, &token, &store_ulid, &bar).await;

    let body = |printer: &str, language: &str, second: &str| {
        serde_json::json!({
            "tenant_id": tenant_ulid.clone(),
            "receipt_printer_id": printer,
            "receipt_language": language,
            "receipt_second_language": second,
        })
    };
    let (en, none) = ("RECEIPT_LANGUAGE_EN", "RECEIPT_SECOND_LANGUAGE_NONE");
    let refused_as = |refused: &serde_json::Value, field: &str, reason: &str| {
        assert_eq!(refused["error"]["details"][0]["field"], field, "{refused}");
        assert_eq!(
            refused["error"]["details"][0]["reason"], reason,
            "{refused}"
        );
    };

    let at = approved_device_version(&router, &token, &store_ulid, &counter).await;
    let (status, refused) =
        post_till_receipt(&router, &cookie, &counter, &at, &body(&counter, en, none)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a printer is not a till");
    refused_as(&refused, "id", "WRONG_KIND");

    for (printer, reason) in [
        ("not-a-ulid", "NOT_A_ULID"),
        (pending.as_str(), "UNKNOWN_REFERENCE"),
        (theirs.as_str(), "UNKNOWN_REFERENCE"),
        (oven.as_str(), "WRONG_KIND"),
        (door.as_str(), "WRONG_KIND"),
    ] {
        let sent = body(printer, en, none);
        let (status, refused) = post_till_receipt(&router, &cookie, &bar, &version, &sent).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{sent}");
        refused_as(&refused, "receipt_printer_id", reason);
    }

    for (language, second, field) in [
        ("RECEIPT_LANGUAGE_JA", none, "receipt_language"),
        ("RECEIPT_LANGUAGE_UNSPECIFIED", none, "receipt_language"),
        (en, "RECEIPT_LANGUAGE_EN", "receipt_second_language"),
        (
            en,
            "RECEIPT_SECOND_LANGUAGE_UNSPECIFIED",
            "receipt_second_language",
        ),
    ] {
        let sent = body(&counter, language, second);
        let (status, refused) = post_till_receipt(&router, &cookie, &bar, &version, &sent).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{sent}");
        refused_as(&refused, field, "INVALID_ENUM_VALUE");
    }

    let unknown = DeviceId::new(Ulid::from_u128(0xDEAD)).to_string();
    let (status, _) = post_till_receipt(
        &router,
        &cookie,
        &unknown,
        &version,
        &body(&counter, en, none),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a device the tenant has not approved"
    );
    let (status, _) = post_till_receipt(
        &router,
        &cookie,
        &pending,
        &version,
        &body(&counter, en, none),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a pending proposal is not a device yet"
    );

    let published = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;
    for field in [
        "receipt_printer_id",
        "receipt_language",
        "receipt_second_language",
    ] {
        assert!(
            published[field].is_null(),
            "nothing refused was written: {published}"
        );
    }
}

/// A till's receipt printer and languages are saved under the paper's conditional write, published
/// as set, and cleared back to the store's (**ADR-0160** decision 4).
///
/// The default is asserted first, because a fleet upgrade rests on it: a terminal nobody set
/// publishes none of the three fields, so every till prints as the store does.
#[tokio::test]
async fn a_tills_receipt_printer_and_languages_are_published_as_set_and_cleared_to_the_stores() {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let fields = [
        "receipt_printer_id",
        "receipt_language",
        "receipt_second_language",
    ];

    let counter = propose_printer(&router, &token, &store_ulid, "Counter", "192.0.2.10:9100").await;
    approve_on_network(&router, &cookie, &tenant_ulid, &counter, None).await;
    let bar_printer = propose_printer(&router, &token, &store_ulid, "Bar", "192.0.2.14:9100").await;
    approve_on_network(&router, &cookie, &tenant_ulid, &bar_printer, None).await;
    let bar = create_terminal(&router, &cookie, &tenant_ulid, &store_ulid, "Bar till").await;

    let unset = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;
    assert!(
        fields.iter().all(|field| unset[*field].is_null()),
        "a terminal nobody set publishes none of the fields: {unset}"
    );

    let version = approved_device_version(&router, &token, &store_ulid, &bar).await;
    let chosen = serde_json::json!({
        "tenant_id": tenant_ulid.clone(),
        "receipt_printer_id": bar_printer.clone(),
        "receipt_language": "RECEIPT_LANGUAGE_EN",
        "receipt_second_language": "RECEIPT_SECOND_LANGUAGE_NONE",
    });
    let unconditional = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/devices/proposals/{bar}/receipt"),
            &chosen,
            &cookie,
        ))
        .await
        .expect("route the unconditional write");
    assert_eq!(
        unconditional.status(),
        StatusCode::BAD_REQUEST,
        "no If-Match is refused, not a silent last-write-wins"
    );
    let (status, _) = post_till_receipt(&router, &cookie, &bar, &version, &chosen).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = post_till_receipt(&router, &cookie, &bar, &version, &chosen).await;
    assert_eq!(
        status,
        StatusCode::PRECONDITION_FAILED,
        "a second manager holding the old version is told to re-read"
    );

    let published = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;
    assert_eq!(
        fields.map(|field| published[field].clone()),
        [
            serde_json::json!(bar_printer),
            serde_json::json!("RECEIPT_LANGUAGE_EN"),
            serde_json::json!("RECEIPT_SECOND_LANGUAGE_NONE"),
        ],
        "the store hears the bar till prints at the bar, in English alone: {published}"
    );
    let printer = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &counter).await;
    assert!(
        fields.iter().all(|field| printer[*field].is_null()),
        "the store's receipt printer is unchanged: {printer}"
    );

    let version = approved_device_version(&router, &token, &store_ulid, &bar).await;
    let cleared = serde_json::json!({ "tenant_id": tenant_ulid.clone() });
    let (status, _) = post_till_receipt(&router, &cookie, &bar, &version, &cleared).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let cleared = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;
    assert!(
        fields.iter().all(|field| cleared[*field].is_null()),
        "fields left out are the store's again: {cleared}"
    );
}

/// Says what the terminal `id`'s drawer opens with, at `version`, and answers the status and the
/// body, `null` for a `204`.
async fn post_till_float(
    router: &axum::Router,
    cookie: &str,
    id: &str,
    version: &str,
    body: &serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(post_config_with_etag(
            &format!("/admin/devices/proposals/{id}/float"),
            body,
            cookie,
            version,
        ))
        .await
        .expect("route the till's float");
    let status = response.status();
    let body = body_bytes(response).await;
    (
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    )
}

/// A till's float outside `shift.opening_float_minor`'s bounds, on a device that is not a
/// terminal, or on one the tenant has not approved, is refused, and nothing is written
/// (**ADR-0167** decision 4). The bounds themselves are floats.
#[tokio::test]
async fn a_tills_float_is_refused_outside_the_settings_bounds_and_off_a_terminal() {
    let TillStore {
        router,
        cookie,
        token,
        tenant_ulid,
        store_ulid,
        counter,
        pending,
        bar,
        ..
    } = till_store().await;
    let body = |minor: i64| serde_json::json!({ "tenant_id": tenant_ulid.clone(), "opening_float_minor": minor });
    let refused_as = |refused: &serde_json::Value, field: &str, reason: &str| {
        assert_eq!(refused["error"]["details"][0]["field"], field, "{refused}");
        assert_eq!(
            refused["error"]["details"][0]["reason"], reason,
            "{refused}"
        );
    };
    let version = approved_device_version(&router, &token, &store_ulid, &bar).await;

    for minor in [-1, 1_000_000_001, i64::MAX] {
        let (status, refused) =
            post_till_float(&router, &cookie, &bar, &version, &body(minor)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{minor}");
        refused_as(&refused, "opening_float_minor", "OUT_OF_RANGE");
    }
    let at = approved_device_version(&router, &token, &store_ulid, &counter).await;
    let (status, refused) = post_till_float(&router, &cookie, &counter, &at, &body(100_000)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a printer keeps no drawer");
    refused_as(&refused, "id", "WRONG_KIND");
    let unknown = DeviceId::new(Ulid::from_u128(0xDEAD)).to_string();
    for id in [unknown.as_str(), pending.as_str()] {
        let (status, _) = post_till_float(&router, &cookie, id, &version, &body(100_000)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "not an approved device: {id}"
        );
    }
    let published = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;
    assert!(
        published.get("opening_float_minor").is_none(),
        "nothing refused was written: {published}"
    );

    for minor in [0, 1_000_000_000] {
        let version = approved_device_version(&router, &token, &store_ulid, &bar).await;
        let (status, _) = post_till_float(&router, &cookie, &bar, &version, &body(minor)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{minor}");
        let published = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;
        assert_eq!(published["opening_float_minor"], minor, "{published}");
    }
}

/// A till's float is saved under the receipts' conditional write, published as set, cleared back
/// to the store's, and each save is audited (**ADR-0167** decision 4).
///
/// The default is asserted first, because a fleet upgrade rests on it: a terminal nobody set
/// publishes no float at all, so every till's drawer opens on the store's.
#[tokio::test]
async fn a_tills_float_is_published_as_set_cleared_to_the_stores_and_audited() {
    let keys = FakeKeys::default();
    let audit = FakeAudit::default();
    let router = device_publish_app_with(
        &keys,
        FakeConfigTrees::default(),
        FakeRollups::default(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let bar = create_terminal(&router, &cookie, &tenant_ulid, &store_ulid, "Bar till").await;
    let door = create_terminal(&router, &cookie, &tenant_ulid, &store_ulid, "Door till").await;

    let unset = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;
    assert!(
        unset.get("opening_float_minor").is_none(),
        "a terminal nobody set publishes no float: {unset}"
    );

    let version = approved_device_version(&router, &token, &store_ulid, &bar).await;
    let float =
        serde_json::json!({ "tenant_id": tenant_ulid.clone(), "opening_float_minor": 250_000 });
    let (status, _) = post_till_float(&router, &cookie, &bar, &version, &float).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = post_till_float(&router, &cookie, &bar, &version, &float).await;
    assert_eq!(
        status,
        StatusCode::PRECONDITION_FAILED,
        "a second manager holding the old version is told to re-read"
    );
    let published = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;
    assert_eq!(published["opening_float_minor"], 250_000, "{published}");
    let other = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &door).await;
    assert!(
        other.get("opening_float_minor").is_none(),
        "only the till that was set carries a float: {other}"
    );

    let version = approved_device_version(&router, &token, &store_ulid, &bar).await;
    let cleared = serde_json::json!({ "tenant_id": tenant_ulid.clone() });
    let (status, _) = post_till_float(&router, &cookie, &bar, &version, &cleared).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let cleared = published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;
    assert!(
        cleared.get("opening_float_minor").is_none(),
        "a float left out is the store's again: {cleared}"
    );

    let recorded = audit.list(None, 50).await.expect("list audit entries");
    let floats: Vec<_> = recorded
        .iter()
        .rev()
        .filter(|entry| entry.action == "device_proposal.set_float")
        .map(|entry| (entry.entity_id.as_str(), entry.after.clone()))
        .collect();
    assert_eq!(
        floats,
        [
            (
                bar.as_str(),
                Some(serde_json::json!({ "opening_float_minor": 250_000 }))
            ),
            (
                bar.as_str(),
                Some(serde_json::json!({ "opening_float_minor": null }))
            ),
        ],
        "each saved float is recorded with what the till now says, and the refused one is not"
    );
}

/// The X/Z report names each till among the day's drawers from the store's published devices node
/// as it is served (**ADR-0167** decision 13): a till the node lists by the name it gives there, and
/// neither the store's drawer nor a till the node no longer lists, which the console labels itself.
#[tokio::test]
async fn the_xz_report_names_each_tills_drawer_from_the_published_devices_node() {
    let keys = FakeKeys::default();
    let rollups = FakeRollups::default();
    let router = device_publish_app_with(
        &keys,
        FakeConfigTrees::default(),
        rollups.clone(),
        Arc::new(NoopAuditRecorder),
    );
    let cookie = admin_cookie(&router).await;
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let bar = create_terminal(&router, &cookie, &tenant_ulid, &store_ulid, "Bar till").await;
    published_device(&router, &cookie, &tenant_ulid, &store_ulid, &bar).await;

    let gone = DeviceId::new(Ulid::from_u128(0x60E)).to_string();
    let paid_in = |paid_in| CashTotals {
        paid_in,
        ..CashTotals::default()
    };
    let mut state = StoredRollups::default();
    state.cash.insert(
        "2026-03-15".to_owned(),
        DailyCash {
            business_date: "2026-03-15".to_owned(),
            currency_code: "VND".to_owned(),
            paid_in: 60_000,
            by_drawer: BTreeMap::from([
                (bar.clone(), paid_in(10_000)),
                (gone.clone(), paid_in(20_000)),
                (STORE_DRAWER.to_owned(), paid_in(30_000)),
            ]),
            ..DailyCash::default()
        },
    );
    rollups
        .save(tenant(), store_id(), &state)
        .await
        .expect("seed the rollup");

    let response = router
        .oneshot(get_with_cookie(
            &format!(
                "/admin/stores/{store_ulid}/reports/xz?tenant_id={tenant_ulid}&business_date=2026-03-15"
            ),
            &cookie,
        ))
        .await
        .expect("route the report");
    assert_eq!(response.status(), StatusCode::OK);
    let report = json_body(response).await;
    assert_eq!(
        report["till_names"],
        serde_json::json!({ bar.as_str(): "Bar till" }),
        "{report}"
    );
    assert_eq!(
        report["cash"]["by_drawer"][gone.as_str()]["paid_in"],
        20_000,
        "a till no longer listed is still reported, by its id"
    );
    assert_eq!(
        report["cash"]["paid_in"], 60_000,
        "the store's figures sit where they always did"
    );
}
