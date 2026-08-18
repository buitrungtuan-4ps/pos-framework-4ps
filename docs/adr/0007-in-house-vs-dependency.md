# ADR-0007 — What we write ourselves

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** Independence is a stated goal, but rewriting mature infrastructure trades proven correctness for effort with no user-visible gain.

**Decision.** Write it ourselves when the component exists only to speak a generic protocol we control both ends of; keep the dependency when it is already far beyond our load and semantically correct.

*Worth writing:* WAL shipping (removes Litestream **and** the S3 server, drops RPO below one second, removes the last AGPL components) · the metrics dashboard (removes Grafana) · small formats (ULID, TOTP, signature parsing, SNTP, mDNS) · direct serial port access.
*Not worth writing:* NATS (we use under 1% of its capacity; the gain is zero and the failure mode is silent data loss) · tokio, hyper/Axum · SolidJS · SQLite, PostgreSQL, TLS and cryptographic primitives — bugs there do not show up in tests.

**Consequences.**
- After the four in-house items, the stack is free of AGPL and MPL.
- We keep a dependency we could replace (NATS) as a conscious choice, with `MessageLink` as the escape hatch.
- Cryptography is always a library; we may implement formats around it, never the primitives.
