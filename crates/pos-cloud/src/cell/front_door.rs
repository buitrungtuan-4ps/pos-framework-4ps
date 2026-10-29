// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The front door: a request for a tenant homed in another cell is redirected there, never proxied
//! ([ADR-0179](../../../../docs/adr/0179-cells-the-directory-and-the-front-door.md) C5).
//!
//! A tenant is reached at its own hostname, `<tenant_slug>.<domain>` ([ADR-0023](../../../../docs/adr/0023-tenant-hostname-and-slug.md)),
//! and the directory says which cell is its home. When a request arrives at a cell that is not the
//! tenant's home (a stale DNS record, a bookmark from before a move, a fork's shared front door),
//! this cell answers with a redirect to the home cell's base URL, keeping the path and query:
//!
//! - **`302 Found`** for a browser: a `GET` or `HEAD` that accepts `text/html`. A browser follows it
//!   and does not keep it.
//! - **`307 Temporary Redirect`** for anything else, an API call: the method and body are kept.
//! - **Never `301`**, which browsers cache without limit, so a tenant that moves again would be
//!   stranded. `308` is for a move recorded as final, which the directory cannot say yet (C9).
//!
//! The redirect carries a `cell_id` cookie naming the home cell, so a client can see where it was
//! sent. The cookie holds only the cell id, no personal data, and is **host-only**: it has no
//! `Domain` attribute, so it is never set on the parent domain every tenant shares.
//!
//! Everything else passes through untouched: a request to this cell's own host, a host whose first
//! label is not a tenant slug, a tenant the directory does not list, and a tenant homed here. A cell
//! in self mode is every tenant's home, so a single deployment never redirects.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::cell::signed::CellDirectory;

/// How long a client keeps the `cell_id` cookie, in seconds: thirty days. It is a hint about where
/// a tenant lives, refreshed by every redirect, never an authority.
const CELL_COOKIE_MAX_AGE_SECS: u32 = 30 * 24 * 60 * 60;

/// Where a request should go instead, and how to say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Redirect {
    /// `302` for a browser, `307` for an API call.
    pub(crate) status: StatusCode,
    /// The home cell's base URL with the request's path and query.
    pub(crate) location: String,
    /// The home cell's id, for the cookie.
    pub(crate) cell_id: String,
}

/// Decides whether a request belongs to another cell. Pure, so every case is a unit test.
pub(crate) fn decide(
    directory: &CellDirectory,
    host: Option<&str>,
    method: &Method,
    headers: &HeaderMap,
    path_and_query: &str,
) -> Option<Redirect> {
    let host = host?.split(':').next()?.to_ascii_lowercase();
    let own_host = directory
        .cell()
        .base_url
        .as_deref()
        .and_then(|base| url::Url::parse(base).ok())
        .and_then(|base| base.host_str().map(str::to_owned));
    if own_host.as_deref() == Some(host.as_str()) {
        return None;
    }
    // A tenant hostname is `<slug>.<domain>`: a single label is never one. Whether the first label is
    // a tenant is the directory's to say, and it holds only well-formed slugs.
    let slug = host.split('.').next()?;
    if slug == host {
        return None;
    }
    let home = directory.tenant_home(slug)?;
    if home.here {
        return None;
    }
    let base = home.base_url?;
    Some(Redirect {
        status: if is_browser(method, headers) {
            StatusCode::FOUND
        } else {
            StatusCode::TEMPORARY_REDIRECT
        },
        location: format!("{base}{path_and_query}"),
        cell_id: home.cell_id,
    })
}

/// A browser navigation: a `GET` or `HEAD` that accepts HTML. Anything else is an API call, whose
/// method and body a `307` keeps.
fn is_browser(method: &Method, headers: &HeaderMap) -> bool {
    (method == Method::GET || method == Method::HEAD)
        && headers
            .get(header::ACCEPT)
            .and_then(|accept| accept.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html"))
}

impl IntoResponse for Redirect {
    fn into_response(self) -> Response {
        let mut response = self.status.into_response();
        let headers = response.headers_mut();
        if let Ok(location) = HeaderValue::from_str(&self.location) {
            headers.insert(header::LOCATION, location);
        }
        // Host-only: no `Domain`, so it is never shared with the parent domain every tenant sits on.
        let cookie = format!(
            "cell_id={}; Path=/; Max-Age={CELL_COOKIE_MAX_AGE_SECS}; Secure; HttpOnly; SameSite=Lax",
            self.cell_id
        );
        if let Ok(cookie) = HeaderValue::from_str(&cookie) {
            headers.insert(header::SET_COOKIE, cookie);
        }
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

/// The front-door layer over the whole service: redirects a request for a tenant homed elsewhere,
/// and passes every other request on.
pub async fn front_door(
    State(directory): State<Arc<CellDirectory>>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .map(str::to_owned)
        .or_else(|| request.uri().host().map(str::to_owned));
    let path_and_query = request
        .uri()
        .path_and_query()
        .map_or("/", |path| path.as_str());
    match decide(
        &directory,
        host.as_deref(),
        request.method(),
        request.headers(),
        path_and_query,
    ) {
        Some(redirect) => redirect.into_response(),
        None => next.run(request).await,
    }
}

#[cfg(test)]
mod tests;
