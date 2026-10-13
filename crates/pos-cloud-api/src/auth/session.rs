// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The super-admin session cookie, scoped to one subdomain
//! ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
//!
//! Setting a session cookie with `Domain=pos.example.com` sends it to *every* subdomain — the single
//! worst multi-tenant isolation failure the roadmap names, because one tenant's admin session would
//! then travel to another tenant's host. The defence is to make the cookie **host-only**: no
//! `Domain` attribute, so the browser returns it only to the exact host that set it. The
//! [`__Host-`](https://developer.mozilla.org/docs/Web/HTTP/Headers/Set-Cookie#cookie_prefixes) name
//! prefix has the browser *enforce* that — a `__Host-` cookie is refused unless it is `Secure`,
//! `Path=/`, and carries no `Domain` — so the isolation rests on the browser, not on our remembering
//! to omit an attribute.
//!
//! [`set_cookie`] is a pure string builder; the opaque session token it carries is generated (from a
//! CSPRNG) and stored server-side by the login route, a later slice.

/// The session cookie's name. The `__Host-` prefix makes host-only, `Secure`, `Path=/` a
/// browser-enforced invariant rather than a convention.
pub const COOKIE_NAME: &str = "__Host-pos_admin_session";

/// Builds the `Set-Cookie` value for a freshly issued session `token`, valid for `max_age_seconds`.
///
/// `Secure` (TLS only), `HttpOnly` (no script access), `SameSite=Strict` (not sent on cross-site
/// navigation, blunting CSRF), `Path=/`, and — deliberately — **no `Domain`**, so the cookie is
/// host-only and never crosses to another subdomain.
#[must_use]
pub fn set_cookie(token: &str, max_age_seconds: u64) -> String {
    format!(
        "{COOKIE_NAME}={token}; Max-Age={max_age_seconds}; Path=/; Secure; HttpOnly; SameSite=Strict"
    )
}

/// Builds the `Set-Cookie` value that clears the session, for logout. Same attributes (so the
/// browser matches and replaces the cookie) with an immediate expiry.
#[must_use]
pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict")
}

#[cfg(test)]
mod tests {
    use super::{COOKIE_NAME, clear_cookie, set_cookie};

    #[test]
    fn the_session_cookie_is_host_only_and_hardened() {
        let cookie = set_cookie("opaque-token-abc123", 3600);
        assert!(cookie.starts_with(&format!("{COOKIE_NAME}=opaque-token-abc123")));
        assert!(
            !cookie.contains("Domain"),
            "a Domain attribute would leak the session across subdomains: {cookie}"
        );
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("Max-Age=3600"));
    }

    #[test]
    fn the_name_carries_the_host_prefix() {
        // The prefix is what makes the browser enforce host-only + Secure + Path=/.
        assert!(COOKIE_NAME.starts_with("__Host-"));
    }

    #[test]
    fn clearing_matches_the_attributes_so_the_browser_replaces_it() {
        let cookie = clear_cookie();
        assert!(cookie.starts_with(&format!("{COOKIE_NAME}=;")));
        assert!(cookie.contains("Max-Age=0"));
        assert!(!cookie.contains("Domain"));
        assert!(cookie.contains("Secure") && cookie.contains("HttpOnly"));
    }
}
