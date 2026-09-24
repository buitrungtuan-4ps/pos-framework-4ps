// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge gzips what it sends, when asked and when it is worth it
//! ([ADR-0138](../../../docs/adr/0138-the-edge-compresses-what-it-sends.md)).
//!
//! Drives [`pos_edge::http::compress`] itself rather than a layer built here, so the suite proves the
//! shipped policy — the size floor, the negotiation — and not merely that `tower-http` works.

use std::io::Read as _;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A menu-sized JSON answer: repetitive, like the real one, and well over the floor.
fn large_json() -> String {
    let items: Vec<String> = (0..200)
        .map(|n| {
            format!(r#"{{"menu_item_id":"{n:026}","display_name":"Pizza {n}","available":true}}"#)
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn app() -> Router {
    pos_edge::http::compress(
        Router::new()
            .route(
                "/api/menu",
                get(|| async { ([(header::CONTENT_TYPE, "application/json")], large_json()) }),
            )
            .route(
                "/api/small",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        "{\"ok\":true}",
                    )
                }),
            ),
    )
}

async fn get_with(path: &str, accept: Option<&str>) -> (StatusCode, Option<String>, Vec<u8>) {
    let mut request = Request::builder().uri(path);
    if let Some(accept) = accept {
        request = request.header(header::ACCEPT_ENCODING, accept);
    }
    let response = app()
        .oneshot(request.body(Body::empty()).expect("request builds"))
        .await
        .expect("router responds");
    let status = response.status();
    let encoding = response
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, encoding, bytes)
}

#[tokio::test]
async fn a_large_answer_is_gzipped_for_a_client_that_asks_and_decodes_to_the_same_bytes() {
    let (status, encoding, bytes) = get_with("/api/menu", Some("gzip, deflate, br")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(encoding.as_deref(), Some("gzip"));

    let mut decoded = String::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_string(&mut decoded)
        .expect("a valid gzip stream");
    assert_eq!(
        decoded,
        large_json(),
        "compression must not change what the till reads"
    );
    assert!(
        bytes.len() * 4 < decoded.len(),
        "a menu-shaped answer shrinks by far more than a quarter ({} → {} bytes)",
        decoded.len(),
        bytes.len()
    );
}

#[tokio::test]
async fn a_client_that_does_not_ask_gets_exactly_what_it_got_before() {
    let (status, encoding, bytes) = get_with("/api/menu", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(encoding, None);
    assert_eq!(bytes, large_json().into_bytes());
}

#[tokio::test]
async fn a_body_under_the_floor_is_not_worth_a_gzip_header() {
    let (status, encoding, bytes) = get_with("/api/small", Some("gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(encoding, None);
    assert_eq!(bytes, b"{\"ok\":true}");
}
