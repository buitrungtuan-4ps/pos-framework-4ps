// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The SSRF filter's tests, beside it rather than inside it so its file stays under the size limit.

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
        classify_ip(ip("fec0::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("fec0::1"),
            ForbiddenReason::Private
        ))
    );
    assert_eq!(
        classify_ip(ip("fec0:0:0:1::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("fec0:0:0:1::1"),
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
    // IPv4-mapped interface identifier smuggling cases (`<prefix>:0:ffff:a.b.c.d` or `<prefix>:ffff:ffff:a.b.c.d`).
    assert_eq!(
        classify_ip(ip("2001:db8::ffff:127.0.0.1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("2001:db8::ffff:127.0.0.1"),
            ForbiddenReason::Loopback
        ))
    );
    assert_eq!(
        classify_ip(ip("2001:1234:5678:9abc:ffff:ffff:169.254.169.254")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("2001:1234:5678:9abc:ffff:ffff:169.254.169.254"),
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
fn v6_documentation_prefixes_are_refused() {
    // IPv6 Documentation ranges (2001:db8::/32 and 3fff::/20 RFC 9637).
    assert_eq!(
        classify_ip(ip("2001:db8::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("2001:db8::1"),
            ForbiddenReason::Documentation
        ))
    );
    assert_eq!(
        classify_ip(ip("3fff::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("3fff::1"),
            ForbiddenReason::Documentation
        ))
    );
    assert_eq!(
        classify_ip(ip("3fff:0fff:ffff::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("3fff:0fff:ffff::1"),
            ForbiddenReason::Documentation
        ))
    );
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
fn v6_tunneling_prefixes_are_refused() {
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
}

#[test]
fn v6_benchmarking_and_special_purpose_prefixes_are_refused() {
    // Benchmarking IPv6 range (2001:2::/48).
    assert_eq!(
        classify_ip(ip("2001:2::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("2001:2::1"),
            ForbiddenReason::Benchmarking
        ))
    );
    // Discard-Only IPv6 range (100::/64, RFC 6666).
    assert_eq!(
        classify_ip(ip("100::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("100::1"),
            ForbiddenReason::Reserved
        ))
    );
    assert_eq!(
        classify_ip(ip("100::dead:beef")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("100::dead:beef"),
            ForbiddenReason::Reserved
        ))
    );
    // PCP and TURN anycast IPv6 range (2001:1::/32, RFC 7723 / RFC 8155).
    assert_eq!(
        classify_ip(ip("2001:1::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("2001:1::1"),
            ForbiddenReason::Reserved
        ))
    );
    // ORCHIDv1 (2001:10::/28, RFC 4843) and ORCHIDv2 (2001:20::/28, RFC 7343) IPv6 ranges.
    assert_eq!(
        classify_ip(ip("2001:10::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("2001:10::1"),
            ForbiddenReason::Reserved
        ))
    );
    assert_eq!(
        classify_ip(ip("2001:20::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("2001:20::1"),
            ForbiddenReason::Reserved
        ))
    );
    // AMT (2001:3::/32, RFC 7450) and AS112 (2001:4:112::/48, RFC 7535) IPv6 ranges.
    assert_eq!(
        classify_ip(ip("2001:3::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("2001:3::1"),
            ForbiddenReason::Reserved
        ))
    );
    assert_eq!(
        classify_ip(ip("2001:4:112::1")),
        Err(SsrfRejection::ForbiddenAddress(
            ip("2001:4:112::1"),
            ForbiddenReason::Reserved
        ))
    );
}

#[test]
fn iana_s_special_purpose_ipv4_anycast_blocks_are_reserved() {
    // AMT (192.52.193.0/24, RFC 7450), AS112-v4 (192.31.196.0/24, RFC 7535) and Direct Delegation
    // AS112 (192.175.48.0/24, RFC 7534): globally reachable, but no webhook receiver lives there.
    for address in ["192.52.193.1", "192.31.196.1", "192.175.48.1"] {
        assert_eq!(
            classify_ip(ip(address)),
            Err(SsrfRejection::ForbiddenAddress(
                ip(address),
                ForbiddenReason::Reserved
            )),
            "{address}"
        );
    }
    // The addresses either side of each /24 stay public.
    for address in [
        "192.52.192.255",
        "192.52.194.0",
        "192.31.195.255",
        "192.31.197.0",
        "192.175.47.255",
        "192.175.49.0",
    ] {
        assert_eq!(classify_ip(ip(address)), Ok(()), "{address}");
    }
}

#[test]
fn the_rest_of_iana_s_special_purpose_ipv6_blocks_are_reserved() {
    // Direct Delegation AS112 (2620:4f:8000::/48, RFC 7534), LISP EID space (2001:5::/32, RFC 7954)
    // and DRIP Entity Tags (2001:30::/28, RFC 9374).
    for address in ["2620:4f:8000::1", "2001:5::1", "2001:3f::1"] {
        assert_eq!(
            classify_ip(ip(address)),
            Err(SsrfRejection::ForbiddenAddress(
                ip(address),
                ForbiddenReason::Reserved
            )),
            "{address}"
        );
    }
    // The neighbours stay public: the /48 and the /28 end where IANA says they do.
    for address in ["2620:4f:8001::1", "2001:40::1"] {
        assert_eq!(classify_ip(ip(address)), Ok(()), "{address}");
    }
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
