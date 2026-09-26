// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! SSRF protection for webhook destinations.
//!
//! A webhook URL is attacker-controllable: a tenant types it into an admin form. Left unchecked it
//! is a server-side request forgery primitive — `http://169.254.169.254/…` reads the cloud's own
//! instance-metadata credentials, `http://127.0.0.1:5432` probes the database, `http://10.0.0.5`
//! reaches the private network. So a destination is **vetted before every registration and before
//! the transport connects**, and the policy here is a blocklist of the ranges that must never be a
//! webhook target.
//!
//! Two layers, because DNS can lie:
//!
//!  1. **Structural** ([`vet`]): the scheme must be `https` (a webhook carries business data, so
//!     plaintext is refused), there must be no `user:pass@` credentials in the authority, and there
//!     must be a host.
//!  2. **Address** ([`classify_ip`]): every IP the host resolves to must be a public unicast
//!     address. A hostname that resolves to *any* forbidden address is refused whole, which is what
//!     stops a name that resolves to both a public IP and `127.0.0.1`.
//!
//! [`vet`] takes the resolver as an argument so the policy is tested without a network; the
//! transport passes a real `getaddrinfo`-backed resolver and then connects to one of the returned,
//! already-vetted addresses — never re-resolving — which closes the DNS-rebinding gap between check
//! and connect.

use core::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A destination that passed every check: the URL to send to, and the exact addresses it resolved
/// to (so the transport connects to a vetted one rather than resolving again).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VettedUrl {
    /// The normalized destination URL.
    pub url: String,
    /// The vetted resolved addresses — non-empty, every one public unicast.
    pub addresses: Vec<IpAddr>,
}

/// Why a destination was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SsrfRejection {
    /// The URL did not parse.
    #[error("the webhook URL is not a valid URL")]
    BadUrl,
    /// The scheme is not `https`.
    #[error("the webhook URL must use https")]
    SchemeNotHttps,
    /// The authority carried `user:pass@` credentials.
    #[error("the webhook URL must not contain credentials")]
    CredentialsInUrl,
    /// There was no host.
    #[error("the webhook URL has no host")]
    MissingHost,
    /// The host resolved to no addresses.
    #[error("the webhook host did not resolve")]
    Unresolved,
    /// The host is, or resolves to, an address that must never be reached.
    #[error("the webhook host resolves to a forbidden address ({0}): {1}")]
    ForbiddenAddress(IpAddr, ForbiddenReason),
}

impl SsrfRejection {
    /// What a caller may be told, which is not everything [`Display`](fmt::Display) says.
    ///
    /// The first four variants describe the string the caller submitted — a bad URL, a wrong
    /// scheme, embedded credentials, a missing host — so repeating them verbatim tells the caller
    /// nothing they did not already know, and telling them precisely is how they fix it.
    ///
    /// The last two are different in kind, and this method exists for them. Both are decided by
    /// **this server's resolver**, not by the caller's string, so their `Display` text is a report
    /// on the cloud's own DNS view: `ForbiddenAddress` names the exact address a hostname resolved
    /// to and its class, and `Unresolved` says the name resolved to nothing. Rendered into a
    /// response those three outcomes — resolves publicly, resolves privately to *this* address,
    /// does not resolve — make the register route an internal name-to-address mapper, one name per
    /// request, and answer the exact question the SSRF block exists to refuse. So they collapse
    /// into one sentence that says what is required without saying which way it failed, and the
    /// detail goes to the log instead ([ADR-0032](../../../../docs/adr/0032-webhooks.md)).
    ///
    /// A caller who typed an IP literal learns nothing new either way, but distinguishing that case
    /// would restore the oracle for anyone who tries a hostname first.
    #[must_use]
    pub const fn caller_message(&self) -> &'static str {
        match self {
            Self::BadUrl => "the webhook URL is not a valid URL",
            Self::SchemeNotHttps => "the webhook URL must use https",
            Self::CredentialsInUrl => "the webhook URL must not contain credentials",
            Self::MissingHost => "the webhook URL has no host",
            Self::Unresolved | Self::ForbiddenAddress(_, _) => {
                "the webhook URL must point at a public address"
            }
        }
    }

    /// The `details` reason that goes with [`caller_message`](Self::caller_message).
    ///
    /// Deliberately next to the message, because the two must not disagree: the whole point of
    /// collapsing `Unresolved` and `ForbiddenAddress` into one sentence is that a caller cannot tell
    /// them apart, and giving them distinct reasons would re-open the oracle the message just
    /// closed. They share `FORBIDDEN_DESTINATION`.
    ///
    /// `INVALID_FORMAT` for the other four: those really are about the shape of the string the
    /// caller sent. `FORBIDDEN_DESTINATION` is not — the URL is well formed and the destination is
    /// refused — so reusing `INVALID_FORMAT` there would tell a client to go and fix a syntax that
    /// is fine.
    #[must_use]
    pub const fn caller_reason(&self) -> &'static str {
        match self {
            Self::BadUrl | Self::SchemeNotHttps | Self::CredentialsInUrl | Self::MissingHost => {
                "INVALID_FORMAT"
            }
            Self::Unresolved | Self::ForbiddenAddress(_, _) => "FORBIDDEN_DESTINATION",
        }
    }
}

