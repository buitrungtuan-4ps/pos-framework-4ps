// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the operator typed on the connect page, understood: an edge's address or its pairing link.
//!
//! Three shapes arrive in the one field, and all three are real:
//!
//! - `192.168.1.10:8080` — the raw-IP form an operator reads off the edge (ADR-0030). No scheme
//!   means `http`, because that form only ever names a box on the shop's own LAN.
//! - `http://192.168.1.10:8080/pair?code=123456` — the pairing link, pasted or read from the QR.
//!   The code comes with it, so the operator need not type it again.
//! - `https://till.example/pair?code=123456` — ADR-0111's hosted form, which is `https` only.
//!
//! Everything reduces to an [`EdgeOrigin`] serialised exactly as a browser serialises
//! `window.location.origin`: lowercase host, default port dropped, IPv6 in brackets. That exactness
//! matters twice. The origin is the keychain account the token is filed under, so two spellings of
//! one edge must not become two entries; and the till's initialization script compares it against
//! `window.location.origin` before writing the token, so a spelling the browser would not produce
//! would leave the till unpaired.

use core::fmt;

/// An `http` or `https` origin with no path: where one edge is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeOrigin {
    secure: bool,
    host: String,
    port: Option<u16>,
}

/// The loopback address and port a store PC's own edge answers on, which is what selects Station
/// mode (ADR-0147).
pub(crate) const LOCAL_EDGE: &str = "127.0.0.1:8080";

impl EdgeOrigin {
    /// The store PC's own edge, `http://127.0.0.1:8080`.
    pub(crate) fn local() -> Self {
        Self {
            secure: false,
            host: "127.0.0.1".to_owned(),
            port: Some(8080),
        }
    }

    /// A path on this edge, as an absolute URL string. `path` starts with `/`.
    pub(crate) fn url(&self, path: &str) -> String {
        format!("{self}{path}")
    }

    /// Whether this names the machine the app runs on — `localhost`, `127.0.0.0/8` or `::1`.
    pub(crate) fn is_loopback(&self) -> bool {
        self.host == "localhost"
            || self.host == "[::1]"
            || self
                .host
                .parse::<std::net::Ipv4Addr>()
                .is_ok_and(|ip| ip.is_loopback())
    }
}

impl fmt::Display for EdgeOrigin {
    /// The browser's own serialisation of an origin, which is also the keychain account name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.secure { "https" } else { "http" };
        match self.port {
            Some(port) => write!(f, "{scheme}://{}:{port}", self.host),
            None => write!(f, "{scheme}://{}", self.host),
        }
    }
}

/// A six-digit pairing code, the only shape `POST /api/pair` accepts.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PairingCode(String);

impl PairingCode {
    /// Accepts six ASCII digits, ignoring spaces an operator types between groups (`123 456`).
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let digits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit())).then_some(Self(digits))
    }

    /// The six digits, as the edge expects them.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PairingCode {
    /// Redacted: a live code is redeemable for a device token until it expires.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode(<redacted>)")
    }
}

/// The connect page's address field, understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectInput {
    /// The edge to pair with.
    pub(crate) origin: EdgeOrigin,
    /// The code, when the field held a pairing link that carried a valid one.
    pub(crate) code_from_link: Option<PairingCode>,
}

/// Why an address was refused. Each maps to one message on the connect page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddressError {
    /// Nothing was typed.
    Empty,
    /// A scheme other than `http` or `https`.
    UnsupportedScheme,
    /// `user:password@` in the authority, which no edge uses and a pasted link should never carry.
    Credentials,
    /// No host before the port.
    MissingHost,
    /// A host with characters no hostname or address has, or a non-ASCII one the browser would
    /// punycode (and so serialise differently from what is stored).
    BadHost,
    /// A port that is not a number from 1 to 65535.
    BadPort,
}

