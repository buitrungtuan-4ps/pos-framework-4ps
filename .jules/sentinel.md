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
