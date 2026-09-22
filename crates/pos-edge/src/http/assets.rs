// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Serving the operator UI.
//!
//! By default the `ui/dist` build is compiled into the binary with `rust-embed`, so the store is one
//! static file ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md),
//! [ADR-0002](../../../docs/adr/0002-one-binary-per-tier.md)). Under the `dev-ui` feature the same
//! paths are read from disk instead, so a UI change is a browser refresh rather than a Rust rebuild.

use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
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
pub(crate) async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let mut requested = uri.path().trim_start_matches('/');
    // A path escaping the UI root can only be a probe; treat it as a client route.
    if requested.is_empty() || requested.contains("..") {
        requested = "index.html";
    }
    if let Some(asset) = asset_at(requested) {
        return respond(requested, asset, &headers);
    }
    if let Some(asset) = asset_at("index.html") {
        return respond("index.html", asset, &headers);
    }
    (StatusCode::NOT_FOUND, "no UI is embedded in this build").into_response()
}

/// How long a hashed asset may be held. One year, the maximum the specification gives meaning to.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Held, but revalidated on every use. Not `no-store`: the point is that the answer to the
/// revalidation is a `304` carrying no body at all.
const REVALIDATE: &str = "no-cache";

/// A UI asset and, where the build computed one, the hash that identifies its contents.
struct Asset {
    bytes: Vec<u8>,
    /// `rust-embed` hashes every file at build time, so this costs nothing at run time. `None`
    /// under `dev-ui`, where the assets come off disk and the whole point is not to cache them.
    etag: Option<String>,
}

/// Whether a path may be cached forever.
///
/// Vite writes content-hashed filenames under `assets/` — `index-BM0Q6qvm.js` — precisely so they
/// can be. A new build is a new name, so `immutable` can never serve a stale one. Everything else,
/// `index.html` above all, must revalidate: it is the file that names the hashed ones, and pinning
/// it for a year would pin the release with it.
fn cache_control(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        IMMUTABLE
    } else {
        REVALIDATE
    }
}

/// Builds a response with the right content type and security headers. The MIME comes from the extension
/// via [`mime_for`], which returns a `&'static str`, so the header value is infallible.
fn respond(path: &str, asset: Asset, request: &HeaderMap) -> Response {
    let caching = cache_control(path);
    // A revalidation the client already holds the answer to. Answering it with the body again is
    // the whole cost this exists to remove: over shop Wi-Fi to a tablet, the bundle is the largest
    // thing a reload asks for, and a till reloads often.
    if let Some(etag) = asset.etag.as_deref()
        && none_match(request, etag)
    {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, HeaderValue::from_str(etag).unwrap_or(EMPTY)),
                (header::CACHE_CONTROL, HeaderValue::from_static(caching)),
            ],
        )
            .into_response();
    }
    let etag = asset
        .etag
        .as_deref()
        .and_then(|etag| HeaderValue::from_str(etag).ok())
        .unwrap_or(EMPTY);
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(mime_for(path)),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
            // Security: Prevent sensitive referrer information (e.g. internal routes or parameters)
            // from leaking to external origins when external resources or links are loaded from the UI.
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static(caching)),
            (header::ETAG, etag),
        ],
        asset.bytes,
    )
        .into_response()
}

/// A placeholder for an asset with no hash (`dev-ui`). An empty `ETag` is never sent as a validator
/// by a client, so it cannot match anything, and `none_match` is never reached without one.
const EMPTY: HeaderValue = HeaderValue::from_static("");

/// Whether the client's `If-None-Match` already holds this asset.
///
/// A list, because a client may offer several, and `*` matches anything it has.
fn none_match(request: &HeaderMap, etag: &str) -> bool {
    let Some(header) = request
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    header
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || candidate == etag)
}

/// The bytes of a UI asset, from the binary or (under `dev-ui`) from disk. `None` if absent.
#[cfg(not(feature = "dev-ui"))]
fn asset_at(path: &str) -> Option<Asset> {
    Assets::get(path).map(|file| Asset {
        etag: Some(hex_etag(file.metadata.sha256_hash())),
        bytes: file.data.into_owned(),
    })
}

/// The build-time content hash as a strong `ETag`.
#[cfg(not(feature = "dev-ui"))]
fn hex_etag(hash: [u8; 32]) -> String {
    use std::fmt::Write as _;
    // Sixteen bytes of SHA-256 is far past any collision a build could produce, and keeps the
    // header short enough to read in a trace.
    let mut etag = String::with_capacity(34);
    etag.push('"');
    for byte in &hash[..16] {
        // Infallible: writing to a `String` cannot fail.
        let _ = write!(etag, "{byte:02x}");
    }
    etag.push('"');
    etag
}

/// The bytes of a UI asset read from `ui/dist` on disk, for `dev-ui`. `None` if absent.
#[cfg(feature = "dev-ui")]
fn asset_at(path: &str) -> Option<Asset> {
    // `..` is already stripped by `serve`, so joining `path` stays under the UI root.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    std::fs::read(root.join(path)).ok().map(|bytes| Asset {
        bytes,
        // No hash off disk, and none wanted: `dev-ui` exists so a UI change is a browser refresh.
        etag: None,
    })
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
    use super::{IMMUTABLE, REVALIDATE, cache_control, mime_for, none_match};
    use axum::http::{HeaderMap, HeaderValue, header};

    #[test]
    fn a_content_hashed_asset_may_be_held_forever_and_index_may_not() {
        // Vite writes the hash into the name, so a new build is a new path and `immutable` can
        // never serve a stale one.
        assert_eq!(cache_control("assets/index-BM0Q6qvm.js"), IMMUTABLE);
        assert_eq!(cache_control("assets/index-By4X-Wi7.css"), IMMUTABLE);
        // `index.html` names those hashed files. Pinning it for a year pins the release with it,
        // and the till never learns an update shipped.
        assert_eq!(cache_control("index.html"), REVALIDATE);
        // Anything the build drops at the root is unhashed too, so it revalidates.
        assert_eq!(cache_control("favicon.ico"), REVALIDATE);
    }

    fn with_if_none_match(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_str(value).expect("a valid header value"),
        );
        headers
    }

    #[test]
    fn a_client_that_already_holds_the_asset_is_recognised() {
        let etag = "\"abc123\"";
        assert!(none_match(&with_if_none_match(etag), etag));
        // A client may offer several, and the match may be any of them.
        assert!(none_match(
            &with_if_none_match("\"other\", \"abc123\""),
            etag
        ));
        // `*` means "anything you have".
        assert!(none_match(&with_if_none_match("*"), etag));
    }

    #[test]
    fn a_stale_or_absent_validator_is_not_a_match() {
        let etag = "\"abc123\"";
        assert!(!none_match(&with_if_none_match("\"stale\""), etag));
        assert!(!none_match(&HeaderMap::new(), etag));
        // A prefix is not a match: the comparison is on the whole opaque value.
        assert!(!none_match(&with_if_none_match("\"abc\""), etag));
    }

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
