# Architecture Decision Records

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

Each record states the context, the decision, and the consequences we accept. Records are immutable once merged: to change a decision, add a new record that supersedes the old one.

| ID | Decision | Status |
|---|---|---|
| [0001](0001-offline-first-store-autonomy.md) | The store sells without the cloud | Accepted |
| [0002](0002-one-binary-per-tier.md) | One binary per tier (modular monolith) | Accepted |
| [0003](0003-cattle-not-pets.md) | Machines are replaceable; activation codes and leases | Accepted |
| [0004](0004-cloud-owned-configuration.md) | All configuration lives in the cloud | Accepted |
| [0005](0005-country-neutral-core.md) | Country-neutral core, fiscalization plug-ins | Accepted |
| [0006](0006-ports-and-adapters.md) | Own the boundary, not the implementation | Superseded by 0021 |
| [0007](0007-in-house-vs-dependency.md) | What we write ourselves and what we do not | Accepted |
| [0008](0008-postgres-partitioning.md) | Partitioned PostgreSQL with RLS, not database-per-store | Accepted |
| [0009](0009-licence.md) | Licence: proprietary, internal use | Accepted |
| [0010](0010-naming-standard.md) | snake_case everywhere; deviations from Google AIP | Accepted |
| [0011](0011-country-in-hostname.md) | Country lives in the hostname; redirect, never proxy | Accepted |
| [0012](0012-qr-ordering-via-cloud.md) | QR ordering is a cloud module reusing `OrderIn` | Accepted |
| [0013](0013-async-strategy.md) | Sans-I/O domain core; `pos-core` and `pos-ports` as siblings; async ports | Accepted |
| [0014](0014-datetime-library.md) | Date, time, and timezone library | Accepted |
| [0021](0021-corrected-port-list.md) | The sixteen ports, superseding 0006 | Accepted |
| [0024](0024-protocol-version-negotiation.md) | `PROTOCOL_VERSION` negotiation | Accepted |
| [0026](0026-port-shapes.md) | Port shapes: one failure type, one transaction handle, three corrections to 0013 | Accepted |
| [0027](0027-country-modules.md) | Country modules are bundles at `countries/<cc>/`, selected by Cargo feature | Accepted |
| [0025](0025-receipt-number-authority.md) | Receipt number gapless only while one store authority is reachable; authority is configuration | Accepted |
| [0028](0028-settlement-and-payment-invariant.md) | What "payments sum to the bill" means; tendered vs applied, tips a separate ledger, explicit rounding | Accepted |
| [0029](0029-append-command-merge-semantics.md) | Line merge: terminal states win, other fields last-writer-wins on (event_time, device_id) | Accepted |
| [0015](0015-sqlite-access.md) | SQLite at the edge: `rusqlite` behind one single-writer thread | Accepted |
| [0017](0017-migrations.md) | Migrations: forward-only, additive, enforced by an `xtask` gate | Accepted |
| [0018](0018-http-websocket-stack.md) | Edge HTTP/WebSocket stack: axum, a broadcast fan-out, an embedded UI | Accepted |
| [0030](0030-pairing-and-offline-auth.md) | Edge discovery, pairing, and offline device & user authentication | Accepted |
| [0031](0031-cloud-adapter-transports.md) | Cloud adapter transports: async-nats for the link, hand-rolled S3 and VictoriaMetrics HTTP | Accepted |
| [0032](0032-webhooks.md) | Webhooks: a signed, SSRF-guarded cursor over the event log, with a per-endpoint circuit breaker | Accepted |
| [0020](0020-i18n-runtime.md) | i18n runtime: ICU MessageFormat over the platform `Intl`, `en` the enforced fallback | Accepted |
| [0016](0016-postgres-access.md) | Cloud PostgreSQL access: `tokio-postgres` behind a pool, SQL by hand, RLS per transaction | Accepted |
| [0022](0022-events-partition-strategy.md) | Events partitioned monthly by business date; tenant isolation by RLS, not by the partition key | Accepted |
| [0023](0023-tenant-hostname-and-slug.md) | Flat per-tenant subdomains; DNS is the slug-uniqueness ledger; redirect never proxy | Accepted |
| [0019](0019-openapi-generation.md) | OpenAPI generated from the handlers with `utoipa`; a CI drift check fails on divergence | Accepted |

**When a new ADR is required:** changing a port or wire protocol, adding a third-party dependency or infrastructure component, changing a security or data-retention boundary, or reversing any record above.
