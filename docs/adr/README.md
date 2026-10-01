# Architecture Decision Records

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-01

Each record states the context, the decision, and the consequences we accept. Records are immutable once merged: to change a decision, add a new record that supersedes the old one.

A record's status says where its decision stands. **Proposed**: it waits on a decision it names, by the owner, accounting or legal. **Accepted**: the owner approved it. A record the owner approves before it merges is merged as Accepted; a Proposed record is marked Accepted when the decision it waits on is made, and says when. **Superseded**: a later record replaced it. Marking its status is the only edit a merged record takes.

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
| [0062](0062-the-relay-wake.md) | The relay wakes its waiters instead of polling: no live cloud→store channel, `MessageLink` stays one-directional, and an idle store stops costing ten queue queries a second | Accepted |
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
| [0107](0107-the-buyer-is-a-subject.md) | The buyer on a tax invoice is a subject, not a string — and a store gets the twentieth port to keep them in | Accepted |
| [0108](0108-the-lease-generation-is-authority.md) | The lease generation is authority, not configuration, and a box takes it once | Accepted |
| [0109](0109-counting-the-taps-an-operator-makes.md) | The step budget counts taps in a browser: a handler that exists is not a tap a thumb can reach | Accepted |
| [0110](0110-edge-placement-is-a-deployment-axis.md) | Edge placement is a deployment axis, and the lease is what moves along it | Accepted |
| [0111](0111-a-second-origin-may-address-the-edge.md) | A second origin may address the edge, from a list the cloud publishes | Accepted |
| [0112](0112-print-agents.md) | A paired device may own a printer's transport, and the edge still renders every byte | Accepted |
| [0113](0113-the-host-agent.md) | The host agent pulls jobs and starts one container per store, and it never holds a store's credential | Accepted |
| [0114](0114-region-is-required-recorded-visible.md) | Region is a required, recorded, visible attribute of every hosted edge placement | Accepted |
| [0115](0115-reason-codes-are-a-managed-list.md) | Reason codes are a managed list the cloud publishes, and absence is not a brick | Accepted |
| [0116](0116-the-qr-hold-is-derived-and-it-gates-firing.md) | The staff-confirmation hold is derived state, and it gates firing | Accepted |
| [0117](0117-a-headless-store-keeps-a-log.md) | A headless store keeps a log, and hands over its pairing code without logging it | Accepted |
| [0118](0118-one-credential-per-box-and-the-cloud-learns.md) | One credential per box, and the cloud learns what a store admitted | Accepted |
| [0119](0119-each-admin-signs-in-as-themselves.md) | Each admin signs in as themselves | Accepted |
| [0120](0120-navigation-preserves-the-working-context.md) | An absent `?store=` is silence, not a denial | Accepted |
| [0121](0121-one-way-to-author-an-entity.md) | One way to author an entity: a shell and a lifecycle | Accepted |
| [0122](0122-a-store-group-is-a-delivery-cohort.md) | A store group is a delivery cohort, and a batch publish reports every store | Accepted |
| [0123](0123-a-superseded-box-opens-nothing-new.md) | A superseded box opens nothing new, and finishes everything it holds | Accepted |
| [0124](0124-a-store-that-can-be-restored.md) | A store that can be restored: the shop seals its own archive | Accepted |
| [0125](0125-a-release-is-one-decision-many-writes.md) | A release is one decision and many writes: nodes × stores, scheduled in each store's own clock | Accepted |
| [0126](0126-when-an-agent-may-merge.md) | When an agent may merge: the repo owner's exception, and what it does not cover | Accepted |
| [0127](0127-modifier-groups-reach-the-edge.md) | Modifier groups reach the edge: the compiled book carries the rule, and the edge enforces it | Accepted |
| [0128](0128-a-bill-splits-and-merges.md) | A bill splits and merges: a bill covers lines, and the parts partition them exactly | Accepted |
| [0129](0129-a-receipt-itemises-what-was-sold.md) | A receipt itemises what was sold: name, quantity, unit price and amount per line | Accepted |
| [0130](0130-a-course-is-something-the-catalog-names.md) | A course is something the catalog names: the entity every `course_id` in the tree already points at | Accepted |
| [0131](0131-a-chained-event-log.md) | The event log chains, and the cloud holds the anchor: tamper-evidence for a log that is append-only only by convention | Accepted |
| [0132](0132-the-cloud-recomputes-the-chain-it-holds.md) | The cloud recomputes the chain from the events it holds, at the anchor — what closes truncation, which the anchor alone does not | Accepted |
| [0133](0133-the-backbone-defines-what-is-hashed-not-how.md) | `sha2` stays out of the backbone allow-list; `pos-proto` owns the preimage-digest-wrap sequence and takes the digest as an argument | Accepted |
| [0134](0134-a-currency-says-how-many-decimals-it-has.md) | A currency says how many decimals it has, and a missing answer is not zero: the exponent is a published country value, not a three-row table in the front end | Accepted |
| [0135](0135-the-console-reads-money-the-way-the-till-does.md) | The console reads and writes money the way the till does: the same published exponent, on the surface where prices are authored | Accepted |
| [0136](0136-a-store-publishes-how-it-writes-numbers.md) | A store publishes how it writes numbers, and each surface knows whose reading it serves: the till and the paper draw the store's, the console draws the reader's (Amendment 1 — a menu belongs to a tenant, so only a store's own settings use its marks) | Accepted |
| [0137](0137-a-deep-outbox-warns-and-never-refuses.md) | A deep outbox warns and never refuses a sale: the disk is the bound, the status bar says "offline — selling normally", and a refusal carries a translatable reason | Accepted |
| [0138](0138-the-edge-compresses-what-it-sends.md) | The edge compresses what it sends to a device: gzip on `/api/*` and the embedded assets, negotiated, above a size floor, never on `/ws` | Accepted |
| [0139](0139-the-till-draws-the-pairing-code-as-a-qr.md) | The till draws the pairing link as a QR code: `qrcode-generator` in the till, loaded only by the screens that show a link | Accepted |
| [0140](0140-a-store-pc-installs-itself-from-one-file.md) | A store PC installs itself from one file: `pos-edge install` carries the tested script, reads the store and cloud from its own file name, asks for elevation, and activation restarts the box | Accepted |
| [0141](0141-the-console-hands-out-the-installer-by-name.md) | The console hands out the installer by name: a cloud route serves the hosted Windows release as `pos-edge-setup_<cloud>_<store>.exe`, and the setup window asks for the store key | Accepted |
| [0142](0142-windows-signing-is-the-forks-choice.md) | Windows code signing is the fork's choice: one script reads `none`, `pfx` or a hardware `command` from the environment, runs before minisign, and says in the build which it did | Accepted |
| [0143](0143-the-device-credential-syncs-and-events-travel-over-https.md) | One credential per box: the device credential authenticates `/sync`, archiving the device revokes it, and events travel over an HTTPS route that binds them to the publishing store; NATS stays for deployments that run it | Accepted |
| [0144](0144-a-line-records-the-names-of-its-modifiers.md) | A line records the names of its modifiers beside their ids, so the receipt prints what the guest agreed to; an older line prints none | Accepted |
| [0145](0145-the-edge-keeps-events-until-synced-and-n-days-old.md) | The edge keeps an event until it is synced **and** N days old and nothing open depends on it; N is per store from the cloud (default 90), and a chain checkpoint keeps what is left verifiable | Accepted |
| [0146](0146-a-counter-store-starts-its-own-orders.md) | A counter store starts its own orders at the till: `POST /api/orders` and an order-keyed add that prices at the order's own channel | Accepted |
| [0147](0147-pos-station-is-a-tauri-shell-over-the-edge.md) | POS Station is a Tauri v2 shell around the edge's own UI: outside the workspace, same-origin with the edge, native pairing into the keychain, Station and Terminal modes | Accepted |
| [0148](0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md) | An unclaimed box shows a code and the console claims it (RFC 8628): one generic image, a person picks the store, the box collects its credential once | Accepted |
| [0149](0149-a-replacement-box-numbers-above-what-the-cloud-has-seen.md) | A spare box takes over by hand, and every lease bump publishes a receipt floor so a replacement never reuses a number the cloud has seen | Accepted |
| [0150](0150-the-appliance-is-a-linux-image-that-claims-itself.md) | The appliance is a stock Linux install turned into a store box by one script, generic until claimed; Android stays a spike until it runs on a device | Accepted |
| [0151](0151-a-headless-linux-box-seals-its-secrets-with-systemd-creds.md) | A headless Linux box seals its secrets under a vault key systemd-creds keeps (TPM2 or host key), so an activation survives a reboot; a key that cannot be read is a vault error, never the keyring | Accepted |
| [0152](0152-a-receipt-number-belongs-to-a-series-and-each-country-sets-its-rules.md) | A receipt number is a series and a gapless sequence; a new lease generation opens a new series; each country module sets the printed format, restarts and voids; the cloud reports gaps | Proposed |
| [0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md) | A vendor is a provider in a catalogue: its adapter runs where the vendor is (LAN device on the edge, internet service on the cloud), a tenant configures a connection from a schema the adapter describes, and secrets never leave the cloud | Proposed |
| [0154](0154-a-card-is-taken-through-the-stores-terminal.md) | A card is taken through the store's terminal: the edge drives a `card.<vendor>` driver the `integrations` node names, the amount is typed once, and an unknown answer parks the bill until `look_up`, `void` or a manager resolves it | Proposed |
| [0155](0155-a-qr-payment-is-confirmed-by-its-gateway-through-the-cloud.md) | A QR payment is confirmed by its gateway through the cloud: a `QrGateway` port, a `qr.<vendor>` connector holds the credentials and receives the webhook, the confirmation is relayed to the store, and hand confirmation stays the offline path | Proposed |
| [0156](0156-a-store-issues-its-e-invoice-and-the-cloud-submits-it.md) | A store issues its e-invoice from a range the cloud allocated, in the sale's transaction, as `fiscal.invoice.issued`; the cloud's `einvoice.<vendor>` connector submits from the event log and reconciles daily | Proposed |
| [0157](0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md) | A guest note is held in the edge's memory for the service, bounded, and never written to disk, the log or telemetry; the kitchen ticket, the live orders and the `/ws` frame carry it, and a note lost to a restart is shown as lost | Proposed |
| [0158](0158-the-till-enforces-each-persons-own-permissions.md) | The till enforces each signed-in person's own permissions, deny by default: every state-changing route names its permission, a person may hold several roles, approval is set per role, and a per-store switch rolls it out | Proposed |
| [0159](0159-a-fee-is-configuration.md) | A fee is configuration: a published list of rules, each a rate or an amount, taxed or not, by channel and by item; the core computes them, a bill keeps the rules it opened with, and events and receipts itemise them | Proposed |
| [0160](0160-everything-a-store-runs-differently-is-published-configuration.md) | Everything a store runs differently is typed, defaulted configuration, authored at tenant, brand, group or store scope and per device; every console switch is honoured or hidden; one generated register of settings | Proposed |
| [0161](0161-a-paid-order-without-a-table-stays-on-the-kitchen-board-until-it-is-done.md) | A paid order without a table stays on the kitchen board and the pass until a station bumps it, it is voided, or its business day ends; its guest note stays with it, amending ADR-0157's drop rule; `GET /api/orders/kitchen` serves what the kitchen still has to make | Accepted |
| [0162](0162-a-bill-is-taxed-at-the-rates-of-one-stated-moment.md) | A bill is taxed at the rates of one stated moment: frozen when it opens by default, or at settlement where the country pack's `tax_point` says so; the settled bill records the rates it used. Awaits accounting's reading of the Vietnamese tax point | Proposed |
| [0163](0163-a-table-seated-by-mistake-is-released.md) | A table seated by mistake is released, `OCCUPIED → FREE`, only while nothing is sold on it, and a bill is never opened on an order with nothing to bill | Accepted |
| [0164](0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md) | A receipt is reprinted as a copy marked COPY under its own number, for today's settled bills, under `billing.receipt.reprint`; every reprint is a `billing.receipt.reprinted` event, so the dashboard can count it per employee; a copy never prints a figure the settle did not record | Accepted |
| [0165](0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) | Cash paid in and out is recorded from the shift screen under `cash.movement.record` and counted in the expected drawer; a drawer opened outside a sale needs a manager's PIN and a reason; a drawer opens only through a USB printer the console marks **Cash drawer attached**, published as the device's additive `drawer_attached` field | Accepted |

**When a new ADR is required:** changing a port or wire protocol, adding a third-party dependency or infrastructure component, changing a security or data-retention boundary, or reversing any record above.