/// Parses the connect page's address field.
pub(crate) fn parse(input: &str) -> Result<ConnectInput, AddressError> {
    let text = input.trim();
    if text.is_empty() {
        return Err(AddressError::Empty);
    }
    let (secure, rest) = match text.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => (false, rest),
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => (true, rest),
        Some(_) => return Err(AddressError::UnsupportedScheme),
        None => (false, text),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    if authority.contains('@') {
        return Err(AddressError::Credentials);
    }
    let (host, port) = split_host_port(authority)?;
    let default_port = if secure { 443 } else { 80 };
    let origin = EdgeOrigin {
        secure,
        host,
        port: port.filter(|port| *port != default_port),
    };
    Ok(ConnectInput {
        origin,
        code_from_link: code_in(tail),
    })
}

/// Splits `host[:port]` or `[v6][:port]`, lowercasing and validating the host.
fn split_host_port(authority: &str) -> Result<(String, Option<u16>), AddressError> {
    let (host, port_text) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (inside, after) = bracketed.split_once(']').ok_or(AddressError::BadHost)?;
        if inside.is_empty() || !inside.chars().all(|c| c.is_ascii_hexdigit() || c == ':') {
            return Err(AddressError::BadHost);
        }
        let port_text = match after {
            "" => None,
            _ => Some(after.strip_prefix(':').ok_or(AddressError::BadPort)?),
        };
        (format!("[{}]", inside.to_ascii_lowercase()), port_text)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_ascii_lowercase(), Some(port)),
            None => (authority.to_ascii_lowercase(), None),
        }
    };
    if host.is_empty() {
        return Err(AddressError::MissingHost);
    }
    let plain = !host.starts_with('[');
    if plain
        && !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return Err(AddressError::BadHost);
    }
    let port = match port_text {
        None => None,
        Some(text) => match text.parse::<u16>() {
            Ok(port) if port != 0 && text.bytes().all(|b| b.is_ascii_digit()) => Some(port),
            _ => return Err(AddressError::BadPort),
        },
    };
    Ok((host, port))
}

/// The pairing code in a `/pair?code=NNNNNN` tail, if there is a valid one.
fn code_in(tail: &str) -> Option<PairingCode> {
    let (path, query) = tail.split_once('?')?;
    if path.trim_end_matches('/') != "/pair" {
        return None;
    }
    let query = query
        .split_once('#')
        .map_or(query, |(query, _fragment)| query);
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("code="))
        .and_then(PairingCode::parse)
}

#[cfg(test)]
mod tests {
    use super::{AddressError, EdgeOrigin, PairingCode, parse};

    fn origin(input: &str) -> String {
        parse(input).unwrap().origin.to_string()
    }

    fn code(input: &str) -> Option<String> {
        parse(input)
            .unwrap()
            .code_from_link
            .map(|code| code.as_str().to_owned())
    }

    #[test]
    fn a_bare_address_is_an_http_origin() {
        assert_eq!(origin("192.168.1.10:8080"), "http://192.168.1.10:8080");
        assert_eq!(origin("  192.168.1.10:8080 \n"), "http://192.168.1.10:8080");
        assert_eq!(code("192.168.1.10:8080"), None);
    }

    #[test]
    fn the_pairing_link_carries_its_code() {
        let input = "http://192.168.1.10:8080/pair?code=123456";
        assert_eq!(origin(input), "http://192.168.1.10:8080");
        assert_eq!(code(input), Some("123456".to_owned()));
    }

    #[test]
    fn the_hosted_form_is_https_and_drops_its_default_port() {
        assert_eq!(
            origin("https://till.example/pair?code=654321"),
            "https://till.example"
        );
        assert_eq!(origin("https://till.example:443/"), "https://till.example");
        assert_eq!(origin("http://till.example:80"), "http://till.example");
        assert_eq!(origin("http://till.example:443"), "http://till.example:443");
        assert_eq!(
            code("https://till.example/pair?code=654321"),
            Some("654321".to_owned())
        );
    }

    #[test]
    fn the_origin_is_spelled_the_way_a_browser_spells_it() {
        assert_eq!(
            origin("HTTP://Till.Example:8080/"),
            "http://till.example:8080"
        );
        assert_eq!(origin("[::1]:8080"), "http://[::1]:8080");
        assert_eq!(origin("[FE80::1]"), "http://[fe80::1]");
    }

