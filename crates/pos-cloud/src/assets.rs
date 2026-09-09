// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Serving the embedded back-office dashboard (ADR-0060).
//!
//! The `dashboard/dist` build is compiled into the binary with `rust-embed`, so `pos_cloud` stays
//! one static file exactly as `pos_edge` does ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md),
//! [ADR-0002](../../../docs/adr/0002-one-binary-per-tier.md)). This is the router's fallback: the API
//! routes match first, and everything else — `/`, client-routed paths, the built assets — resolves
//! here. Under the `dev-ui` feature the same paths are read from disk instead, so a UI change is a
//! browser refresh rather than a Rust rebuild.

use axum::http::{HeaderValue, Uri, header};
use axum::response::{IntoResponse, Response};
#[cfg(not(feature = "dev-ui"))]
use rust_embed::RustEmbed;

use pos_proto::error::ErrorStatus;

use crate::http::api_error;

/// The embedded dashboard. `debug-embed` (see `Cargo.toml`) makes this embed in every profile, so the
/// tests exercise the shipped serving path. Under `dev-ui` the assets are read from disk instead, so
/// this is not compiled.
#[cfg(not(feature = "dev-ui"))]
#[derive(RustEmbed)]
#[folder = "../../dashboard/dist"]
struct Assets;

/// The API prefixes this binary serves. A path on one of these is never a client route, so an
/// unmatched one is a caller's mistake and must say so rather than silently receiving the SPA.
///
/// Kept as a list rather than derived from the router because axum offers no way to enumerate a
/// composed `Router`'s routes; `tests::the_api_prefixes_match_the_routers` holds it against the
/// registrations instead.
const API_PREFIXES: [&str; 6] = [
    "/admin",
    "/v1",
    "/sync",
    "/internal",
    "/activate",
    "/health",
];

/// Whether `path` addresses the API rather than the dashboard.
///
/// A **segment** match, deliberately, not a string prefix: the console has a screen at `/admins`,
/// and treating `/admin` as a bare prefix would make that screen — and any future screen whose path
/// begins with an API prefix — answer a `404` instead of loading. This is the same collision that
/// made the Admins screen a blank page under `pnpm dev`, in the other direction (Wave 3 · D4/D5).
fn is_api_path(path: &str) -> bool {
    API_PREFIXES.iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Serves a dashboard asset, falling back to `index.html`.
///
/// An unknown path is generally not a 404: it is a client-routed path the single-page app will
/// resolve, so it receives `index.html`. Three cases are not that, and each answers the error
/// envelope instead, because handing them an HTML document turns a clear failure into a confusing
/// one (Wave 3 · D5):
///
/// - a path on an API prefix ([`is_api_path`]) — the caller has the route wrong, and a typed client
///   cannot parse HTML;
/// - a missing file under `assets/` — the browser would parse a page as JavaScript;
/// - no dashboard embedded at all, which only happens if the build is misconfigured.
#[expect(
    clippy::unused_async,
    reason = "an axum fallback handler must be async even when the body does no I/O; serving \
              embedded bytes is synchronous"
)]
pub async fn serve(uri: Uri) -> Response {
    // An API path that reached the fallback matched no route: a typo, a stale client, a probe. It is
    // not a client route, so it gets the error envelope every other refusal on this surface uses
    // rather than `index.html` — which is what it received before, as a `200` with an HTML body that
    // no typed client can parse, turning "you have the path wrong" into "the response is corrupt"
    // (Wave 3 · D5).
    if is_api_path(uri.path()) {
        return api_error(ErrorStatus::NotFound, "no route matches this path");
    }
    let mut requested = uri.path().trim_start_matches('/');
    // A path escaping the dashboard root can only be a probe; treat it as a client route.
    if requested.is_empty() || requested.contains("..") {
        requested = "index.html";
    }
    if let Some(bytes) = bytes_of(requested) {
        return respond(requested, bytes);
    }
    // A **missing** build asset is not a client route either, and this one is worth its own branch
    // because the failure it produced was so misleading. `assets/` is what Vite emits under; nothing
    // there is ever a screen. When one was absent — a browser holding a stale `index.html` across a
    // deploy asks for the previous build's hashed chunk, and `index.html` is served `no-cache` while
    // the chunks are `immutable`, so that window is real — the fallback answered `200` with the HTML
    // document. The browser then parsed a page as JavaScript and reported a syntax error at line 1
    // of a file that exists, which says nothing about the cause. A `404` makes it "failed to load
    // resource", which is the truth and points at the deploy.
    if requested.starts_with("assets/") {
        return api_error(ErrorStatus::NotFound, "no such dashboard asset");
    }
    if let Some(bytes) = bytes_of("index.html") {
        return respond("index.html", bytes);
    }
    api_error(
        ErrorStatus::NotFound,
        "no dashboard is embedded in this build",
    )
}

