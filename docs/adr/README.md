# Architecture Decision Records

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-02

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
| [0033](0033-config-tree.md) | The four-level config tree: deep-merged layers, RFC 7386 merge-patch deltas, cloud-side validation, K-bounded snapshots | Accepted |
| [0034](0034-super-admin-auth.md) | Super-admin auth: Argon2id password + mandatory RFC 6238 TOTP (SHA256), no-oracle two-factor, host-only `__Host-` session cookie | Accepted |
| [0035](0035-retention-and-pii-masking.md) | Retention is enforced by masking the subject store (not deleting it), on a configured period; idempotent daily sweep; rights requests stay escalated | Accepted |
| [0036](0036-materialised-rollups.md) | Dashboards answer from a materialised rollup maintained by a projector cursor (each event folded once, one shared fold); the read takes no `EventStore`, so it never scans the log | Accepted |
| [0037](0037-api-keys.md) | Scoped per-tenant API keys: `pos_<id>_<secret>` bearer tokens, SHA-256-hashed (not Argon2), tenant-bound and deny-by-default by scope, revocable and shown once | Accepted |
| [0020](0020-i18n-runtime.md) | i18n runtime: ICU MessageFormat over the platform `Intl`, `en` the enforced fallback | Accepted |
| [0016](0016-postgres-access.md) | Cloud PostgreSQL access: `tokio-postgres` behind a pool, SQL by hand, RLS per transaction | Accepted |
| [0022](0022-events-partition-strategy.md) | Events partitioned monthly by business date; tenant isolation by RLS, not by the partition key | Accepted |
| [0023](0023-tenant-hostname-and-slug.md) | Flat per-tenant subdomains; DNS is the slug-uniqueness ledger; redirect never proxy | Accepted |
| [0019](0019-openapi-generation.md) | OpenAPI generated from the handlers with `utoipa`; a CI drift check fails on divergence | Accepted |
| [0038](0038-webhook-tls-sender.md) | The webhook TLS sender reuses the tree's rustls stack, and owns its dial | Accepted |
| [0039](0039-config-delivery.md) | Config reaches the store by authenticated pull on a store-facing `/sync` surface | Accepted |
| [0040](0040-reconciliation.md) | Reconciliation is an edge-initiated missing-id diff on the internal surface | Accepted |
| [0041](0041-device-onboarding.md) | Device onboarding is discover → propose → admin-approves, over a proposal table | Accepted |
| [0042](0042-image-pipeline.md) | The image pipeline buys `image`, re-encodes to JPEG, and fits a byte budget by ladder | Accepted |
| [0043](0043-translation-grid.md) | The translation grid: one jsonb per tenant, `en` required as the fallback | Accepted |
| [0044](0044-fork-and-deploy.md) | Fork-and-deploy: one VPS, Docker Compose, secrets generated on the server | Accepted |
| [0045](0045-first-boot-admin-enrolment.md) | First-boot super-admin enrolment, and the reset break-glass | Accepted |
| [0046](0046-backups-and-restore.md) | Cloud backups and the restore drill | Accepted |
| [0047](0047-minisign-verification.md) | Minisign update verification: `ed25519-dalek` + `blake2`, verify-only | Accepted |
| [0048](0048-ota-rollout-model.md) | OTA rollout: rings, canary, self-test rollback, and a kill switch, as one pure decision | Accepted |
| [0049](0049-single-active-lease.md) | The single-active lease: generation-based, offline-durable, with a disjoint invoice range | Accepted |
| [0050](0050-activation-code-exchange.md) | Activation-code exchange: single-use, locally checkable, credential into the vault | Accepted |
| [0051](0051-device-credential-provisioning.md) | Device-credential provisioning: the cloud activation exchange | Accepted |
| [0052](0052-ota-rollout-config.md) | The OTA rollout is published as configuration, validated by shared rules | Accepted |
| [0053](0053-cloud-sync-port.md) | CloudSync: the store's request/response channel to the cloud (the seventeenth port) | Accepted |
| [0054](0054-edge-cloud-http-client.md) | The edge→cloud HTTP client reuses the tree's rustls stack, behind a transport seam | Accepted |
| [0055](0055-edge-ota-updater.md) | The edge OTA updater orchestrates behind an install seam; the OS steps are gated | Accepted |
| [0056](0056-public-order-intake.md) | Public order intake: `POST /v1/orders` over the OrderIn port, tenant-bound via a StoreDirectory seam | Accepted |
| [0057](0057-qr-ordering.md) | QR ordering: an HMAC-signed `table_id` and a pure guardrail decision, over the public intake | Accepted |
| [0058](0058-shipping-adapters.md) | Shipping adapters: the `ShippingDispatch` port over a REST courier API, behind a transport seam | Accepted |
| [0059](0059-erp-adapter.md) | ERP adapter: the `ErpSink` port over a REST posting API, behind a transport seam | Accepted |
| [0060](0060-cloud-back-office-dashboard.md) | Cloud back-office: an embedded SolidJS SPA served by `pos_cloud` over the existing admin API | Accepted |
| [0061](0061-order-relay.md) | Order relay: a durable per-store queue the store pulls; the cloud implements `OrderIn` over it | Accepted |
| [0063](0063-store-menu-catalog.md) | Store menu catalog: the store's authoritative price book, synced as config; `pos-core` reprices inbound lines from it | Accepted |
| [0064](0064-edge-order-in.md) | Edge `OrderIn`: the store reprices from its menu, opens a tableless order in its local log, and dedupes on the caller's reference | Accepted |
| [0065](0065-cloud-org-registry.md) | The cloud org registry: named Tenant/Brand/Store/Device, RLS by tenant, backfilled from config_trees; identity and naming distinct from configuration | Accepted |
| [0066](0066-cloud-catalog.md) | The cloud catalog: a normalized 12-entity authoring model (items, menus with inheritance, channel price lists, tax classes, display taxonomy, layouts) compiled per store×channel to a flat `MenuBook`/`DisplayPlan` and pushed via the config tree | Accepted |