    #[test]
    fn the_code_is_found_among_other_parameters_and_not_elsewhere() {
        assert_eq!(
            code("http://10.0.0.2:8080/pair?lang=vi&code=000042#top"),
            Some("000042".to_owned())
        );
        assert_eq!(
            code("http://10.0.0.2:8080/pair/?code=123456"),
            Some("123456".to_owned())
        );
        assert_eq!(code("http://10.0.0.2:8080/floor?code=123456"), None);
        assert_eq!(code("http://10.0.0.2:8080/pair?code=12345"), None);
        assert_eq!(code("http://10.0.0.2:8080/pair?code=12345a"), None);
    }

    #[test]
    fn a_path_other_than_pair_still_names_the_edge() {
        assert_eq!(
            origin("http://10.0.0.2:8080/devices"),
            "http://10.0.0.2:8080"
        );
    }

    #[test]
    fn what_is_not_an_address_is_refused_with_its_reason() {
        assert_eq!(parse(""), Err(AddressError::Empty));
        assert_eq!(parse("   "), Err(AddressError::Empty));
        assert_eq!(
            parse("ftp://10.0.0.2"),
            Err(AddressError::UnsupportedScheme)
        );
        assert_eq!(
            parse("http://admin:pw@10.0.0.2"),
            Err(AddressError::Credentials)
        );
        assert_eq!(parse(":8080"), Err(AddressError::MissingHost));
        assert_eq!(parse("http://"), Err(AddressError::MissingHost));
        assert_eq!(parse("ho st:8080"), Err(AddressError::BadHost));
        assert_eq!(parse("tiệm-pizza:8080"), Err(AddressError::BadHost));
        assert_eq!(parse("[zz::1]:8080"), Err(AddressError::BadHost));
        assert_eq!(parse("10.0.0.2:99999"), Err(AddressError::BadPort));
        assert_eq!(parse("10.0.0.2:"), Err(AddressError::BadPort));
        assert_eq!(parse("10.0.0.2:0"), Err(AddressError::BadPort));
        assert_eq!(parse("10.0.0.2:+80"), Err(AddressError::BadPort));
        assert_eq!(parse("[::1]8080"), Err(AddressError::BadPort));
    }

    #[test]
    fn loopback_is_recognised_in_every_spelling() {
        for input in [
            "127.0.0.1:8080",
            "localhost:8080",
            "127.1.2.3",
            "[::1]:8080",
        ] {
            assert!(parse(input).unwrap().origin.is_loopback(), "{input}");
        }
        for input in ["192.168.1.10:8080", "till.example", "127.0.0.1.example"] {
            assert!(!parse(input).unwrap().origin.is_loopback(), "{input}");
        }
        assert!(EdgeOrigin::local().is_loopback());
        assert_eq!(EdgeOrigin::local().to_string(), "http://127.0.0.1:8080");
        assert_eq!(
            parse(super::LOCAL_EDGE).unwrap().origin,
            EdgeOrigin::local()
        );
    }

    #[test]
    fn a_code_is_six_digits_and_spaces_between_groups_are_forgiven() {
        assert_eq!(PairingCode::parse("123456").unwrap().as_str(), "123456");
        assert_eq!(PairingCode::parse(" 123 456 ").unwrap().as_str(), "123456");
        assert!(PairingCode::parse("12345").is_none());
        assert!(PairingCode::parse("1234567").is_none());
        assert!(PairingCode::parse("12a456").is_none());
        assert!(PairingCode::parse("١٢٣٤٥٦").is_none());
        assert_eq!(
            format!("{:?}", PairingCode::parse("123456").unwrap()),
            "PairingCode(<redacted>)"
        );
    }

    #[test]
    fn a_url_on_the_edge_is_the_origin_and_the_path() {
        let edge = parse("192.168.1.10:8080").unwrap().origin;
        assert_eq!(edge.url("/api/pair"), "http://192.168.1.10:8080/api/pair");
    }
}
