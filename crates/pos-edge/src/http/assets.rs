// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Serving the operator UI.
//!
//! By default the `ui/dist` build is compiled into the binary with `rust-embed`, so the store is one
//! static file ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md),
//! [ADR-0002](../../../docs/adr/0002-one-binary-per-tier.md)). Under the `dev-ui` feature the same
//! paths are read from disk instead, so a UI change is a browser refresh rather than a Rust rebuild.

use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
#[cfg(not(feature = "dev-ui"))]
use rust_embed::RustEmbed;

/// The embedded UI. `debug-embed` (see `Cargo.toml`) makes this embed in every profile, so the tests
/// exercise the shipped serving path. Under `dev-ui` the assets are read from disk instead, so this
/// is not compiled.
#[cfg(not(feature = "dev-ui"))]
#[derive(RustEmbed)]
#[folder = "../../ui/dist"]
struct Assets;

/// Serves a UI asset, falling back to `index.html`.
///
/// An unknown path is not a 404: it is a client-routed path the single-page app will resolve, so it
/// receives `index.html`. A `404` is reserved for the genuinely empty case — no UI is embedded at
/// all, which only happens if the build is misconfigured.
pub(crate) async fn serve(uri: Uri) -> Response {
    let mut requested = uri.path().trim_start_matches('/');
    // A path escaping the UI root can only be a probe; treat it as a client route.
    if requested.is_empty() || requested.contains("..") {
        requested = "index.html";
    }
    if let Some(bytes) = bytes_of(requested) {
        return respond(requested, bytes);
    }
    if let Some(bytes) = bytes_of("index.html") {
        return respond("index.html", bytes);
    }
    (StatusCode::NOT_FOUND, "no UI is embedded in this build").into_response()
}

/// Builds a response with the right content type. The MIME comes from the extension via
/// [`mime_for`], which returns a `&'static str`, so the header value is infallible.
fn respond(path: &str, bytes: Vec<u8>) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(mime_for(path)),
        )],
        bytes,
    )
        .into_response()
}

/// The bytes of a UI asset, from the binary or (under `dev-ui`) from disk. `None` if absent.
#[cfg(not(feature = "dev-ui"))]
fn bytes_of(path: &str) -> Option<Vec<u8>> {
    Assets::get(path).map(|file| file.data.into_owned())
}

/// The bytes of a UI asset read from `ui/dist` on disk, for `dev-ui`. `None` if absent.
#[cfg(feature = "dev-ui")]
fn bytes_of(path: &str) -> Option<Vec<u8>> {
    // `..` is already stripped by `serve`, so joining `path` stays under the UI root.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    std::fs::read(root.join(path)).ok()
}

/// The content type for a UI asset, by extension. A closed set: these are the only kinds a built
/// SolidJS app emits, and an unknown extension gets the safe binary default.
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
    use super::mime_for;

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
}
