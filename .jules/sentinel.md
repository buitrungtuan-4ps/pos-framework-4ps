## 2026-09-15 - [SSRF IPv6 Translation Bypass]
**Vulnerability:** Webhook destinations could potentially bypass SSRF IPv6 checks when hostnames/IPs were provided using IPv6 translation prefixes such as NAT64 (`64:ff9b::/96`) or 6to4 (`2002::/16`), allowing embedded private/loopback IPv4 addresses (like `127.0.0.1` or `169.254.169.254`) to fall through unclassified.
**Learning:** IPv6 address classification must account for all mechanisms that embed IPv4 addresses (such as IPv4-mapped, IPv4-compatible, NAT64, and 6to4 prefixes), translating the embedded IPv4 bytes back into IPv4 classification logic before deciding if a destination address is safe.
**Prevention:** When classifying IPv6 addresses for SSRF/network safety, explicitly check and extract embedded IPv4 addresses for well-known translation prefixes (`64:ff9b::`, `2002::`, `::ffff:`, `::`) and evaluate them through `classify_v4`.

## 2026-09-21 - [ISATAP and NAT64 Subnet SSRF Bypass]
**Vulnerability:** ISATAP interface identifiers (`:0:5efe:a.b.c.d` and `:200:5efe:a.b.c.d`) were only checked when paired with a zero IPv6 prefix (`::/64`), allowing attackers to attach any non-zero 64-bit prefix to bypass SSRF validation while embedding loopback/link-local IPv4 addresses. Similarly, NAT64 local-use subnets (`64:ff9b:1::/48`) were strictly requiring zeroed intermediate segments.
**Learning:** IPv6 transition and tunneling mechanisms like ISATAP embed IPv4 addresses in the interface ID portion (the last 64 bits), regardless of the routing prefix in the upper 64 bits.
**Prevention:** Inspect ISATAP interface identifiers (`segments[5] == 0x5efe` with `segments[4] == 0 || 0x0200`) across all 64-bit IPv6 prefixes and classify the embedded IPv4 address.

## 2026-09-22 - [SSRF IPv4 0.0.0.0/8 Bypass via Standard Unspecified Check]
**Vulnerability:** Webhook SSRF validation relied on `Ipv4Addr::is_unspecified()`, which only matches `0.0.0.0`. Other IPv4 addresses within RFC 1122 `0.0.0.0/8` ("This host on this network", e.g., `0.0.0.1` or `0.1.2.3`) fell through as public unicast addresses.
**Learning:** `Ipv4Addr::is_unspecified()` in Rust std checks solely for `0.0.0.0`. However, operating systems and socket libraries often route/bind `0.0.0.0/8` IP addresses to localhost/loopback or local services.
**Prevention:** When filtering IPv4 addresses for SSRF/safety, check the first octet `a == 0` to block the entire `0.0.0.0/8` range rather than checking `is_unspecified()` alone.

## 2026-09-21 - [NAT64 /48 embeds its IPv4 around the reserved u octet]
**Vulnerability:** The `64:ff9b:1::/48` local-use branch added for the ISATAP fix read its embedded IPv4 from segment 4 whole, so `64:ff9b:1:c000:2:500:808:808` — which embeds the documentation address 192.0.2.5 — classified as public and passed the SSRF filter.
**Learning:** RFC 6052 §2.2 does not place a prefix's embedded IPv4 in the low 32 bits for anything shorter than a /96. A /48 splits it around the reserved `u` octet at bits 64-71: the address is bits 48-63 and 72-87. Most wrong readings are invisible in tests because the private ranges are decided by their first byte or two, so bytes 2 and 3 can be read wrongly and still land in the same forbidden /8.
**Prevention:** When testing an address-extraction fix, choose a range that needs the *late* bytes to decide — `192.0.2.0/24` rather than `127.0.0.0/8` — so a passing test cannot be passing by luck.

## 2026-09-23 - [NAT64 subnets outside the two assigned prefixes were never classified]
**Vulnerability:** `classify_nat64` recognised only `64:ff9b::/96` and `64:ff9b:1::/48`, so any other subnet of `64:ff9b::/32` — `64:ff9b:0:1::127.0.0.1`, `64:ff9b:2::127.0.0.1`, `64:ff9b:dead::10.0.0.5` — matched no branch, fell out of `classify_v6` as ordinary public unicast, and was accepted as a webhook destination.
**Learning:** The two RFCs assign two prefixes, not the whole `64:ff9b::/32`; RFC 6052 §3.2 takes a network-specific prefix from the operator's own space, never from there. So the remainder has no legitimate destination *and* no prefix length to decode one at, and the fix is to refuse the block rather than to widen the decoder. Widening it is actively worse: an extraction borrowed from the wrong prefix length does not abstain on a mismatch, it invents an answer. Running the /48 split over a /96 address reads its zero padding as `0.0.0.0`, which `classify_v4` refuses as unspecified — so a decoder stretched across the /32 refuses every well-known-prefix destination, including the public ones NAT64 exists to carry.
**Prevention:** When a check is widened to cover a gap, test the range it *already* covered for a false positive — an address that must still be allowed — not only the gap. A filter that fails closed breaks quietly, and a suite that only asserts refusals cannot see it.

## 2026-09-24 - [SSRF IPv4 IETF Protocol Assignments 192.0.0.0/24 and 192.88.99.0/24 Bypass]
**Vulnerability:** Webhook SSRF validation did not check RFC 6890 special-purpose IPv4 ranges `192.0.0.0/24` (IETF Protocol Assignments / DS-Lite) and `192.88.99.0/24` (deprecated 6to4 Anycast Relay). Addresses such as `192.0.0.1` (local DS-Lite tunnel interface) fell through `classify_v4` as public unicast addresses.
**Learning:** Standard library checks like `is_private()` or `is_loopback()` do not cover all RFC 6890 non-globally-routable special-purpose IPv4 blocks. `192.0.0.0/24` and `192.88.99.0/24` carry protocol assignments and local gateways that must never be targeted by outbound webhooks.
**Prevention:** Explicitly match octets `a == 192 && b == 0 && c == 0` (`192.0.0.0/24`) and `a == 192 && b == 88 && c == 99` (`192.88.99.0/24`) alongside standard reserved ranges in `classify_v4`.

## 2026-09-24 - [6over4 Subnet SSRF Bypass]
**Vulnerability:** Webhook destinations could bypass SSRF IPv6 checks when hostnames/IPs were provided using 6over4 / IPv4-compatible interface identifiers (`0:0:a.b.c.d`) attached to arbitrary 64-bit IPv6 prefixes (such as `2001:1234:5678:9abc::127.0.0.1` or `2606:2800:220:1::169.254.169.254`), allowing embedded private/loopback IPv4 addresses to fall through unclassified as public unicast.
**Learning:** 6over4 / IPv4-compatible interface identifiers embed an IPv4 address in the low 32 bits (`segments[4] == 0 && segments[5] == 0`), regardless of the routing prefix in the upper 64 bits. However, when inspecting non-zero 64-bit prefixes, low-byte zero padding (`a == 0` in `a.b.c.d`) indicates a standard IPv6 host suffix rather than an embedded 6over4 address, and must be skipped to avoid false positives.
**Prevention:** Inspect interface identifiers with `segments[4] == 0 && segments[5] == 0` across all IPv6 prefixes, evaluating `a.b.c.d` through `classify_v4` when `a != 0` or when `segments[0..4]` is all-zero.
