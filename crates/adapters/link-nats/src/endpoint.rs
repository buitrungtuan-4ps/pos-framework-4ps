// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Lifting credentials out of a NATS URL, because `async-nats` does not.
//!
//! # The defect this exists to fix
//!
//! Every document in this tree tells an operator to put the broker token in the URL —
//! `bootstrap.sh` writes `url = "nats://:THE_NATS_TOKEN@nats:4222"` into `cloud.toml` as the
//! instruction for arming the ingest cursor, the new-store wizard emits `POS_EDGE_NATS_URL` the same
//! way, and [ADR-0089](../../../../docs/adr/0089-edge-event-bus-transport.md) rests on it.
//!
//! **`async-nats` never reads it.** Its connector builds the `CONNECT` frame from
//! `ConnectOptions::auth` only; `ServerAddr::username`, `password` and `has_user_pass` are public
//! accessors with no caller inside the crate. So a token in the URL is silently discarded, and a
//! broker configured with `authorization { token: … }` — which is what `bootstrap.sh` generates —
//! answers with an authorization violation. The documented way to turn the feed on could not work.
//!
//! It survived because the integration suite runs NATS with **no authorization at all**
//! (`.github/workflows/main.yml`, `nats -js -m 8222`) and connects with a bare `127.0.0.1:4222`. The
//! tests exercise the one posture no deployment uses.
//!
//! # What this module does
//!
//! [`split`] separates a URL into the address to dial and the credentials to present, and the
//! adapter's two `connect` functions pass the latter through `ConnectOptions`. Three properties
//! matter:
//!
//!  * **A schemeless address is returned untouched.** `127.0.0.1:4222` and `nats:4222` are the forms
//!    the tests and `compose.yml` use, and `Url::parse` would read the host as a *scheme*. Anything
//!    that does not start with a NATS scheme is passed through, so this cannot change how an
//!    existing address behaves.
//!  * **The credentials are removed from the address.** They are of no use to `async-nats`, and an
//!    address travels into connection errors and `Debug` output — a token has no business there.
//!  * **Percent-encoding is decoded.** A URL's userinfo *must* encode `@`, `:`, `/`, `?` and `#` to
//!    parse at all, so sending the raw field would present a mangled secret for exactly the tokens a
//!    fork is most likely to have trouble with.

use core::fmt;

use percent_encoding::percent_decode_str;
use url::Url;

/// The schemes `async_nats::ServerAddr` accepts. A string that starts with none of them is not a URL
/// as far as this module is concerned.
const SCHEMES: [&str; 4] = ["nats://", "tls://", "ws://", "wss://"];

/// What a NATS URL's userinfo asks us to present at connect time.
///
/// `Debug` is redacted: this type exists to carry a secret, and it is held on a struct that error
/// paths format.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Credentials {
    /// A single token — `nats://TOKEN@host` or `nats://:TOKEN@host`. Both spellings are in use, and
    /// both mean the same thing to a broker with an `authorization { token: … }` block.
    Token(String),
    /// A username and password — `nats://user:pass@host`.
    UserPassword { user: String, password: String },
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Token(_) => formatter.write_str("Token(<redacted>)"),
            Self::UserPassword { user, .. } => formatter
                .debug_struct("UserPassword")
                .field("user", user)
                .field("password", &"<redacted>")
                .finish(),
        }
    }
}

/// A NATS address with its credentials separated out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
    address: String,
    credentials: Option<Credentials>,
}

impl Endpoint {
    /// The address to dial, with any credentials removed.
    pub(crate) fn address(&self) -> &str {
        &self.address
    }

    /// The connect options carrying whatever the URL asked for. Defaults when it asked for nothing,
    /// which is the tokenless posture the tests use.
    pub(crate) fn options(&self) -> async_nats::ConnectOptions {
        let options = async_nats::ConnectOptions::new();
        match &self.credentials {
            Some(Credentials::Token(token)) => options.token(token.clone()),
            Some(Credentials::UserPassword { user, password }) => {
                options.user_and_password(user.clone(), password.clone())
            }
            None => options,
        }
    }
}

/// Splits `url` into the address to dial and the credentials to present.
///
/// Never fails: a string this cannot read as a NATS URL — a schemeless `host:port`, or a malformed
/// URL — is returned verbatim with no credentials, so `async_nats` produces exactly the address
/// error it produces today rather than one invented here.
pub(crate) fn split(url: &str) -> Endpoint {
    let lower = url.to_ascii_lowercase();
    if !SCHEMES.iter().any(|scheme| lower.starts_with(scheme)) {
        return Endpoint {
            address: url.to_owned(),
            credentials: None,
        };
    }
    let Ok(mut parsed) = Url::parse(url) else {
        return Endpoint {
            address: url.to_owned(),
            credentials: None,
        };
    };
    let credentials = credentials_of(&parsed);
    // Strip what we lifted. Both setters only fail on a cannot-be-a-base URL, which a `nats://` URL
    // is not; on the impossible branch keep the parsed form rather than the original, so a caller
    // cannot end up dialling an address whose credentials we also passed as options.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    Endpoint {
        address: parsed.into(),
        credentials,
    }
}

