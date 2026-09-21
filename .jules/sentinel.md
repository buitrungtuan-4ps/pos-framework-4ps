## 2026-09-15 - [SSRF IPv6 Translation Bypass]
**Vulnerability:** Webhook destinations could potentially bypass SSRF IPv6 checks when hostnames/IPs were provided using IPv6 translation prefixes such as NAT64 (`64:ff9b::/96`) or 6to4 (`2002::/16`), allowing embedded private/loopback IPv4 addresses (like `127.0.0.1` or `169.254.169.254`) to fall through unclassified.
**Learning:** IPv6 address classification must account for all mechanisms that embed IPv4 addresses (such as IPv4-mapped, IPv4-compatible, NAT64, and 6to4 prefixes), translating the embedded IPv4 bytes back into IPv4 classification logic before deciding if a destination address is safe.
**Prevention:** When classifying IPv6 addresses for SSRF/network safety, explicitly check and extract embedded IPv4 addresses for well-known translation prefixes (`64:ff9b::`, `2002::`, `::ffff:`, `::`) and evaluate them through `classify_v4`.

## 2026-09-21 - [ISATAP and NAT64 Subnet SSRF Bypass]
**Vulnerability:** ISATAP interface identifiers (`:0:5efe:a.b.c.d` and `:200:5efe:a.b.c.d`) were only checked when paired with a zero IPv6 prefix (`::/64`), allowing attackers to attach any non-zero 64-bit prefix to bypass SSRF validation while embedding loopback/link-local IPv4 addresses. Similarly, NAT64 local-use subnets (`64:ff9b:1::/48`) were strictly requiring zeroed intermediate segments.
**Learning:** IPv6 transition and tunneling mechanisms like ISATAP embed IPv4 addresses in the interface ID portion (the last 64 bits), regardless of the routing prefix in the upper 64 bits.
**Prevention:** Inspect ISATAP interface identifiers (`segments[5] == 0x5efe` with `segments[4] == 0 || 0x0200`) across all 64-bit IPv6 prefixes and classify the embedded IPv4 address.