/// Which class of never-reachable address a destination hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForbiddenReason {
    /// `0.0.0.0` / `::` — the unspecified address.
    Unspecified,
    /// Loopback (`127.0.0.0/8`, `::1`).
    Loopback,
    /// A private network (`10/8`, `172.16/12`, `192.168/16`, unique-local `fc00::/7`).
    Private,
    /// Link-local (`169.254/16` — the cloud metadata range — or `fe80::/10`).
    LinkLocal,
    /// Carrier-grade NAT shared space (`100.64/10`).
    SharedCgn,
    /// Benchmarking (`198.18/15`).
    Benchmarking,
    /// Documentation ranges (`192.0.2/24`, `198.51.100/24`, `203.0.113/24`, `2001:db8::/32`).
    Documentation,
    /// Reserved or future-use (`240/4`, `255.255.255.255`, and the part of `64:ff9b::/32` that
    /// carries no assigned NAT64 prefix).
    Reserved,
    /// Multicast (`224/4`, `ff00::/8`).
    Multicast,
}

impl fmt::Display for ForbiddenReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Unspecified => "unspecified",
            Self::Loopback => "loopback",
            Self::Private => "private network",
            Self::LinkLocal => "link-local",
            Self::SharedCgn => "carrier-grade NAT",
            Self::Benchmarking => "benchmarking",
            Self::Documentation => "documentation",
            Self::Reserved => "reserved",
            Self::Multicast => "multicast",
        };
        formatter.write_str(text)
    }
}

/// Vets `url` with the real resolver, on the blocking pool.
///
/// [`vet`] is sync because it is pure given a resolver, and the real resolver blocks — so every
/// caller in an async context has to hand it to `spawn_blocking`. One wrapper rather than each
/// caller remembering: the webhook dispatcher re-vets a stored URL before every delivery (a DNS
/// record can be repointed at a private address after registration), and the alert channel vets its
/// configured destination once at boot.
///
/// # Errors
///
/// A human-readable reason, whether the URL was refused or the vetting task failed to join.
pub async fn vet_blocking(url: &str) -> Result<VettedUrl, String> {
    let raw = url.to_owned();
    match tokio::task::spawn_blocking(move || vet(&raw, resolve_host)).await {
        Ok(Ok(vetted)) => Ok(vetted),
        Ok(Err(rejection)) => Err(rejection.to_string()),
        Err(join_error) => Err(format!(
            "the SSRF vetting task failed to join: {join_error}"
        )),
    }
}

/// Vets a webhook destination, resolving its host with `resolve`.
///
/// # Errors
///
/// [`SsrfRejection`] if the URL is structurally unsafe, or the host is or resolves to any forbidden
/// address.
pub fn vet(
    raw: &str,
    resolve: impl Fn(&str) -> std::io::Result<Vec<IpAddr>>,
) -> Result<VettedUrl, SsrfRejection> {
    let parsed = url::Url::parse(raw).map_err(|_| SsrfRejection::BadUrl)?;

    if parsed.scheme() != "https" {
        return Err(SsrfRejection::SchemeNotHttps);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(SsrfRejection::CredentialsInUrl);
    }

    let addresses = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => vec![IpAddr::V4(ip)],
        Some(url::Host::Ipv6(ip)) => vec![IpAddr::V6(ip)],
        Some(url::Host::Domain(host)) => resolve(host).map_err(|_| SsrfRejection::Unresolved)?,
        None => return Err(SsrfRejection::MissingHost),
    };

    if addresses.is_empty() {
        return Err(SsrfRejection::Unresolved);
    }
    for address in &addresses {
        classify_ip(*address)?;
    }
    Ok(VettedUrl {
        url: parsed.to_string(),
        addresses,
    })
}

