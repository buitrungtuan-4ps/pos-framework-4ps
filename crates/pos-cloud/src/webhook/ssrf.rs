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
    /// Reserved or future-use (`240/4`, `255.255.255.255`).
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
    if ip.is_unspecified() {
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
    } else if ip.is_broadcast() || a >= 240 {
        Some(ForbiddenReason::Reserved)
    } else if ip.is_multicast() {
        Some(ForbiddenReason::Multicast)
    } else {
        None
    }
}

/// The forbidden class of an IPv6 address, or `None` if it is public unicast.
fn classify_v6(ip: Ipv6Addr) -> Option<ForbiddenReason> {
    let [first, second, ..] = ip.segments();
    if ip.is_unspecified() {
        Some(ForbiddenReason::Unspecified)
    } else if ip.is_loopback() {
        Some(ForbiddenReason::Loopback)
    } else if ip.is_multicast() {
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
            ("100.64.0.1", ForbiddenReason::SharedCgn),
            ("198.18.0.1", ForbiddenReason::Benchmarking),
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
        // The smuggling case: loopback wearing a v6 coat.
        assert_eq!(
            classify_ip(ip("::ffff:127.0.0.1")),
            Err(SsrfRejection::ForbiddenAddress(
                ip("::ffff:127.0.0.1"),
                ForbiddenReason::Loopback
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