/// The credentials a parsed URL's userinfo carries, decoded.
fn credentials_of(parsed: &Url) -> Option<Credentials> {
    let user = decode(parsed.username());
    let password = parsed.password().map(decode);
    match (user.is_empty(), password) {
        // `nats://user:pass@host`.
        (false, Some(password)) if !password.is_empty() => {
            Some(Credentials::UserPassword { user, password })
        }
        // `nats://TOKEN@host` — a lone userinfo is a token, the convention the `nats` CLI uses.
        // Also `nats://user:@host`, where an empty password leaves the username as the only secret.
        (false, _) => Some(Credentials::Token(user)),
        // `nats://:TOKEN@host` — the spelling every document in this tree uses.
        (true, Some(password)) if !password.is_empty() => Some(Credentials::Token(password)),
        // No userinfo, or an empty one: present nothing, exactly as before this module existed.
        (true, _) => None,
    }
}

/// A userinfo field with its percent-escapes resolved. Invalid UTF-8 is kept lossily rather than
/// dropped — a broker's answer to a wrong credential is the same either way, and losing the value
/// silently would be worse than presenting it.
fn decode(field: &str) -> String {
    percent_decode_str(field).decode_utf8_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::{Credentials, split};

    #[test]
    fn the_documented_token_spelling_is_lifted_and_removed_from_the_address() {
        // The exact form bootstrap.sh writes into cloud.toml, and the wizard into the store's env
        // file. Before this module it was sent as nothing at all.
        let endpoint = split("nats://:s3cr3t@nats:4222");
        assert_eq!(
            endpoint.credentials,
            Some(Credentials::Token("s3cr3t".to_owned()))
        );
        assert_eq!(
            endpoint.address(),
            "nats://nats:4222",
            "the secret must not travel on into connection errors or Debug output"
        );
    }

    #[test]
    fn a_lone_userinfo_is_a_token() {
        let endpoint = split("nats://s3cr3t@nats:4222");
        assert_eq!(
            endpoint.credentials,
            Some(Credentials::Token("s3cr3t".to_owned()))
        );
        assert_eq!(endpoint.address(), "nats://nats:4222");
    }

    #[test]
    fn a_user_and_password_stay_a_user_and_password() {
        let endpoint = split("nats://derek:s3cr3t@nats:4222");
        assert_eq!(
            endpoint.credentials,
            Some(Credentials::UserPassword {
                user: "derek".to_owned(),
                password: "s3cr3t".to_owned(),
            })
        );
        assert_eq!(endpoint.address(), "nats://nats:4222");
    }

    #[test]
    fn the_tls_scheme_is_preserved_because_it_is_what_requires_tls() {
        // async-nats decides TLS from the scheme alone (`ServerAddr::tls_required`), so stripping
        // credentials must not disturb it — this is the address E7 will hand a store.
        let endpoint = split("tls://:s3cr3t@cloud.example.com:4222");
        assert_eq!(endpoint.address(), "tls://cloud.example.com:4222");
        assert_eq!(
            endpoint.credentials,
            Some(Credentials::Token("s3cr3t".to_owned()))
        );
    }

    #[test]
    fn a_schemeless_address_is_returned_untouched() {
        // `127.0.0.1:4222` is what the integration tests pass and `nats:4222` is compose's internal
        // name. `Url::parse` reads the host of either as a *scheme*, so they must not go near it.
        for address in ["127.0.0.1:4222", "nats:4222", "localhost"] {
            let endpoint = split(address);
            assert_eq!(endpoint.address(), address);
            assert_eq!(
                endpoint.credentials, None,
                "a schemeless address carries no userinfo to lift"
            );
        }
    }

    #[test]
    fn a_url_without_userinfo_presents_nothing() {
        let endpoint = split("nats://nats:4222");
        assert_eq!(endpoint.credentials, None);
        assert_eq!(endpoint.address(), "nats://nats:4222");
    }

    #[test]
    fn an_empty_userinfo_presents_nothing_rather_than_an_empty_token() {
        // `nats://@host` and `nats://:@host` are what an unset variable interpolates into. An empty
        // token would be a credential the broker refuses, with a worse error than none at all.
        for url in ["nats://@nats:4222", "nats://:@nats:4222"] {
            assert_eq!(split(url).credentials, None, "{url}");
        }
    }

    #[test]
    fn percent_escapes_are_decoded() {
        // A token containing `@` or `/` cannot appear literally — the URL would not parse — so the
        // raw field is the encoded form and sending it would present the wrong secret.
        let endpoint = split("nats://:p%40ss%2Fword@nats:4222");
        assert_eq!(
            endpoint.credentials,
            Some(Credentials::Token("p@ss/word".to_owned()))
        );
    }

    #[test]
    fn a_malformed_url_is_passed_through_for_async_nats_to_reject() {
        // Inventing an error here would replace the address diagnostics async-nats already gives.
        let endpoint = split("nats://");
        assert_eq!(endpoint.address(), "nats://");
        assert_eq!(endpoint.credentials, None);
    }

    #[test]
    fn the_debug_of_credentials_redacts_both_secret_shapes() {
        let token = format!("{:?}", Credentials::Token("s3cr3t".to_owned()));
        assert!(!token.contains("s3cr3t"), "{token}");
        let pair = format!(
            "{:?}",
            Credentials::UserPassword {
                user: "derek".to_owned(),
                password: "s3cr3t".to_owned(),
            }
        );
        assert!(!pair.contains("s3cr3t"), "{pair}");
        assert!(pair.contains("derek"), "a username is not a secret: {pair}");
    }
}