| [0067](0067-multi-admin-console-rbac.md) | Multi-admin console identities with role-based access | Accepted |
| [0068](0068-fleet-liveness.md) | Fleet liveness: last-seen + config-version-held from the store pull | Accepted |
| [0069](0069-audit-trail.md) | Console audit trail: an append-only record of who changed what | Accepted |
| [0070](0070-people-and-access.md) | People & access: employees, store assignments, role templates, and the permissions a store enforces | Accepted |
| [0071](0071-config-without-json.md) | Config without JSON: a form-driven capability editor, and an edge that applies the structured nodes | Accepted |
| [0072](0072-floor-and-kitchen.md) | Floor & kitchen: areas/tables and stations as published master data the edge reads | Accepted |
| [0073](0073-alerting.md) | Alerting: server-side detection, storage, and delivery of operational conditions | Accepted |
| [0074](0074-localization-and-tax.md) | Localization & tax: authoring tax rates the edge already knows how to apply, and surfacing countries, locale packs, and store timezone as master data | Accepted |
| [0075](0075-media-and-file-rail.md) | Media & file rail: images in Postgres `bytea`, and a CSV import/export rail with dry-run validation | Accepted |
| [0076](0076-subject-request-tooling.md) | Subject-request tooling: per-subject PDPD/GDPR lookup, export, and erasure | Accepted |
| [0077](0077-campaigns-and-scheduling.md) | Campaigns & scheduling: authoring promotions over the finished engine, and publishing them (and any config) on a future date | Accepted |
| [0078](0078-sync-and-ota-closure.md) | Sync & OTA closure: the cloud learns what each store is running, and gets first-class levers instead of hand-edited JSON | Accepted |
| [0079](0079-inventory-and-suppliers.md) | Inventory & suppliers: author recipes and stock thresholds in the cloud, so the finished §8 engine finally has inputs | Accepted |
| [0080](0080-channels-and-payments.md) | Channels & payments: author per-store channel enablement, accepted tender, QR guardrails, and vendor policy as config nodes | Accepted |
| [0081](0081-reports-and-analytics.md) | Reports & analytics: windowed rollups, revenue & product-mix, and X/Z close semantics, on a registry-driven projector | Accepted |
| [0082](0082-catalog-and-layout-rebuild.md) | Catalog & Layout rebuild: split the monolith into kit sub-screens, make Layout a visual grid | Accepted |
| [0083](0083-integration-doctrine.md) | Integration doctrine: the core stays small, everything else plugs in through three points | Accepted |
| [0084](0084-device-authentication.md) | Device authentication: the edge enforces the pairing token on every domain route | Accepted |
| [0085](0085-edge-cloud-sync-transport.md) | The edge dials its cloud: config-pull and heartbeat over the tree's rustls stack, keyed by the store's scoped credential | Accepted |
| [0086](0086-edge-keyvault-and-activation.md) | The edge's OS-keyring KeyVault, and composing activation into the shipped binary | Accepted |
| [0087](0087-edge-relay-and-event-publish.md) | Wiring the store's two outbound rails: the order relay, and edge event publish | Accepted |
| [0088](0088-ota-artifact-hosting.md) | The cloud hosts the update artifact, and stays a dumb host | Accepted |
| [0089](0089-edge-event-bus-transport.md) | The edge reaches the event bus directly, over TLS on its own port | Accepted |
| [0090](0090-tls-postures.md) | TLS termination is a fork-level posture, chosen explicitly | Accepted |
| [0091](0091-durable-edge-auth-state.md) | Edge auth state is durable: a `DeviceRegistry` port, hashed tokens, and an idle timeout | Accepted |
| [0092](0092-artifact-trust-chain.md) | The edge cannot fetch an artifact without its signature, and its trusted keys come only from the build | Accepted |
| [0093](0093-bill-keyed-on-order.md) | A bill belongs to an order, not to a table | Accepted |
| [0094](0094-console-optimistic-concurrency.md) | The console stops losing edits: an opaque version at the seam, Postgres `xmin` beneath it | Accepted |
| [0095](0095-conditional-writes-for-collections.md) | What ADR-0094 left: three shapes, not one, and only one of them is hard | Accepted |
| [0096](0096-unprocessable-status.md) | A twelfth status, because nine refusals cannot say what is wrong with them | Accepted |
| [0097](0097-internal-route-authentication.md) | The `/internal` routes get a key of their own, and now is the only cheap time to do it | Accepted |
| [0098](0098-paged-admin-reads.md) | Paging is a second read, not a change to the read that exists | Accepted |
| [0099](0099-store-hub.md) | The console's landing page answers "is this shop all right", not "how much did it make" | Accepted |
| [0100](0100-receipt-and-ticket-printing.md) | A receipt and a kitchen ticket are documents the store composes, and the printer only carries them | Accepted |
| [0101](0101-the-cloud-stamps-the-tenant.md) | The cloud stamps the tenant, because the store cannot be trusted to name one | Accepted |
| [0102](0102-printing-any-script.md) | A store draws the lines its printer's code page cannot spell | Accepted |
| [0103](0103-directly-attached-printers.md) | A printer on a cable is a transport, not a second driver | Accepted |
| [0104](0104-multi-component-and-inclusive-tax.md) | A tax rate is a list, and a price may already contain it | Accepted |
| [0105](0105-a-country-pack-is-values.md) | A country pack is a list of values, and none of them are in the framework | Accepted |
| [0106](0106-the-store-is-a-legal-person.md) | A receipt names who sold, and the store's identity is data | Accepted |

**When a new ADR is required:** changing a port or wire protocol, adding a third-party dependency or infrastructure component, changing a security or data-retention boundary, or reversing any record above.
