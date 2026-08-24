# ADR-0030 — Edge discovery, pairing, and offline device & user authentication

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-19
**Relates to** [ADR-0001](0001-offline-first-store-autonomy.md) · [ADR-0018](0018-http-websocket-stack.md) · [ADR-0003](0003-cattle-not-pets.md) · [ADR-0013](0013-async-strategy.md)

**Context.** Before a device can sell, two things must happen, and both must work with the network
cable unplugged ([ADR-0001](0001-offline-first-store-autonomy.md)):

1. **the device must find the edge** on the store LAN, and
2. **a device and a user must authenticate** to it.

The field constraints are specific and unforgiving. Chrome on Android does not resolve mDNS, so
`pos.local` cannot be the only way in. An in-browser camera (for scanning a QR code) requires a secure
context, so a bare `http://` origin will not open one on some clients. The store's Wi-Fi access point
can die — an intra-store partition ([ADR-0025](0025-receipt-number-authority.md) met the same
enemy) — so discovery cannot depend on any one mechanism. And the cloud is regularly unreachable, so
user authentication cannot call home.

Two authentications are in play and must not be conflated: **which devices** may talk to the edge
(device pairing) and **which employee** is acting (user PIN). They have different lifetimes and
different secrets.

**Options considered.**

1. **mDNS only.** Rejected: it does not resolve on the most common guest hardware (Android Chrome),
   and it is one AP failure from useless. It is a convenience, not a foundation.
2. **A cloud-brokered pairing handshake.** Rejected outright: it violates [ADR-0001](0001-offline-first-store-autonomy.md).
   Pairing a new tablet during an internet outage is exactly when a store needs it to work.
3. **Long-lived device certificates minted by the cloud.** A good end state, but it needs the cloud at
   enrolment and a PKI the framework does not yet have. Deferred.
4. **Always-works fallbacks (a raw-IP QR link and manual `IP:port` + a 6-digit code) with mDNS as a
   pure convenience behind a trait; a single-use pairing code for devices; and offline PIN against
   cloud-synced Argon2id hashes with a local lockout for users.** Chosen.

**Decision.**

*Discovery has one path that always works and one that is a convenience.* The always-works path is a
**QR code carrying a raw-IP URL** (`http://192.168.1.42:8787/pair?code=NNNNNN`) plus **manual
`IP:port` entry** on every client, because those need no name resolution at all. A **DHCP reservation**
pins the edge's IP so the URL stays valid. **mDNS advertising of `pos.local` is a convenience behind an
`Advertiser` trait**, whose real multicast implementation lands with hardware bring-up (roadmap A5) —
exactly as the printer's real transports do ([`printer-escpos`](../../crates/adapters/printer-escpos/src/lib.rs)):
multicast cannot be exercised in CI, so the framework ships the trait and a no-op default, and the
selling paths never depend on it. No mDNS dependency enters the framework now.

*Device pairing is a short-lived, single-use 6-digit code.* The edge mints a 6-digit code from a
vetted CSPRNG (`getrandom`), shows it (on the edge console and in the pairing QR), and a device that
presents a valid code is issued a device token. The code **expires** (five minutes) and is **single
use**, so a shoulder-surfed code is worth little. This is device-level trust — which tablets may reach
the edge — and is distinct from who is using the tablet.

*User authentication is a PIN verified offline against cloud-synced hashes, with a local lockout.* The
cloud syncs each employee's **Argon2id** PIN hash to the edge as configuration
([ADR-0004](0004-cloud-owned-configuration.md)); the edge verifies a PIN against the synced hash with
`argon2`, entirely offline. **Five consecutive failures for an employee lock that employee out for five
minutes**, enforced locally in `pos_edge` (not the cloud, which may be unreachable). Because a PIN is
short, the Argon2id cost plus the lockout — not PIN entropy — is the brute-force defence.

*Where the logic lives.* Discovery and pairing are `pos_edge`'s (the binary composes them). The PIN
lockout is a small state machine over `(employee, clock)` and is pure and unit-tested; PIN
verification calls `argon2`. None of this is in `pos-core`: authentication is an edge session concern,
not country-neutral sell-side domain, and `pos-core` performs no I/O
([ADR-0013](0013-async-strategy.md)). The clock arrives through `ClockSource`, so the five-minute
window is testable without waiting five minutes.

**Data protection.** A device token and an employee id are **identifiers, not PII**, and are the only
things logged. A **PIN and its hash are secrets** and never enter a log, a span, an event payload, or
the fan-out. The pairing code is a short-lived secret and is likewise never logged. This is the no-PII,
no-secrets rule of [`telemetry`](../../crates/pos-edge/src/telemetry.rs) applied to authentication.

**Dependencies added**, at the binary layer only (the dependency-rule test keeps them out of the
backbone): `getrandom` (the OS CSPRNG, for the pairing code and the ULID `IdGenerator`'s randomness)
and `argon2` (PIN hashing). No mDNS crate yet — that arrives with the real `Advertiser` at hardware
bring-up.

**Consequences.**

- A tablet is paired and a cashier signs in during a total internet outage, which is the whole point.
- The failure modes are bounded and visible: a partitioned device still reaches the edge by raw IP; a
  locked-out employee sees a countdown rather than a silent rejection; mDNS being absent degrades to
  "type the IP", never to "cannot sell".
- The real mDNS advertiser and cloud-minted device certificates are deferred, and the code is
  structured (a trait, a synced-hash input) so they slot in without reshaping the callers.
- Cost: an operator may have to read an IP off the edge's console once per device. That is the price of
  not depending on name resolution that is not there when it matters.
