// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Entity tags, and a `304` for a configuration read the till already holds
//! ([ADR-0173](../../../../docs/adr/0173-the-till-declares-each-piece-of-store-data-once.md)
//! decision 2).
//!
//! The till re-reads its configuration on every reconnect and every `config_applied`, and most of
//! those reads return what it already has. [`revalidate`] wraps a read route: it tags a `200`'s body
//! with a hash of it, and answers an `If-None-Match` that names that tag with a bodiless `304`. The
//! route's handler still runs, so its own checks and its answer are unchanged; what is saved is the
//! body on the wire and the till's work to apply it again. A client that sends no `If-None-Match`,
//! such as a till older than this, gets the `200` it always did, with two more headers.

use axum::body::{Body, HttpBody as _, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use sha2::{Digest as _, Sha256};

/// The largest body this hashes. A configuration read is a few kilobytes to a few hundred; a body
/// that could be larger, or does not say how large it is, passes through untagged rather than be
/// held in memory without a bound.
const MAX_BODY: u64 = 4 * 1024 * 1024;

/// Held, but revalidated on every use, and by this device alone: the reads are behind the device
/// token, so no shared cache may keep one.
const REVALIDATE: &str = "private, no-cache";

/// `route`, with every answer [`revalidate`] tags.
pub(crate) fn revalidated<S>(route: MethodRouter<S>) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    route.layer(from_fn(revalidate))
}

/// Tags a `200` from the wrapped route and answers a matching `If-None-Match` with a `304`.
pub(crate) async fn revalidate(request: Request, next: Next) -> Response {
    let offered = request.headers().get(header::IF_NONE_MATCH).cloned();
    let response = next.run(request).await;
    if response.status() != StatusCode::OK
        || response
            .body()
            .size_hint()
            .upper()
            .is_none_or(|upper| upper > MAX_BODY)
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    // The size hint said the body fits, so this cannot overrun; a body that broke its own hint
    // is an error the handler made, answered as one.
    let Ok(bytes) = to_bytes(body, usize::try_from(MAX_BODY).unwrap_or(usize::MAX)).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let etag = hex_etag(Sha256::digest(&bytes).into());
    let Ok(tag) = HeaderValue::from_str(&etag) else {
        // Unreachable: the tag is ASCII hex in quotes. Sent untagged rather than refused.
        return Response::from_parts(parts, Body::from(bytes));
    };
    let mut held = HeaderMap::new();
    if let Some(offered) = offered {
        held.insert(header::IF_NONE_MATCH, offered);
    }
    if none_match(&held, &etag) {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, tag),
                (header::CACHE_CONTROL, HeaderValue::from_static(REVALIDATE)),
            ],
        )
            .into_response();
    }
    parts.headers.insert(header::ETAG, tag);
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(REVALIDATE));
    Response::from_parts(parts, Body::from(bytes))
}

/// Whether the client's `If-None-Match` already holds the representation tagged `etag`.
///
/// A list, because a client may offer several, and `*` matches anything it has. Compared **weakly**
/// (RFC 9110 §13.1.2), as `If-None-Match` always is: `W/` is not part of the value. That is also
/// what lets a tablet still holding the strong tag an older edge sent revalidate against this one
/// instead of downloading the bundle again.
pub(crate) fn none_match(request: &HeaderMap, etag: &str) -> bool {
    let Some(header) = request
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let ours = opaque(etag);
    header
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || opaque(candidate) == ours)
}

/// An entity tag without its weakness marker.
fn opaque(etag: &str) -> &str {
    etag.strip_prefix("W/").unwrap_or(etag)
}

/// A SHA-256 as a **weak** `ETag`.
///
/// Weak because the hash is of the body, and the compression layer serves that body gzip or plain
/// under whatever tag this sets. A strong tag promises byte-identical representations (RFC 9110
/// §8.8.3), so one strong tag on both let a cache hand gzip bytes to a client that never asked
/// for them. Nothing here needs the strong kind: it is only for `If-Match` and `If-Range`, and
/// these routes serve no ranges.
pub(crate) fn hex_etag(hash: [u8; 32]) -> String {
    use std::fmt::Write as _;
    // Sixteen bytes of SHA-256 is far past any collision a build could produce, and keeps the
    // header short enough to read in a trace.
    let mut etag = String::with_capacity(36);
    etag.push_str("W/\"");
    for byte in &hash[..16] {
        // Infallible: writing to a `String` cannot fail.
        let _ = write!(etag, "{byte:02x}");
    }
    etag.push('"');
    etag
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt as _;

    use axum::routing::get;

    use super::revalidated;

    fn app(body: &'static str) -> Router {
        Router::new().route("/read", revalidated(get(move || async move { body })))
    }

    async fn read(app: Router, none_match: Option<&str>) -> axum::response::Response {
        let mut request = Request::get("/read");
        if let Some(tag) = none_match {
            request = request.header(header::IF_NONE_MATCH, tag);
        }
        app.oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    #[tokio::test]
    async fn a_read_carries_a_weak_tag_and_says_to_revalidate() {
        let response = read(app("{\"menu\":[]}"), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let tag = response.headers()[header::ETAG].to_str().expect("ascii");
        assert!(tag.starts_with("W/\"") && tag.len() == 36, "{tag}");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-cache"
        );
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(&body[..], b"{\"menu\":[]}");
    }

    #[tokio::test]
    async fn the_tag_it_sent_is_answered_with_a_bodiless_304() {
        let first = read(app("{\"menu\":[]}"), None).await;
        let tag = first.headers()[header::ETAG]
            .to_str()
            .expect("ascii")
            .to_owned();
        let again = read(app("{\"menu\":[]}"), Some(&tag)).await;
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(again.headers()[header::ETAG].to_str().expect("ascii"), tag);
        let body = to_bytes(again.into_body(), 1024).await.expect("body");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn a_changed_body_is_sent_whole_under_a_new_tag() {
        let first = read(app("{\"menu\":[]}"), None).await;
        let tag = first.headers()[header::ETAG]
            .to_str()
            .expect("ascii")
            .to_owned();
        let changed = read(app("{\"menu\":[1]}"), Some(&tag)).await;
        assert_eq!(changed.status(), StatusCode::OK);
        assert_ne!(
            changed.headers()[header::ETAG].to_str().expect("ascii"),
            tag
        );
    }

    #[tokio::test]
    async fn a_refusal_is_passed_through_untagged() {
        let app = Router::new().route(
            "/read",
            revalidated(get(|| async { StatusCode::FORBIDDEN })),
        );
        let response = read(app, Some("*")).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(response.headers().get(header::ETAG).is_none());
    }
}
