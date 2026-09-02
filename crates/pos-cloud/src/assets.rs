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

/// Serves a dashboard asset, falling back to `index.html`.
///
/// An unknown path is not a 404: it is a client-routed path the single-page app will resolve, so it
/// receives `index.html`. A `404` is reserved for the genuinely empty case — no dashboard is embedded
/// at all, which only happens if the build is misconfigured.
#[expect(
    clippy::unused_async,
    reason = "an axum fallback handler must be async even when the body does no I/O; serving \
              embedded bytes is synchronous"
)]
pub async fn serve(uri: Uri) -> Response {
    let mut requested = uri.path().trim_start_matches('/');
    // A path escaping the dashboard root can only be a probe; treat it as a client route.
    if requested.is_empty() || requested.contains("..") {
        requested = "index.html";
    }
    if let Some(bytes) = bytes_of(requested) {
        return respond(requested, bytes);
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
    use super::{cache_control_for, mime_for};

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
}
