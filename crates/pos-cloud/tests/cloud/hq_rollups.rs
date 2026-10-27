// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/hq/rollups`: the daily rollups of the stores an HQ reads in other cells, aggregates
//! only, and never a key (ADR-0179 C10).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::cell::Cell;
use pos_cloud::cell::federation::{
    FederationSection, FederationSource, RollupFetcher, RollupWindow, SourceKey,
};
use pos_cloud::cell::hq::hq_rollups_router;
use pos_cloud::cell::signed::{CellDirectory, parse_public_key};
use pos_cloud_reports::types::DailyRollup;
use pos_proto::locale::CountryCode;
use tower::ServiceExt;

use crate::{admin_cookie, app_all, clock, get, get_with_cookie, json_body, provisioned_admin};

const KEY: &str = "pos_k1_placeholder";
const STORE: &str = "01M2219QK4T3W6Z0Y8FBQ2X5MV";

/// `vn1`, holding the committed two-cell fixture directory, which lists `jp1`.
fn directory() -> Arc<CellDirectory> {
    let public = include_str!("../../src/cell/fixtures/directory.pub")
        .lines()
        .nth(1)
        .unwrap_or_default();
    let cell = Cell {
        cell_id: "vn1".to_owned(),
        country_alias: Some("vn".to_owned()),
        home_country: Some(CountryCode::VN),
        base_url: Some("https://vn1.example.com".to_owned()),
        runs_in: Some(CountryCode::VN),
        configured: true,
    };
    let directory = CellDirectory::trusting(cell, parse_public_key(public).expect("the key"));
    directory
        .offer(
            include_bytes!("../../src/cell/fixtures/directory-v3.json"),
            include_bytes!("../../src/cell/fixtures/directory-v3.json.minisig"),
        )
        .expect("the fixture verifies");
    Arc::new(directory)
}

/// Answers every store with two days, without a network.
#[derive(Clone, Default)]
struct TwoDays;

impl RollupFetcher for TwoDays {
    async fn fetch(
        &self,
        base_url: &str,
        _store_id: &str,
        _key: &SourceKey,
        window: &RollupWindow,
    ) -> Result<Vec<DailyRollup>, String> {
        assert_eq!(base_url, "https://jp1.example.com");
        assert_eq!(window.from.as_deref(), Some("2026-10-26"));
        Ok(["2026-10-26", "2026-10-27"]
            .into_iter()
            .map(|date| DailyRollup {
                business_date: date.to_owned(),
                total_events: 5,
                by_type: BTreeMap::from([("sales.order.created".to_owned(), 5)]),
            })
            .collect())
    }
}

fn hq_app(federation: Option<FederationSection>) -> axum::Router {
    let admin = provisioned_admin();
    let app = app_all(
        pos_cloud::Cloud::new(pos_fakes::FakeStore::new()),
        crate::FakeRollups::default(),
        crate::FakeKeys::default(),
        admin.clone(),
        crate::FakeConfigTrees::default(),
        crate::FakeWebhooks::default(),
    );
    pos_cloud::http::router(app).merge(hq_rollups_router(
        directory(),
        federation,
        TwoDays,
        admin,
        clock(),
    ))
}

fn jp1() -> FederationSection {
    FederationSection {
        sources: vec![FederationSource {
            cell_id: "jp1".to_owned(),
            api_key: SourceKey::new(KEY),
            stores: vec![STORE.to_owned()],
        }],
    }
}

#[tokio::test]
async fn an_hq_reads_aggregates_from_another_cell_and_never_shows_a_key() {
    let router = hq_app(Some(jp1()));
    let cookie = admin_cookie(&router).await;
    let response = router
        .oneshot(get_with_cookie(
            "/admin/hq/rollups?from=2026-10-26",
            &cookie,
        ))
        .await
        .expect("route");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["configured"], true);
    assert_eq!(body["stores"][0]["cell_id"], "jp1");
    assert_eq!(body["stores"][0]["store_id"], STORE);
    assert_eq!(body["stores"][0]["rollups"][1]["total_events"], 5);
    assert_eq!(body["stores"][0]["error"], serde_json::Value::Null);
    assert!(!body.to_string().contains(KEY), "a key is never answered");
}

#[tokio::test]
async fn no_federation_is_an_empty_answer() {
    let router = hq_app(None);
    let cookie = admin_cookie(&router).await;
    let body = json_body(
        router
            .oneshot(get_with_cookie("/admin/hq/rollups", &cookie))
            .await
            .expect("route"),
    )
    .await;
    assert_eq!(
        body,
        serde_json::json!({ "configured": false, "stores": [] })
    );
}

#[tokio::test]
async fn a_malformed_date_is_refused_and_a_session_is_required() {
    let router = hq_app(Some(jp1()));
    let cookie = admin_cookie(&router).await;
    let refused = router
        .clone()
        .oneshot(get_with_cookie("/admin/hq/rollups?to=27-10-2026", &cookie))
        .await
        .expect("route");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(refused).await["error"]["details"][0]["field"],
        "to"
    );
    let anonymous = router
        .oneshot(get("/admin/hq/rollups", None))
        .await
        .expect("route");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}