/// Refuses an address that must never be a webhook target.
///
/// A blocklist rather than an allowlist of exact ranges, so a new global range does not silently
/// become unreachable; the ranges here are the ones an SSRF payload aims at.
///
/// # Errors
///
/// [`SsrfRejection::ForbiddenAddress`] naming the class, if `ip` is not a public unicast address.
pub fn classify_ip(ip: IpAddr) -> Result<(), SsrfRejection> {
    let reason = match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        // An IPv4-mapped v6 address (`::ffff:a.b.c.d`) is really its v4 address; classify it as such
        // so `::ffff:127.0.0.1` cannot smuggle loopback past the v6 checks.
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => classify_v4(v4),
            None => classify_v6(v6),
        },
    };
    match reason {
        Some(reason) => Err(SsrfRejection::ForbiddenAddress(ip, reason)),
        None => Ok(()),
    }
}

/// The forbidden class of an IPv4 address, or `None` if it is public unicast.
///
/// The documentation and benchmarking ranges are checked by hand rather than with
/// `Ipv4Addr::is_documentation`, which is still unstable.
fn classify_v4(ip: Ipv4Addr) -> Option<ForbiddenReason> {
    let [a, b, c, _] = ip.octets();
    let is_documentation = (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113);
    let is_ietf_protocol_or_relay =
        (a == 192 && b == 0 && c == 0) || (a == 192 && b == 88 && c == 99);
    if a == 0 {
        // 0.0.0.0/8 (RFC 1122 "This host on this network", including 0.0.0.0 unspecified).
        Some(ForbiddenReason::Unspecified)
    } else if ip.is_loopback() {
        Some(ForbiddenReason::Loopback)
    } else if ip.is_private() {
        Some(ForbiddenReason::Private)
    } else if ip.is_link_local() {
        // 169.254/16, which includes the 169.254.169.254 cloud metadata endpoint.
        Some(ForbiddenReason::LinkLocal)
    } else if a == 100 && (64..=127).contains(&b) {
        Some(ForbiddenReason::SharedCgn)
    } else if a == 198 && (b == 18 || b == 19) {
        Some(ForbiddenReason::Benchmarking)
    } else if is_documentation {
        Some(ForbiddenReason::Documentation)
    } else if ip.is_broadcast() || a >= 240 || is_ietf_protocol_or_relay {
        // Reserved, future-use, 192.0.0.0/24 IETF Protocol Assignments (RFC 6890), and 192.88.99.0/24 6to4 relay.
        Some(ForbiddenReason::Reserved)
    } else if ip.is_multicast() {
        Some(ForbiddenReason::Multicast)
    } else {
        None
    }
}