/// Builds a response with the right content type and cache policy. The MIME comes from the extension
/// via [`mime_for`] and the caching from [`cache_control_for`]; both return a `&'static str`, so the
/// header values are infallible.
fn respond(path: &str, bytes: Vec<u8>) -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(mime_for(path)),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control_for(path)),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// The `Cache-Control` for a dashboard asset, by path.
///
/// Vite fingerprints everything it emits under `assets/` with a content hash, so those bytes are
/// immutable — a new build changes the filename, never the contents at a given name — and can be
/// cached hard and effectively forever. Everything else, above all the `index.html` that the SPA and
/// every client-routed path fall back to, must be revalidated, so a fresh deploy is picked up on the
/// next load rather than served stale from a browser cache.
fn cache_control_for(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// The bytes of a dashboard asset, from the binary or (under `dev-ui`) from disk. `None` if absent.
#[cfg(not(feature = "dev-ui"))]
fn bytes_of(path: &str) -> Option<Vec<u8>> {
    Assets::get(path).map(|file| file.data.into_owned())
}

/// The bytes of a dashboard asset read from `dashboard/dist` on disk, for `dev-ui`. `None` if absent.
#[cfg(feature = "dev-ui")]
fn bytes_of(path: &str) -> Option<Vec<u8>> {
    // `..` is already stripped by `serve`, so joining `path` stays under the dashboard root.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dashboard/dist");
    std::fs::read(root.join(path)).ok()
}

/// The content type for a dashboard asset, by extension. A closed set: these are the only kinds a
/// built SolidJS app emits, and an unknown extension gets the safe binary default.
fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{StatusCode, Uri, header};

    use super::{API_PREFIXES, cache_control_for, is_api_path, mime_for, serve};

    #[test]
    fn known_extensions_map_to_web_mime_types() {
        assert_eq!(mime_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("style.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("logo.svg"), "image/svg+xml");
    }

    #[test]
    fn an_unknown_extension_is_a_safe_binary_default() {
        assert_eq!(mime_for("mystery.xyz"), "application/octet-stream");
        assert_eq!(mime_for("noextension"), "application/octet-stream");
    }

    #[test]
    fn hashed_assets_cache_forever_and_the_entry_document_revalidates() {
        // Fingerprinted bundles under `assets/` are immutable — cache them hard.
        assert_eq!(
            cache_control_for("assets/index-a1b2c3.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control_for("assets/index-d4e5f6.css"),
            "public, max-age=31536000, immutable"
        );
        // The SPA entry document and root files must revalidate so a new deploy is seen at once.
        assert_eq!(cache_control_for("index.html"), "no-cache");
        assert_eq!(cache_control_for("favicon.ico"), "no-cache");
    }

    #[test]
    fn an_api_path_is_recognised_whole_or_with_a_child() {
        for prefix in API_PREFIXES {
            assert!(is_api_path(prefix), "{prefix} is an API path on its own");
            assert!(
                is_api_path(&format!("{prefix}/anything")),
                "{prefix}/anything is an API path"
            );
        }
        assert!(is_api_path("/admin/stores/01J9/config"));
        assert!(is_api_path("/sync/stores/01J9/heartbeat"));
    }

    /// The reason `is_api_path` matches segments rather than string prefixes. `/admins` is the
    /// console's admin-roster screen; a bare-prefix test would answer it `404` and blank the screen,
    /// which is precisely the collision this file is here to stop making in the other direction.
    #[test]
    fn a_console_screen_whose_path_starts_with_an_api_prefix_is_not_an_api_path() {
        assert!(!is_api_path("/admins"));
        assert!(!is_api_path("/v1beta-screen"));
        assert!(!is_api_path("/healthy-stores"));
        assert!(!is_api_path("/synchronise"));
    }

    #[test]
    fn a_client_route_is_not_an_api_path() {
        assert!(!is_api_path("/"));
        assert!(!is_api_path("/login"));
        assert!(!is_api_path("/t/01J9/stores"));
        assert!(!is_api_path("/assets/index-a1b2c3.js"));
    }

    /// Builds the `Uri` axum hands the fallback, so these read like a request.
    fn get(path: &str) -> Uri {
        path.parse().expect("a literal path parses as a Uri")
    }

    #[tokio::test]
    async fn a_client_route_receives_the_spa_document() {
        // What makes the SPA's own routing work: a path the server knows nothing about is the
        // client's to resolve, so it gets `index.html` and a `200`.
        for path in ["/", "/login", "/admins", "/t/01J9/stores"] {
            let response = serve(get(path)).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{path} should load the SPA"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("text/html; charset=utf-8"),
                "{path} should be served the HTML document"
            );
        }
    }

    #[tokio::test]
    async fn an_unmatched_api_path_is_refused_rather_than_handed_the_spa() {
        let response = serve(get("/admin/no-such-endpoint")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json"),
            "an API refusal is the error envelope, not an HTML page a typed client cannot parse"
        );
    }

    #[tokio::test]
    async fn a_missing_build_asset_is_refused_rather_than_handed_the_spa() {
        // The stale-`index.html`-across-a-deploy case: answering `200 text/html` here makes the
        // browser parse a page as JavaScript and report a syntax error in a file that exists.
        let response = serve(get("/assets/index-nosuchhash.js")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// Holds [`API_PREFIXES`] against the routers, since axum cannot enumerate a composed `Router`.
    /// A new top-level surface added to `main.rs` or `http.rs` without a line here would resolve to
    /// the SPA again, so this reads the sources and fails when they disagree.
    #[test]
    fn the_api_prefixes_match_the_routers() {
        let sources = [
            include_str!("main.rs"),
            include_str!("http.rs"),
            include_str!("relay.rs"),
        ];
        let mut found: Vec<&str> = Vec::new();
        for source in sources {
            for (index, _) in source.match_indices("\"/") {
                // The segment after the opening quote, up to the next `/` or the closing quote.
                let rest = source.get(index + 2..).unwrap_or_default();
                let end = rest.find(['/', '"']).unwrap_or(0);
                let Some(segment) = rest.get(..end) else {
                    continue;
                };
                if segment.is_empty()
                    || !segment.chars().all(|character| {
                        character.is_ascii_lowercase() || character.is_ascii_digit()
                    })
                {
                    continue;
                }
                if !found.contains(&segment) {
                    found.push(segment);
                }
            }
        }
        for prefix in API_PREFIXES {
            let segment = prefix.trim_start_matches('/');
            assert!(
                found.contains(&segment),
                "API_PREFIXES lists {prefix} but no source registers a path under it — stale entry"
            );
        }
    }
}
