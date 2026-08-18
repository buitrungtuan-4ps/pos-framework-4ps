# ADR-0002 — One binary per tier

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** The system spans up to a thousand unattended in-store machines and a single small cloud host. Every additional process is something to install, monitor, upgrade and debug remotely.

**Decision.** Exactly two binaries: `pos_edge` (Windows/Linux service, embedded SolidJS UI via `rust-embed`) and `pos_cloud` (modular monolith containing API, auth, integration hub and fleet management). Infrastructure processes are limited to PostgreSQL, NATS and object storage; monitoring is an optional compose profile.

**Consequences.**
- Deployment, upgrade and rollback are single-artifact operations; OTA becomes tractable.
- Internal module boundaries must be enforced by discipline (crate boundaries, ports) rather than by network separation.
- Horizontal scaling of the cloud is not available without work; capacity analysis shows a single host is sufficient far beyond current targets.
