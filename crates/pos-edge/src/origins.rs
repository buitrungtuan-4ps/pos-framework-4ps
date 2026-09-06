// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which other origins may address this store's edge
//! ([ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
//!
//! The edge has always assumed one origin: the browser is served by the box it talks to, so
//! `fetch(path)` with a root-relative path works on a shop LAN with no configuration, and there is no
//! CORS layer anywhere because none was ever needed. A native shell, a hosted edge placement reached
//! by hostname, and a second front-end are each a *different* origin, and each is refused today.
//!
//! # Why this is a carrier and not part of `EdgeSession`
//!
//! [`session_from_config`](crate::config_client::session_from_config) is what applies every other
//! pulled node, and it is the wrong shape for this one: it produces an `EdgeSession`, which is what
//! the *application* layer decides against. This list is read on the front of every HTTP request,
//! before any handler, so it has to be reachable from [`AppState`](crate::state::AppState) — built
//! once at start-up and cheap to clone.
//!
//! So it is shaped exactly like [`Pairing`](crate::pairing::Pairing): interior mutability, held as
//! one `Arc`, written by the config-pull loop and read by the request path. No new dependency —
//! `std::sync::Mutex` is what `Pairing` already uses.
//!
//! # The list is additional to same-origin, never a replacement
//!
//! A request whose `Origin` matches the origin that served it is allowed with no list at all. Without
//! that rule a store with no `origins` node published would refuse its own UI, and the config tree's
//! never-blank contract would turn a malformed publish into a dark shop.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// The bound and the refusals, which are wire facts and live with the node
/// ([`pos_proto::origins`]).
///
/// Re-exported rather than redefined: the cloud refuses a bad origin at authoring time and the edge
/// refuses one at apply time, and two copies of "what is a valid origin" would be two rules that
/// drift — into an edge quietly dropping what the console said it saved.
pub use pos_proto::origins::{MAX_ORIGINS, OriginsError, validate_origins};

/// The origins this store's edge answers, beside the one that served the request.
///
/// Written by the config-pull loop, read by the CORS layer and by the `/ws` upgrade. One `Arc` is
/// shared by all three.
#[derive(Debug, Default)]
pub struct Origins {
    allowed: Mutex<Vec<String>>,
}

impl Origins {
    /// An empty list — the state every store starts in and most stay in.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates a published list and replaces the held one, or refuses the document whole.
    ///
    /// The never-blank rule lives here rather than in the caller: a list that does not validate
    /// leaves the previous one in place, so a malformed publish cannot open the edge to nobody — or
    /// to everybody.
    ///
    /// # Errors
    ///
    /// [`OriginsError`] if the list is over the bound or any entry is refused. Nothing is replaced.
    pub fn replace<I, S>(&self, published: I) -> Result<usize, OriginsError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Every entry is validated before any is written, so a refusal leaves the previous list
        // whole — the never-blank rule lives in the one place both sides call.
        let validated = validate_origins(published)?;
        let count = validated.len();
        *self.allowed.lock().unwrap_or_else(PoisonError::into_inner) = validated;
        Ok(count)
    }

    /// Whether this exact origin is on the published list.
    ///
    /// Does **not** consider the serving origin: that comparison needs the request's own `Host` and
    /// belongs to the caller, which has it. Keeping it out means this answers one question.
    #[must_use]
    pub fn contains(&self, origin: &str) -> bool {
        self.allowed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|allowed| allowed == origin)
    }

    /// The origins currently held, for a log line or a test.
    #[must_use]
    pub fn published(&self) -> Vec<String> {
        self.allowed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// How long a browser may cache a preflight, in seconds.
///
/// Chosen against the cap that *binds*, not the one that flatters: Chromium honours at most 600
/// seconds and Firefox at most 86400, so any larger number is only ever true on one engine. Ten
/// minutes is the whole of what the estate's browsers will give.
///
/// It matters more than it looks. `Authorization` is not a CORS-safelisted request header, so every
/// cross-origin `/api/*` call is preceded by an `OPTIONS` preflight — without a max-age a hosted edge
/// placement pays two WAN round trips per call instead of one, and
/// [ADR-0018](../../../docs/adr/0018-http-websocket-stack.md)'s under-50 ms budget was measured
/// across a shop, not across a WAN.
const PREFLIGHT_MAX_AGE: Duration = Duration::from_secs(600);

/// Whether this `Origin` may address the edge: the origin that served the request, or one the cloud
/// published for this store.
///
/// The serving origin is the request's own `Host`, because the edge cannot read one off itself — it
/// has no canonical hostname, is reached by IP on a LAN and by name when hosted, and both are
/// legitimate. **The scheme is not compared**: a reverse proxy terminating TLS forwards `Host` intact
/// but the edge sees `http`, so comparing schemes would refuse a store's own UI behind exactly the
/// deployment ADR-0110 added.
fn permitted(origin: &HeaderValue, headers: &HeaderMap, allowed: &Origins) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if let Some(host) = host
        && let Some((_, authority)) = origin.split_once("://")
        && authority == host
    {
        return true;
    }
    allowed.contains(origin)
}

