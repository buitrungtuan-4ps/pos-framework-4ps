use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::routing::post;
use tower::ServiceExt as _;

use super::{decide, front_door};
use crate::cell::Cell;
use crate::cell::directory::tests::vn1;
use crate::cell::signed::{CellDirectory, parse_public_key};
use crate::cell::testkit::{V3, public_key_line};

/// `vn1`, holding the two-cell fixture directory: `pho-hanoi` is homed here, `ramen-osaka` in `jp1`.
fn signed() -> CellDirectory {
    let directory = CellDirectory::trusting(
        vn1(),
        parse_public_key(public_key_line()).expect("the key parses"),
    );
    assert!(directory.offer(V3.document, V3.minisig).is_ok());
    directory
}

fn browser() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("text/html,application/xhtml+xml"),
    );
    headers
}

#[test]
fn a_browser_asking_for_a_tenant_homed_elsewhere_gets_a_302() {
    let redirect = decide(
        &signed(),
        Some("ramen-osaka.example.com"),
        &Method::GET,
        &browser(),
        "/menu?table=4",
    )
    .expect("a redirect");
    assert_eq!(redirect.status, StatusCode::FOUND);
    assert_eq!(redirect.location, "https://jp1.example.com/menu?table=4");
    assert_eq!(redirect.cell_id, "jp1");
}

#[test]
fn an_api_call_gets_a_307_that_keeps_its_method() {
    for (method, headers) in [
        (Method::POST, browser()),
        (Method::GET, HeaderMap::new()),
        (Method::PUT, HeaderMap::new()),
    ] {
        let redirect = decide(
            &signed(),
            Some("Ramen-Osaka.example.com:443"),
            &method,
            &headers,
            "/v1/orders",
        )
        .expect("a redirect");
        assert_eq!(redirect.status, StatusCode::TEMPORARY_REDIRECT, "{method}");
        assert_eq!(redirect.location, "https://jp1.example.com/v1/orders");
    }
}

#[test]
fn everything_else_passes_through() {
    let directory = signed();
    for host in [
        None,
        Some("vn1.example.com"),
        Some("vn1.example.com:8443"),
        Some("pho-hanoi.example.com"),
        Some("unknown.example.com"),
        Some("localhost"),
        Some("ramen-osaka"),
        Some("ramen-osaka:8080"),
        Some("Not_A_Slug.example.com"),
        Some("203.0.113.9"),
    ] {
        assert_eq!(
            decide(&directory, host, &Method::GET, &browser(), "/"),
            None,
            "{host:?}"
        );
    }
}

#[test]
fn a_single_cell_never_redirects() {
    assert_eq!(
        decide(
            &CellDirectory::single(Cell::single()),
            Some("ramen-osaka.example.com"),
            &Method::GET,
            &browser(),
            "/",
        ),
        None
    );
}

#[test]
fn a_cell_waiting_for_its_directory_never_redirects() {
    let waiting = CellDirectory::trusting(
        vn1(),
        parse_public_key(public_key_line()).expect("the key parses"),
    );
    assert_eq!(
        decide(
            &waiting,
            Some("ramen-osaka.example.com"),
            &Method::GET,
            &browser(),
            "/"
        ),
        None
    );
}

async fn through_the_layer(
    host: &str,
    method: Method,
    accept: Option<&str>,
) -> axum::response::Response {
    let router = Router::new()
        .route("/v1/orders", post(|| async { "served here" }))
        .fallback(|| async { "the console" })
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(signed()),
            front_door,
        ));
    let mut request = Request::builder()
        .method(method)
        .uri("/v1/orders")
        .header(header::HOST, host);
    if let Some(accept) = accept {
        request = request.header(header::ACCEPT, accept);
    }
    router
        .oneshot(request.body(Body::empty()).expect("a request"))
        .await
        .expect("the router answers")
}

#[tokio::test]
async fn the_layer_redirects_with_a_host_only_cookie_and_never_a_301() {
    let response = through_the_layer("ramen-osaka.example.com", Method::POST, None).await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    let headers = response.headers();
    assert_eq!(
        headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("https://jp1.example.com/v1/orders")
    );
    let cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("a cookie");
    assert!(cookie.starts_with("cell_id=jp1;"), "{cookie}");
    assert!(
        !cookie.to_ascii_lowercase().contains("domain="),
        "host-only: never set on the parent domain: {cookie}"
    );
    assert!(cookie.contains("Secure") && cookie.contains("SameSite=Lax"));
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );

    let browser =
        through_the_layer("ramen-osaka.example.com", Method::GET, Some("text/html")).await;
    assert_eq!(
        browser.status(),
        StatusCode::FOUND,
        "the SPA fallback is covered too"
    );
}

#[tokio::test]
async fn the_layer_passes_this_cells_own_requests_on() {
    let response = through_the_layer("vn1.example.com", Method::POST, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = through_the_layer("pho-hanoi.example.com", Method::POST, None).await;
    assert_eq!(response.status(), StatusCode::OK);
}
