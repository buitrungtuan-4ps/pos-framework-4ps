use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::well_known_cell_router;
use crate::cell::Cell;
use crate::cell::directory::tests::vn1;
use crate::cell::signed::{CellDirectory, parse_public_key};
use crate::cell::testkit::{V3, public_key_line};

fn signed() -> CellDirectory {
    let directory = CellDirectory::trusting(
        vn1(),
        parse_public_key(public_key_line()).expect("the key parses"),
    );
    assert!(directory.offer(V3.document, V3.minisig).is_ok());
    directory
}

fn missing() -> CellDirectory {
    CellDirectory::trusting(
        vn1(),
        parse_public_key(public_key_line()).expect("the key parses"),
    )
}

async fn ask(directory: CellDirectory, uri: &str) -> (StatusCode, Option<String>, Value) {
    let response = well_known_cell_router(Arc::new(directory))
        .oneshot(Request::get(uri).body(Body::empty()).expect("a request"))
        .await
        .expect("the router answers");
    let status = response.status();
    let cache = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (
        status,
        cache,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn detail_reason(body: &Value) -> Option<&str> {
    body["error"]["details"][0]["reason"].as_str()
}

#[tokio::test]
async fn it_names_this_cell_with_no_tenant_field() {
    let (status, cache, body) = ask(signed(), "/.well-known/pos-cell").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("public, max-age=60"));
    assert_eq!(
        body,
        json!({
            "cell_id": "vn1",
            "base_url": "https://vn1.example.com",
            "country_alias": "vn",
            "home_country": "VN",
            "directory_version": 3,
        })
    );
}

#[tokio::test]
async fn a_tenant_homed_elsewhere_is_named_with_its_cell() {
    let (status, _, body) = ask(signed(), "/.well-known/pos-cell?tenant=ramen-osaka").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["tenant"],
        json!({
            "tenant_slug": "ramen-osaka",
            "cell_id": "jp1",
            "base_url": "https://jp1.example.com",
            "here": false,
        })
    );
}

#[tokio::test]
async fn a_tenant_homed_here_says_so() {
    let (status, _, body) = ask(signed(), "/.well-known/pos-cell?tenant=pho-hanoi").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tenant"]["cell_id"], "vn1");
    assert_eq!(body["tenant"]["here"], true);
}

#[tokio::test]
async fn an_unlisted_tenant_is_not_found() {
    let (status, cache, body) = ask(signed(), "/.well-known/pos-cell?tenant=nobody").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(cache, None, "a refusal is not cached");
    assert_eq!(detail_reason(&body), Some("NOT_LISTED"));
}

#[tokio::test]
async fn a_malformed_slug_is_refused_before_any_lookup() {
    for uri in [
        "/.well-known/pos-cell?tenant=Pho",
        "/.well-known/pos-cell?tenant=",
        "/.well-known/pos-cell?tenant=-pho",
    ] {
        let (status, _, body) = ask(missing(), uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(detail_reason(&body), Some("MALFORMED"), "{uri}");
    }
}

#[tokio::test]
async fn a_cell_waiting_for_its_directory_names_no_tenant() {
    let (status, _, _) = ask(missing(), "/.well-known/pos-cell?tenant=pho-hanoi").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _, body) = ask(missing(), "/.well-known/pos-cell").await;
    assert_eq!(status, StatusCode::OK, "it still names itself");
    assert_eq!(body["directory_version"], Value::Null);
}

#[tokio::test]
async fn the_single_default_cell_is_every_tenants_home() {
    let (status, _, body) = ask(
        CellDirectory::single(Cell::single()),
        "/.well-known/pos-cell?tenant=anyone",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cell_id"], "default");
    assert_eq!(
        body["tenant"],
        json!({
            "tenant_slug": "anyone",
            "cell_id": "default",
            "base_url": null,
            "here": true,
        })
    );
}