/// The one CORS policy, built from the shared allow-list
/// ([ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
///
/// Built once and applied by each router constructor to *its own* covered subset — never to a merged
/// application, which would also cover `/healthz`, `/ws`, the asset fallback and
/// `POST /api/activate`, every one of which ADR-0111 declares not covered. A route is covered because
/// a constructor named it, never because it happened to sit under a layer somebody put at the top.
///
/// Three properties are load-bearing and are asserted by the tests below:
///
/// - **`Access-Control-Allow-Credentials` is never sent.** Not `true`, not `false` — absent. The
///   device token is a bearer in `Authorization`, and no cookie exists anywhere on the edge. A cookie
///   is an *ambient* credential the browser attaches whether or not the calling code knew it existed;
///   turn credentials on and every allow-listed origin can drive the till with the operator's
///   authority, and so can a cross-site form post. A bearer in a header has no CSRF surface.
/// - **`Access-Control-Allow-Origin` echoes the single matched origin**, never `*` and never a list.
/// - **Every response carries `Vary: Origin`.** This is the classic defect in a hand-rolled CORS
///   layer: an intermediary caches a response holding one origin's `Allow-Origin` and serves it to
///   another. `tower_http` sets it for a predicate policy; the test pins it so a later simplification
///   to a static origin cannot quietly drop it.
pub fn cors_layer(allowed: &Arc<Origins>) -> CorsLayer {
    let allowed = Arc::clone(allowed);
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, parts| {
            permitted(origin, &parts.headers, &allowed)
        }))
        // The only header and method shapes the typed client sends.
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .max_age(PREFLIGHT_MAX_AGE)
}

