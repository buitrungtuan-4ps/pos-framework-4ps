# ADR-0004 — All configuration lives in the cloud

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** Configuration edited on store machines drifts, cannot be audited, and cannot be reproduced when hardware is replaced.

**Decision.** Menu, pricing, tax, devices, printers, roles, capability flags and locale live in a versioned Tenant → Brand → Store tree in the cloud. Stores receive versioned deltas and hot-reload in under a second. Locally discovered hardware (printers, terminals) is *proposed* to the cloud and only becomes active once an administrator assigns a role. Employee PINs sync as hashes so login works offline. Invalid configuration is rejected; the last known-good version stays in force.

**Consequences.**
- Machine replacement restores identical behaviour with no manual setup.
- Anything a store genuinely must set alone (network address for pairing) is an explicit, narrow exception.
- The configuration tree becomes a critical dependency: its validation, versioning and rollback must be first-class.
