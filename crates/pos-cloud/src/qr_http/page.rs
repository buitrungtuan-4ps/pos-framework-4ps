// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The guest page at `/qr/`
//! ([ADR-0224](../../../../docs/adr/0224-self-order-is-a-guest-page-and-a-kiosk.md) decision 2).
//!
//! A small front end in the repository's `guest/`, compiled into the binary and served as it is: an
//! HTML shell, one script, one stylesheet and its English and Vietnamese strings. It reads the menu
//! from `GET /v1/qr/menu` and sends through `POST /v1/qr/orders`, with the table's token from the
//! address's fragment, which no request carries. It asks for nothing personal.
//!
//! Every answer carries a content security policy that allows only this origin's own script,
//! style and requests, and no framing, and a referrer policy that sends nothing on, so the page
//! runs no script but its own. Its security review is phase 9's.

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// The page's files, compiled in.
const INDEX: &str = include_str!("../../../../guest/index.html");
const SCRIPT: &str = include_str!("../../../../guest/app.js");
const STYLE: &str = include_str!("../../../../guest/style.css");
const STRINGS_EN: &str = include_str!("../../../../guest/i18n/en.json");
const STRINGS_VI: &str = include_str!("../../../../guest/i18n/vi.json");

/// What the page may load and where it may send: its own origin, and nothing else.
const POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                      img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The page's routes.
pub(super) fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route(
            "/qr/",
            get(|| async { file("text/html; charset=utf-8", INDEX) }),
        )
        .route(
            "/qr/app.js",
            get(|| async { file("text/javascript; charset=utf-8", SCRIPT) }),
        )
        .route(
            "/qr/style.css",
            get(|| async { file("text/css; charset=utf-8", STYLE) }),
        )
        .route("/qr/i18n/{file}", get(strings))
}

/// `GET /qr/i18n/{file}`: `en.json` or `vi.json`, and nothing else.
async fn strings(Path(name): Path<String>) -> Response {
    match name.as_str() {
        "en.json" => file("application/json", STRINGS_EN),
        "vi.json" => file("application/json", STRINGS_VI),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// One of the page's files, with the headers every one of them carries.
fn file(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(POLICY),
            ),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{SCRIPT, STRINGS_EN, STRINGS_VI};

    fn catalogue(text: &str) -> BTreeMap<String, String> {
        serde_json::from_str(text).expect("a string catalogue is a flat JSON object")
    }

    #[test]
    fn every_language_has_every_string() {
        let en = catalogue(STRINGS_EN);
        let vi = catalogue(STRINGS_VI);
        assert_eq!(en.keys().collect::<Vec<_>>(), vi.keys().collect::<Vec<_>>());
        assert!(vi.values().all(|text| !text.trim().is_empty()));
    }

    #[test]
    fn every_key_the_page_names_is_in_its_strings() {
        let en = catalogue(STRINGS_EN);
        let mut named = 0;
        for piece in SCRIPT.split("\"guest.").skip(1) {
            let key = format!(
                "guest.{}",
                piece.split('"').next().expect("a key ends at a quote")
            );
            assert!(en.contains_key(&key), "{key}");
            named += 1;
        }
        assert!(named > 10, "the page names its strings");
        for key in en.keys() {
            assert!(
                SCRIPT.contains(&format!("\"{key}\"")),
                "{key} is never shown"
            );
        }
    }

    #[test]
    fn the_page_builds_nothing_from_markup() {
        for forbidden in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "eval(",
            "document.write",
        ] {
            assert!(!SCRIPT.contains(forbidden), "{forbidden}");
        }
    }
}