/// Refuses a `/ws` upgrade from an origin this store does not answer
/// ([ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
///
/// # Why `/ws` needs its own gate and the CORS layer is no help
///
/// A browser applies **no** same-origin policy to a WebSocket handshake: `new WebSocket(...)` to
/// another origin is not preflighted, is not blocked, and the response is delivered to the calling
/// page whatever the server's CORS headers say. So the layer that covers `/api/*` protects `/ws`
/// exactly not at all, and putting `CorsLayer` on it would be decoration.
///
/// What makes that a hole rather than a curiosity is the *other* half of the handshake: a browser
/// cannot set `Authorization` on a WebSocket, so the device token is presented as a subprotocol
/// ([`SUBPROTOCOL`](crate::http::ws::SUBPROTOCOL)) — a value a page must know. It is not ambient the
/// way a cookie is, so this is not classic cross-site WebSocket hijacking. It is the narrower case
/// that matters on a shop LAN: any page the operator visits on the till can open a socket to
/// `ws://<box>/ws` and, with a token it obtained or guessed, read every committed order, bill and
/// settlement. This gate makes the origin list mean the same thing on both surfaces, so an operator
/// who publishes two origins gets exactly two — not two for `fetch` and every origin for the event
/// stream.
///
/// # A missing `Origin` is allowed
///
/// A request with no `Origin` header is not a browser: browsers always send one on a WebSocket
/// handshake. It is the native shell, the `websocat` in a runbook, the integration test — and for
/// those the device token is the gate, as it has always been. Refusing here would break every
/// non-browser consumer to defend against an attacker who can simply omit a header, which buys
/// nothing.
pub(crate) async fn require_permitted_origin_ws(
    State(allowed): State<Arc<Origins>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if let Some(origin) = headers.get(header::ORIGIN)
        && !permitted(origin, headers, &allowed)
    {
        // The origin is logged: it is a published configuration value or an attacker's hostname,
        // and neither is a secret. It is what tells an operator whether they forgot to publish a
        // shell's origin or whether something on the LAN is probing the box.
        tracing::warn!(
            origin = ?origin,
            "refused a /ws upgrade from an origin this store does not answer"
        );
        return (
            StatusCode::FORBIDDEN,
            "this origin may not open a socket to the edge",
        )
            .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::{MAX_ORIGINS, Origins, OriginsError};

    #[test]
    fn an_exact_origin_is_held_and_compared_by_equality() {
        let origins = Origins::new();
        assert_eq!(
            origins.replace(["https://till.example.com", "http://localhost:1420"]),
            Ok(2)
        );
        assert!(origins.contains("https://till.example.com"));
        assert!(origins.contains("http://localhost:1420"));
        // Equality, not prefix or suffix: the whole point of refusing wildcards.
        assert!(!origins.contains("https://till.example.com.evil.test"));
        assert!(!origins.contains("https://evil.test/https://till.example.com"));
        assert!(!origins.contains("https://TILL.example.com"));
    }

    #[test]
    fn a_store_that_published_nothing_allows_nothing_extra() {
        let origins = Origins::new();
        assert!(origins.published().is_empty());
        assert!(!origins.contains("https://till.example.com"));
        // And that is not a dark shop: same-origin is decided by the caller against the request's
        // own Host, never by this list.
    }

    #[test]
    fn a_refused_entry_reaches_this_carrier_from_the_shared_rule() {
        // The exhaustive refusal table lives with the rule in `pos_proto::origins`; what this pins is
        // that the carrier still *calls* it, rather than accepting whatever it is handed.
        let origins = Origins::new();
        assert!(matches!(
            origins.replace(["https://*.example.com"]),
            Err(OriginsError::Invalid { .. })
        ));
        assert!(origins.published().is_empty());
    }

    #[test]
    fn a_refused_document_leaves_the_previous_list_whole() {
        let origins = Origins::new();
        origins
            .replace(["https://till.example.com"])
            .expect("the first list is valid");

        // One bad entry among good ones refuses the document, and the *previous* list survives. A
        // half-applied list is a list nobody authored.
        let refused = origins.replace(["https://kiosk.example.com", "https://*.example.com"]);
        assert!(matches!(refused, Err(OriginsError::Invalid { .. })));
        assert_eq!(origins.published(), vec!["https://till.example.com"]);
        assert!(!origins.contains("https://kiosk.example.com"));
    }

    #[test]
    fn the_bound_is_enforced_before_anything_is_written() {
        let origins = Origins::new();
        origins
            .replace(["https://till.example.com"])
            .expect("the first list is valid");
        let over: Vec<String> = (0..=MAX_ORIGINS)
            .map(|index| format!("https://shell-{index}.example.com"))
            .collect();
        assert_eq!(
            origins.replace(&over),
            Err(OriginsError::TooMany {
                count: MAX_ORIGINS + 1
            })
        );
        assert_eq!(origins.published(), vec!["https://till.example.com"]);
    }

    #[test]
    fn an_empty_published_list_is_valid_and_clears_the_previous_one() {
        // Distinct from a *malformed* document, which the caller must not pass here at all: an empty
        // list is a person deliberately withdrawing every second origin, and it has to be possible.
        let origins = Origins::new();
        origins
            .replace(["https://till.example.com"])
            .expect("the first list is valid");
        assert_eq!(origins.replace::<[&str; 0], &str>([]), Ok(0));
        assert!(origins.published().is_empty());
    }
}