/// The forbidden class an address embeds behind a NAT64 prefix, if any.
///
/// `64:ff9b::/32` holds exactly two assigned prefixes, and they do not agree on where the IPv4 sits.
/// The well-known `64:ff9b::/96` (RFC 6052) puts it in the low 32 bits. The local-use `64:ff9b:1::/48`
/// (RFC 8215) is a /48, and RFC 6052 §2.2 splits a /48 prefix's embedded address around the reserved
/// `u` octet at bits 64-71:
///
/// ```text
/// |48|     prefix            |v4(16) | u | (16)  | suffix            |
///  0                        48      64  72      88
/// ```
///
/// so the address is bits 48-63 and 72-87 — not the low 32 bits, and not segment 4 read whole, which
/// takes `u` as an address byte and stops an octet short.
///
/// Each prefix is decoded the way its own length says, and **only** that way. A reading borrowed from
/// the other length does not abstain on a mismatch, it invents an answer: the /48 split reads a /96
/// address's zero padding as `0.0.0.0`, which `classify_v4` refuses as unspecified — so applying it to
/// the well-known prefix would refuse every NAT64 destination there, including the public ones the
/// prefix exists to carry.
///
/// The rest of `64:ff9b::/32` is refused outright rather than decoded. IANA assigns that /32 no
/// translation prefix beyond the two above, and RFC 6052 §3.2 takes a network-specific prefix from the
/// operator's own space, never from here — so nothing in the remainder is a destination anyone can
/// legitimately reach, and there is no prefix length to decode it at. Refusing the block closes both
/// the subnets a decoder would have to enumerate (`64:ff9b:0:1::127.0.0.1`, `64:ff9b:2::127.0.0.1`)
/// and any future length nobody here thought of. The embedded class is still read where it is
/// available, because "private network" tells the operator reading the log more than "reserved" does.
fn classify_nat64(segments: [u16; 8], ip: Ipv6Addr) -> Option<ForbiddenReason> {
    if segments[0] != 0x0064 || segments[1] != 0xff9b {
        return None;
    }
    let [a, b, c, d] = ip.octets()[12..16] else {
        unreachable!()
    };
    let low_32 = Ipv4Addr::new(a, b, c, d);

    // The well-known prefix (`64:ff9b::/96`): a /96, so the low 32 bits are the whole address.
    if segments[2] == 0 && segments[3] == 0 && segments[4] == 0 && segments[5] == 0 {
        return classify_v4(low_32);
    }

    // The local-use prefix (`64:ff9b:1::/48`): bits 48-63 and 72-87, skipping `u`. The low 32 bits are
    // tried too — a malformed address that parks its IPv4 there is still stating an intent.
    if segments[2] == 0x0001 {
        if let Some(reason) = classify_v4(low_32) {
            return Some(reason);
        }
        return classify_v4(Ipv4Addr::new(
            (segments[3] >> 8) as u8,
            (segments[3] & 0xff) as u8,
            (segments[4] & 0xff) as u8,
            (segments[5] >> 8) as u8,
        ));
    }

    // Everything else under `64:ff9b::/32`.
    Some(classify_v4(low_32).unwrap_or(ForbiddenReason::Reserved))
}

/// The forbidden class of an IPv6 address, or `None` if it is public unicast.
fn classify_v6(ip: Ipv6Addr) -> Option<ForbiddenReason> {
    if ip.is_unspecified() {
        return Some(ForbiddenReason::Unspecified);
    }
    if ip.is_loopback() {
        return Some(ForbiddenReason::Loopback);
    }

    let segments = ip.segments();
    // 6over4 / IPv4-compatible interface identifier (`<prefix>:0:0:a.b.c.d`, RFC 2529).
    if segments[4] == 0 && segments[5] == 0 {
        let [a, b, c, d] = ip.octets()[12..16] else {
            unreachable!()
        };
        let is_v4_compat_prefix =
            segments[0] == 0 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0;
        if is_v4_compat_prefix || a != 0 {
            let reason = classify_v4(Ipv4Addr::new(a, b, c, d));
            if reason.is_some() {
                return reason;
            }
        }
    }

    // SIIT IPv4-translated IPv6 address (`<prefix>:ffff:0:a.b.c.d`, RFC 6145).
    if segments[4] == 0xffff && segments[5] == 0 {
        let [a, b, c, d] = ip.octets()[12..16] else {
            unreachable!()
        };
        let is_siit_prefix =
            segments[0] == 0 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0;
        if (is_siit_prefix || a != 0)
            && let Some(reason) = classify_v4(Ipv4Addr::new(a, b, c, d))
        {
            return Some(reason);
        }
    }

    // NAT64, well-known prefix or local-use (RFC 6052, RFC 8215) — its own function because the
    // two prefixes place their embedded address differently and the reason takes explaining.
    if let Some(reason) = classify_nat64(segments, ip) {
        return Some(reason);
    }

    // ISATAP address (`<prefix>:0:5efe:a.b.c.d` or `<prefix>:200:5efe:a.b.c.d`, RFC 5214).
    if (segments[4] == 0 || segments[4] == 0x0200) && segments[5] == 0x5efe {
        let [a, b, c, d] = ip.octets()[12..16] else {
            unreachable!()
        };
        if let Some(reason) = classify_v4(Ipv4Addr::new(a, b, c, d)) {
            return Some(reason);
        }
    }

    let [first, second, third, ..] = segments;

    // Teredo IPv6 prefix (`2001:0000::/32`, RFC 4380): embeds Server IPv4 and inverted Client IPv4.
    if first == 0x2001 && second == 0x0000 {
        let fourth = segments[3];
        let server_v4 = Ipv4Addr::new(
            (third >> 8) as u8,
            (third & 0xff) as u8,
            (fourth >> 8) as u8,
            (fourth & 0xff) as u8,
        );
        if let Some(reason) = classify_v4(server_v4) {
            return Some(reason);
        }
        let octets = ip.octets();
        let client_v4 = Ipv4Addr::new(!octets[12], !octets[13], !octets[14], !octets[15]);
        return classify_v4(client_v4);
    }

    // 6to4 IPv6 prefix (`2002:WWXX:YYZZ::`): embeds IPv4 address WW.XX.YY.ZZ.
    if first == 0x2002 {
        let a = (second >> 8) as u8;
        let b = (second & 0xff) as u8;
        let c = (third >> 8) as u8;
        let d = (third & 0xff) as u8;
        return classify_v4(Ipv4Addr::new(a, b, c, d));
    }

    if ip.is_multicast() {
        Some(ForbiddenReason::Multicast)
    } else if first & 0xfe00 == 0xfc00 {
        // fc00::/7 unique-local — the v6 private network.
        Some(ForbiddenReason::Private)
    } else if first & 0xffc0 == 0xfe80 {
        // fe80::/10 link-local.
        Some(ForbiddenReason::LinkLocal)
    } else if first == 0x2001 && second == 0x0db8 {
        // 2001:db8::/32 documentation.
        Some(ForbiddenReason::Documentation)
    } else if first == 0x2001 && second == 0x0002 {
        // 2001:2::/48 benchmarking (RFC 5180).
        Some(ForbiddenReason::Benchmarking)
    } else {
        None
    }
}

/// Resolves `host` to its IP addresses via the OS resolver (`getaddrinfo`) — the real resolver
/// [`vet`] takes in production, at both registration and each delivery.
///
/// It blocks, so callers run it on a blocking pool (`tokio::task::spawn_blocking`). The port is
/// immaterial — [`vet`] uses only the addresses — so an arbitrary one (443) stands in.
///
/// # Errors
///
/// Any `std::io::Error` the OS resolver returns (an unknown host, a resolver outage).
pub(crate) fn resolve_host(host: &str) -> std::io::Result<Vec<IpAddr>> {
    use std::net::ToSocketAddrs as _;
    Ok((host, 443_u16)
        .to_socket_addrs()?
        .map(|address| address.ip())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{ForbiddenReason, SsrfRejection, classify_ip, vet};

    use std::net::IpAddr;

    fn resolves_to(addr: &'static str) -> impl Fn(&str) -> std::io::Result<Vec<IpAddr>> {
        move |_host| Ok(vec![addr.parse().expect("a valid test address")])
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("a valid address")
    }

    #[test]
    fn the_resolvers_answer_never_reaches_the_caller() {
        // The whole point: `Display` is for the log and carries the address; `caller_message` is
        // for the response and must not. A registration route that echoed this turned itself into
        // an internal name-to-address mapper, one name per request.
        let refused = vet("https://internal.example/hook", resolves_to("10.4.12.7"))
            .expect_err("a private address is refused");
        let logged = refused.to_string();
        assert!(logged.contains("10.4.12.7"), "the log keeps it: {logged}");

        let told = refused.caller_message();
        assert!(!told.contains("10.4.12.7"), "the caller does not: {told}");
        assert!(
            !told.contains("private"),
            "nor the class, which narrows the address on its own: {told}"
        );
    }

    #[test]
    fn a_name_that_does_not_resolve_is_indistinguishable_from_one_that_resolves_inward() {
        // Three outcomes exist — resolves publicly, resolves to a forbidden address, does not
        // resolve — and the last two must not be tellable apart, or the caller still learns whether
        // a name exists in this server's DNS view.
        let unresolved = SsrfRejection::Unresolved;
        let forbidden =
            SsrfRejection::ForbiddenAddress(ip("169.254.169.254"), ForbiddenReason::LinkLocal);
        assert_eq!(unresolved.caller_message(), forbidden.caller_message());
    }

    #[test]
    fn the_reason_does_not_reopen_what_the_message_closed() {
        // Collapsing the two resolver-decided variants into one sentence buys nothing if the
        // machine-readable half tells them apart again.
        let unresolved = SsrfRejection::Unresolved;
        let forbidden = SsrfRejection::ForbiddenAddress(ip("10.0.0.1"), ForbiddenReason::Private);
        assert_eq!(unresolved.caller_reason(), forbidden.caller_reason());
        assert_eq!(unresolved.caller_reason(), "FORBIDDEN_DESTINATION");
        // And the four caller-string variants keep a reason that means what it says.
        assert_eq!(
            SsrfRejection::SchemeNotHttps.caller_reason(),
            "INVALID_FORMAT"
        );
    }

    #[test]
    fn a_refusal_about_the_callers_own_string_stays_precise() {
        // The other four describe what the caller typed, so saying exactly what is wrong tells them
        // nothing they did not already know and is how they fix it. Coarsening these would cost
        // usability and buy no secrecy.
        assert_eq!(
            SsrfRejection::SchemeNotHttps.caller_message(),
            "the webhook URL must use https"
        );
        assert_eq!(
            SsrfRejection::CredentialsInUrl.caller_message(),
            "the webhook URL must not contain credentials"
        );
        assert_eq!(
            SsrfRejection::BadUrl.caller_message(),
            SsrfRejection::BadUrl.to_string()
        );
        assert_eq!(
            SsrfRejection::MissingHost.caller_message(),
            SsrfRejection::MissingHost.to_string()
        );
    }

    #[test]
    fn public_addresses_are_allowed() {
        assert_eq!(classify_ip(ip("93.184.216.34")), Ok(()));
        assert_eq!(classify_ip(ip("8.8.8.8")), Ok(()));
        assert_eq!(
            classify_ip(ip("2606:2800:220:1:248:1893:25c8:1946")),
            Ok(())
        );
    }

    #[test]
    fn the_metadata_and_loopback_and_private_ranges_are_refused() {
        // The classics an SSRF payload reaches for.
        for (address, reason) in [
            ("169.254.169.254", ForbiddenReason::LinkLocal),
            ("127.0.0.1", ForbiddenReason::Loopback),
            ("10.0.0.5", ForbiddenReason::Private),
            ("172.16.9.9", ForbiddenReason::Private),
            ("192.168.1.1", ForbiddenReason::Private),
            ("0.0.0.0", ForbiddenReason::Unspecified),
            ("0.0.0.1", ForbiddenReason::Unspecified),
            ("0.255.255.255", ForbiddenReason::Unspecified),
            ("100.64.0.1", ForbiddenReason::SharedCgn),
            ("198.18.0.1", ForbiddenReason::Benchmarking),
            ("192.0.0.1", ForbiddenReason::Reserved),
            ("192.88.99.1", ForbiddenReason::Reserved),
            ("255.255.255.255", ForbiddenReason::Reserved),
            ("224.0.0.1", ForbiddenReason::Multicast),
        ] {
            assert_eq!(
                classify_ip(ip(address)),
                Err(SsrfRejection::ForbiddenAddress(ip(address), reason)),
                "{address} must be refused"
            );
        }
    }

    #[test]
    fn v6_loopback_ula_and_mapped_v4_are_refused() {
        assert_eq!(
            classify_ip(ip("::1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("::1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("fc00::1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("fc00::1"),
                ForbiddenReason::Private
            ))
        );
        assert_eq!(
            classify_ip(ip("fe80::1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("fe80::1"),
                ForbiddenReason::LinkLocal
            ))
        );
        // Local NAT64 subnet prefix smuggling cases (`64:ff9b:1:<subnet>::a.b.c.d`).
        assert_eq!(
            classify_ip(ip("64:ff9b:1:1::127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("64:ff9b:1:1::127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("64:ff9b:1:abcd::169.254.169.254")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("64:ff9b:1:abcd::169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
        // The smuggling case: loopback wearing a v6 coat.
        assert_eq!(
            classify_ip(ip("::ffff:127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("::ffff:127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        // IPv4-compatible IPv6 smuggling cases (`::a.b.c.d`).
        assert_eq!(
            classify_ip(ip("::127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("::127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("::169.254.169.254")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("::169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
        // Teredo smuggling cases (`2001:0::/32`).
        assert_eq!(
            classify_ip(ip("2001:0:7f00:1::")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2001:0:7f00:1::"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("2001:0:1234:5678:0:0:80ff:fffe")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2001:0:1234:5678:0:0:80ff:fffe"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("2001:0:1234:5678:0:0:5601:5601")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2001:0:1234:5678:0:0:5601:5601"),
                ForbiddenReason::LinkLocal
            ))
        );
    }

    /// A NAT64 **local-use** address embeds its IPv4 where RFC 6052 says, not in the low 32 bits.
    ///
    /// Its own test because it is its own claim, and because the address below is the only kind that
    /// can prove it — see the comment inside.
    #[test]
    fn a_nat64_local_use_prefix_embeds_its_address_around_the_reserved_octet() {
        // The NAT64 **local-use** prefix is a /48, and RFC 6052 §2.2 splits a /48's embedded address
        // around the reserved `u` octet — bits 48-63 and 72-87, not the low 32 bits.
        //
        // This address is chosen to tell the two readings apart, which most do not: the private
        // ranges are decided by their first byte or two, so a wrong reading of bytes 2 and 3 usually
        // lands in the same forbidden /8 anyway and hides the bug. `192.0.2.0/24` is documentation
        // and needs **byte 2** to decide, so:
        //
        //   * bits 48-63 and 72-87 read 192.0.2.5 — documentation, refused;
        //   * segment 4 read whole (the `u` octet as an address byte) reads 192.0.0.2 — public;
        //   * the low 32 bits read 8.8.8.8 — public.
        //
        // Only the RFC's own split refuses it.
        assert_eq!(
            classify_ip(ip("64:ff9b:1:c000:2:500:808:808")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("64:ff9b:1:c000:2:500:808:808"),
                ForbiddenReason::Documentation
            )),
            "a /48 NAT64 address embeds its IPv4 around the reserved u octet, not in the low 32 bits"
        );
        // And the same prefix carrying a genuinely public address is still allowed: every embedded
        // reading here only ever adds a rejection, never removes one.
        assert_eq!(classify_ip(ip("64:ff9b:1:808:0:808:808:808")), Ok(()));
    }

    /// `64:ff9b::/32` carries two assigned prefixes; the rest of it is refused rather than decoded.
    ///
    /// Its own test because the two halves pull against each other, and a fix that only watches one
    /// half breaks the other.
    #[test]
    fn an_unassigned_nat64_subnet_is_refused_and_the_assigned_ones_still_decode() {
        // The gap. A subnet that is neither the well-known `::/96` nor the local-use `:1::/48` matched
        // no branch, so it fell out of `classify_v6` as ordinary public unicast and `64:ff9b:dead::10.0.0.5`
        // reached the private network.
        for (address, reason) in [
            ("64:ff9b:0:1::127.0.0.1", ForbiddenReason::Loopback),
            ("64:ff9b:2::127.0.0.1", ForbiddenReason::Loopback),
            ("64:ff9b:0:1::169.254.169.254", ForbiddenReason::LinkLocal),
            ("64:ff9b:dead::10.0.0.5", ForbiddenReason::Private),
            // With no assigned prefix there is no length to read the address at, so a public-looking
            // embedded figure buys nothing: the block is refused whole. `Reserved` is what is left to
            // say when the bytes decode to nothing forbidden.
            ("64:ff9b:0:1::8.8.8.8", ForbiddenReason::Reserved),
            ("64:ff9b:ffff::8.8.8.8", ForbiddenReason::Reserved),
        ] {
            assert_eq!(
                classify_ip(ip(address)),
                Err(SsrfRejection::ForbiddenAddress(ip(address), reason)),
                "{address} sits in no assigned NAT64 prefix and must be refused"
            );
        }

        // The other half, and the one a wider rule costs. The well-known prefix is a /96: its address
        // is the low 32 bits and the zeros in segments 3-5 are padding, not an address. Read that
        // padding with the /48 split — the reading the *local-use* prefix needs — and it becomes
        // `0.0.0.0`, which `classify_v4` refuses as unspecified. A decoder widened across the whole
        // /32 therefore refuses every well-known-prefix destination, including the public ones the
        // prefix exists to carry, and fails closed so quietly that no existing test notices.
        assert_eq!(classify_ip(ip("64:ff9b::8.8.8.8")), Ok(()));
        assert_eq!(classify_ip(ip("64:ff9b::93.184.216.34")), Ok(()));
    }

    #[test]
    fn v6_translation_prefixes_are_refused() {
        // NAT64 well-known prefix smuggling cases (`64:ff9b::a.b.c.d`).
        assert_eq!(
            classify_ip(ip("64:ff9b::127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("64:ff9b::127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("64:ff9b::169.254.169.254")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("64:ff9b::169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
        // 6to4 prefix smuggling cases (`2002:WWXX:YYZZ::`).
        assert_eq!(
            classify_ip(ip("2002:7f00:0001::")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2002:7f00:0001::"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("2002:a9fe:a9fe::")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2002:a9fe:a9fe::"),
                ForbiddenReason::LinkLocal
            ))
        );
        // SIIT IPv4-translated smuggling cases (`<prefix>:ffff:0:a.b.c.d`).
        assert_eq!(
            classify_ip(ip("::ffff:0:127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("::ffff:0:127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("::ffff:0:169.254.169.254")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("::ffff:0:169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
        assert_eq!(
            classify_ip(ip("2001:db8::ffff:0:127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2001:db8::ffff:0:127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("2001:1234:5678:9abc:ffff:0:169.254.169.254")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2001:1234:5678:9abc:ffff:0:169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
        // Local NAT64 prefix smuggling cases (`64:ff9b:1::a.b.c.d`).
        assert_eq!(
            classify_ip(ip("64:ff9b:1::127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("64:ff9b:1::127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("64:ff9b:1::169.254.169.254")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("64:ff9b:1::169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
        assert_eq!(
            classify_ip(ip("64:ff9b:1:0:1::127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("64:ff9b:1:0:1::127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
    }

    #[test]
    fn v6_tunneling_and_benchmarking_prefixes_are_refused() {
        // 6over4 / IPv4-compatible smuggling cases (`<prefix>:0:0:a.b.c.d`).
        assert_eq!(
            classify_ip(ip("2001:1234:5678:9abc::127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2001:1234:5678:9abc::127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("2606:2800:220:1::169.254.169.254")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2606:2800:220:1::169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
        // ISATAP smuggling cases (`::5efe:a.b.c.d` and with arbitrary prefixes).
        assert_eq!(
            classify_ip(ip("::5efe:127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("::5efe:127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("::5efe:169.254.169.254")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("::5efe:169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
        assert_eq!(
            classify_ip(ip("2001:db8::5efe:127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2001:db8::5efe:127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
        assert_eq!(
            classify_ip(ip("2001:1234:5678:9abc:0:5efe:169.254.169.254")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2001:1234:5678:9abc:0:5efe:169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
        // Benchmarking IPv6 range (2001:2::/48).
        assert_eq!(
            classify_ip(ip("2001:2::1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("2001:2::1"),
                ForbiddenReason::Benchmarking
            ))
        );
    }

    #[test]
    fn a_public_https_url_vets() {
        let vetted = vet(
            "https://hooks.example.com/pos",
            resolves_to("93.184.216.34"),
        )
        .expect("a public https endpoint is allowed");
        assert_eq!(vetted.addresses, vec![ip("93.184.216.34")]);
    }

    #[test]
    fn non_https_schemes_are_refused() {
        assert_eq!(
            vet("http://hooks.example.com", resolves_to("93.184.216.34")),
            Err(SsrfRejection::SchemeNotHttps),
            "plaintext http is refused"
        );
        assert_eq!(
            vet("file:///etc/passwd", resolves_to("93.184.216.34")),
            Err(SsrfRejection::SchemeNotHttps)
        );
    }

    #[test]
    fn credentials_in_the_url_are_refused() {
        assert_eq!(
            vet(
                "https://user:pass@hooks.example.com",
                resolves_to("93.184.216.34")
            ),
            Err(SsrfRejection::CredentialsInUrl)
        );
    }

    #[test]
    fn a_hostname_resolving_to_a_forbidden_address_is_refused_whole() {
        // The DNS-based SSRF: a public-looking name that points inward.
        let outcome = vet("https://sneaky.example.com", resolves_to("169.254.169.254"));
        assert_eq!(
            outcome,
            Err(SsrfRejection::ForbiddenAddress(
                ip("169.254.169.254"),
                ForbiddenReason::LinkLocal
            ))
        );
    }

    #[test]
    fn an_ip_literal_host_is_classified_without_resolving() {
        // No resolver call should be needed, and loopback is still caught.
        let outcome = vet("https://127.0.0.1/hook", |_| {
            panic!("an IP-literal host must not be resolved")
        });
        assert_eq!(
            outcome,
            Err(SsrfRejection::ForbiddenAddress(
                ip("127.0.0.1"),
                ForbiddenReason::Loopback
            ))
        );
    }

    #[test]
    fn a_host_that_does_not_resolve_is_refused() {
        assert_eq!(
            vet("https://nope.example.com", |_| Ok(vec![])),
            Err(SsrfRejection::Unresolved)
        );
    }
}
