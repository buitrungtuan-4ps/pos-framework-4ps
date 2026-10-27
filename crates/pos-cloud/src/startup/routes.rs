// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The sub-routers `run` merges onto the cloud's own router, one function per area, in the order
//! the flat chain merged them. Each registers exactly what the chain did; axum matches a request
//! by its path and method whichever router registered it, and every layer is applied by `run`
//! over the whole, so grouping them changes no route, no precedence and no layer.

use std::sync::Arc;

use axum::Router;
use blob_garage::S3Blobs;
use pos_cloud::anchor::AnchorLedger;
use pos_cloud::audit::AuditRecorder;
use pos_cloud::auth::rate_limit::SlidingRateLimiter;
use pos_cloud::cell::signed::CellDirectory;
use pos_cloud::clock::SystemClock;
use pos_cloud::http::SyncThrottle;
use pos_cloud::qr::TableTokenSecret;
use pos_cloud::relay::OrderRelay;
use pos_cloud::release_source::GithubReleases;
use pos_cloud::starting_roles::RoleSeeder;
use pos_cloud::wake::SharedWake;
use pos_cloud::{CloudConfig, assets, countries, http, orders, qr_http, relay};
use pos_providers::Registry;
use store_postgres::PostgresStore;

use super::App;

/// What the sub-routers are built from: the store's views, the configuration, the audit recorder,
/// and the handles several of them share — one anchor ledger, one vendor catalogue, one orders
/// limiter and one relay wake, so every route that reads or counts against one of them agrees.
#[derive(Clone, Copy)]
pub(crate) struct RouteDeps<'a> {
    pub(crate) store: &'a PostgresStore,
    pub(crate) config: &'a CloudConfig,
    pub(crate) audit: &'a Arc<dyn AuditRecorder>,
    /// The anchor ledger, shared: `Cloud` writes findings into it at ingest and the chain router
    /// reads them back for the console. One handle, so the screen shows what the ingest actually
    /// recorded.
    pub(crate) chain_anchors: &'a Arc<dyn AnchorLedger>,
    pub(crate) providers: &'a Arc<Registry>,
    pub(crate) artifacts: Option<&'a S3Blobs>,
    pub(crate) release_source: Option<&'a GithubReleases>,
    pub(crate) expected_tasks: &'a [String],
    pub(crate) orders_limiter: &'a SlidingRateLimiter,
    /// One wake shared by both halves of the relay: `OrderRelay`'s parked `submit` waits on it and
    /// the store-facing router raises it, so an order reaches a long-polling store, and the store's
    /// ack reaches the parked caller, without either side re-reading PostgreSQL on a timer
    /// ([ADR-0062](../../../../docs/adr/0062-the-relay-wake.md)). It must be ONE instance — two
    /// would each signal an audience the other is not listening to, and every wait would fall back
    /// to its timer.
    pub(crate) relay_wake: &'a SharedWake,
    /// Which cell this cloud is, resolved once at boot (ADR-0179 C1), and the directory it answers
    /// from (C2). One holder, so every route answers from the directory the reload last accepted.
    pub(crate) cell: &'a Arc<CellDirectory>,
}

/// Every sub-router the cloud serves beside its own, merged onto it in the order the flat chain
/// merged them. The reconciliation diff (ADR-0040), device-onboarding (ADR-0041), translation-grid
/// (ADR-0043) and activation-exchange (ADR-0050/0051) endpoints carry their own state, so they are
/// merged in rather than threaded through `CloudApp`.
pub(crate) fn api_routes(app: App, deps: &RouteDeps<'_>) -> Router {
    http::router(app)
        .merge(store_sync_routes(deps))
        .merge(ota_artifact_routes(deps))
        .merge(store_archive_routes(deps))
        .merge(device_routes(deps))
        .merge(store_group_routes(deps))
        .merge(config_release_routes(deps))
        .merge(people_routes(deps))
        .merge(floor_routes(deps))
        .merge(fleet_routes(deps))
        .merge(integration_routes(deps))
        .merge(settings_routes(deps))
        .merge(device_revoke_and_health_routes(deps))
        .merge(catalog_routes(deps))
        .merge(catalog_publish_routes(deps))
        .merge(config_node_routes(deps))
        .merge(locale_routes(deps))
        .merge(order_routes(deps))
}

/// What a store reaches beside its own sync: the chain the console reads, reconciliation, and OTA
/// reports.
fn store_sync_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps {
        store,
        config,
        chain_anchors,
        ..
    } = *deps;
    Router::new()
        .merge(http::chain_router(
            Arc::clone(chain_anchors),
            store.admin(),
            SystemClock,
        ))
        .merge(http::reconcile_router(
            store.reconcile(),
            store.admin(),
            SystemClock,
            config.internal_shared_secret.clone(),
            // The store-facing reconcile route authenticates the box by its scoped key, exactly as
            // the config pull and the OTA report beside it do — a store is off the cloud's private
            // network, so `/internal` was never reachable from one (production-readiness R3).
            store.api_keys(),
        ))
        .merge(http::ota_report_router(
            store.config_trees(),
            SystemClock,
            // The store-facing report route authenticates the box by its scoped key, so a report
            // carries a tenant the cloud established rather than one the body claimed
            // (ADR-0097). The `/internal` route beside it keeps the shared secret.
            store.api_keys(),
            config.internal_shared_secret.clone(),
        ))
}

/// The OTA artifact routes, where an object store is configured.
fn ota_artifact_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps {
        store,
        audit,
        artifacts,
        release_source,
        ..
    } = *deps;
    Router::new()
        // Both halves of R2's artifact hosting, and both need the object store: the store-facing
        // route a box downloads from, and the `/admin` route a release is uploaded to. Absent an
        // `[artifacts]` block there is nowhere for bytes to live, so both are honestly absent rather
        // than present and answering `503`.
        .merge(match artifacts.cloned() {
            Some(blobs) => {
                let hosting = http::ota_artifact_router(
                    store.api_keys(),
                    SystemClock,
                    blobs.clone(),
                    store.ota_artifacts(),
                )
                .merge(http::ota_release_admin_router(
                    blobs.clone(),
                    store.ota_artifacts(),
                    store.admin(),
                    SystemClock,
                    Arc::clone(audit),
                ));
                // The third door is merged wherever bytes can be stored, with or without a forge:
                // configured, it fetches; unconfigured, it says which block of `cloud.toml` is
                // missing, which a console with a person at it can act on where a bare `404` from an
                // unmerged route could not (ADR-0088 Amendment 4).
                hosting.merge(http::ota_release_fetch_router(
                    blobs,
                    store.ota_artifacts(),
                    store.admin(),
                    SystemClock,
                    Arc::clone(audit),
                    release_source.cloned(),
                ))
            }
            None => Router::new(),
        })
}

/// Store archives, where both halves of the pair are configured, and the console's view of them.
fn store_archive_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps {
        store,
        config,
        artifacts,
        ..
    } = *deps;
    Router::new()
        // Store archives (ADR-0124) need **both** halves of the same pair: somewhere for the sealed
        // bytes to go, and a secret to wrap each store's key under. Either one missing and the
        // routes are honestly absent rather than answering `503` — and boot says which, because a
        // durability feature that is quietly off is the one nobody notices until a restore.
        .merge(
            match (artifacts.cloned(), config.archive_key_secret.clone()) {
                (Some(blobs), Some(secret)) => http::store_archive_router(
                    store.api_keys(),
                    SystemClock,
                    blobs,
                    store.archives(),
                    secret,
                ),
                (blobs, secret) => {
                    tracing::warn!(
                        has_object_store = blobs.is_some(),
                        has_archive_key_secret = secret.is_some(),
                        "store archives are off: both an [artifacts] object store and \
                     archive_key_secret are required, and no store will back its database up \
                     without them (ADR-0124)"
                    );
                    Router::new()
                }
            },
        )
        // The console's view of what each store has archived, mounted **unconditionally** unlike
        // the store-facing pair above: it needs only the registry, and an operator whose
        // `archive_key_secret` has gone missing is exactly the one who most needs to see what
        // archives exist (ADR-0124 slice 3).
        .merge(http::store_archive_admin_router(
            store.archives(),
            store.admin(),
            SystemClock,
        ))
}

/// Devices, translations, activation, claims, and the store registry.
fn device_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        .merge(http::device_router(
            store.device_proposals(),
            store.admin(),
            store.api_keys(),
            SystemClock,
            Arc::clone(audit),
        ))
        .merge(http::translation_router(
            store.translations(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        .merge(http::activation_router(
            store.activation_codes(),
            store.admin(),
            SystemClock,
        ))
        // A box installed with no store claims itself (ADR-0148): it shows a code, a console user
        // binds it to a device slot, and the box collects its credential once.
        .merge(http::claim_router(
            store.claims(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // The org registry (ADR-0065): named Tenant/Brand/Store/Device under the super-admin session,
        // the source of the dashboard's named pickers. Its tables are backfilled from `config_trees`
        // by migration 0011, so an existing cell's fleet appears here on the first boot after upgrade.
        .merge(http::registry_router(
            store.registry(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
            // A tenant created here starts with six roles (ADR-0158), written into the same people
            // store the people routes edit.
            Arc::new(RoleSeeder::new(store.people())),
        ))
}

/// Store groups and their batch publish.
fn store_group_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        // Store groups (ADR-0122): the named cohorts a tenant publishes to as one, and the batch
        // publish that fans one node out over a cohort's membership. A group holds no configuration
        // — it is a delivery axis, not a fifth config layer — so the batch writes the same document
        // into N per-store trees, exactly as N individual publishes would have.
        //
        // The registry is here because both halves need it: a membership write checks every id
        // against the tenant's own stores (a typo'd ULID would sit in the cohort permanently and be
        // reported `skipped` by every batch, leaving a group quietly one store short of what the
        // operator believes it is), and a batch skips a member that is archived or absent rather
        // than publishing into it.
        //
        // `batch_nodes` takes the same seven authoring seams the single-store publish routers below
        // already hold, which is what makes the batch the same code path rather than a second one:
        // it calls each node's own compiler and the shared config-tree write, so a document a batch
        // produces is byte-for-byte the document that route would have produced. The fee rules and
        // the registry are the menu's, whose publish brings each store's `fees` along.
        //
        // The people store is here because an assignment can name a group (ADR-0158): a store that
        // joins or leaves such a group gains or loses its people, and is published at once.
        .merge(http::store_group_router(
            store.store_groups(),
            store.registry(),
            store.config_trees(),
            http::batch_nodes(
                store.catalog(),
                store.tax_rates(),
                store.campaigns(),
                store.inventory(),
                store.reason_codes(),
                store.people(),
                store.floor(),
                store.fee_rules(),
                store.registry(),
            ),
            store.people(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
}

/// Configuration releases and previews.
fn config_release_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        // Releases (ADR-0125): a name over a set of publishes — nodes × stores, timed once. Creating
        // one is a draft; scheduling it snapshots each node, expands the cohort to concrete stores,
        // converts "Monday 04:00, local" to one instant per store's published timezone, and writes a
        // `scheduled_publishes` pair per (node, store). The activator above applies them and settles
        // the release's state. It shares the store-group router's node table for exactly the reason
        // ADR-0125 §1 gives: a node publishable through a release but not directly would be a second
        // way for a config version to be born.
        .merge(http::config_release_router(
            store.config_releases(),
            store.scheduled_publishes(),
            store.store_groups(),
            store.registry(),
            store.config_trees(),
            http::batch_nodes(
                store.catalog(),
                store.tax_rates(),
                store.campaigns(),
                store.inventory(),
                store.reason_codes(),
                store.people(),
                store.floor(),
                store.fee_rules(),
                store.registry(),
            ),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // The generic publish preview (roadmap-v3 F11): what publishing one node to one store would
        // change, computed and thrown away. It compiles through the same node table the batch and
        // the release use, so a preview cannot compile its node differently from the publish it is
        // previewing — which is the only way a dry run is worth anything. No audit recorder,
        // because it writes nothing.
        .merge(http::config_preview_router(
            store.config_trees(),
            http::batch_nodes(
                store.catalog(),
                store.tax_rates(),
                store.campaigns(),
                store.inventory(),
                store.reason_codes(),
                store.people(),
                store.floor(),
                store.fee_rules(),
                store.registry(),
            ),
            store.admin(),
            SystemClock,
        ))
}

/// People, their publish, and their permissions' readiness.
fn people_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        // People & access (ADR-0070): employees, role templates over the pos-core catalogue, and
        // assignments to a store, a store group or every store, with PIN set/reset. Every write is
        // audited (id/code/role, never the name or PIN). `store.people()` is the employee,
        // role-template, and assignment seam at once. The registry checks an assignment's store and
        // the store groups its group, both say which stores a wider one reaches, and the config
        // trees take the `permissions` node that removing an assignment, archiving a person or role,
        // or assigning someone to a group or every store publishes at once (ADR-0158 decisions 3
        // and 7). Setting a PIN reads the tenant's settings for the fewest digits a PIN may have
        // (ADR-0160).
        .merge(http::people_router(
            store.people(),
            store.registry(),
            store.store_groups(),
            store.settings(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // People publish (ADR-0070 slice 5): compile a store's people + roles + assignments into the
        // edge-shaped `permissions` document and write it onto the store's `permissions` config node,
        // so it rides the config tree to the store like every other config change.
        .merge(http::people_publish_router(
            store.people(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Permissions readiness (ADR-0158, Rollout): before a store enforces each person's own
        // permissions, what each role held there does not grant, beside what the store runs for
        // `permissions.enforced`. It reads the people store and resolves the setting the way the
        // settings routes do; it writes nothing.
        .merge(http::permissions_readiness_router(
            store.people(),
            store.settings(),
            store.registry(),
            store.store_groups(),
            store.admin(),
            SystemClock,
        ))
}

/// The floor, what is published to it and to devices, and the audit trail.
fn floor_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        // Floor & kitchen master data (ADR-0072, Track M2): a store's areas + tables and its kitchen
        // stations + item→station routing rules. Reads behind Read; writes behind the new ManageFloor
        // and audited. `store.floor()` is the area, table, station, and routing-rule seam at once.
        .merge(http::floor_router(
            store.floor(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
            // The catalog, for one question a floor write has to ask it: does this course exist?
            // A routing rule matching on a course used to be accepted without it (ADR-0130
            // decision 6), published, honoured, and matched nothing for ever.
            store.catalog(),
        ))
        // Floor & kitchen publish (ADR-0072 slice 4): compile the store's areas/tables + stations/
        // routing into the `floor`/`stations` config nodes and version them through the config tree,
        // so the edge applies the real floor plan and station routing.
        .merge(http::floor_publish_router(
            store.floor(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Device publish (ADR-0100, C2 slice 2b): compile the store's *approved* printers and kitchen
        // displays into the `devices` config node, so the edge learns where they are through the
        // config-pull it already runs — and still knows after a reboot with the WAN down, because
        // that node is persisted locally and restored at boot.
        .merge(http::device_publish_router(
            store.device_proposals(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Console audit read (ADR-0069 slice 4): the filterable Audit screen reads the append-only
        // trail here. It carries the concrete audit store (the recorder the write routes hold exposes
        // only `record`), behind the same super-admin session guard as the other reads.
        .merge(http::audit_router(
            store.audit(),
            store.admin(),
            SystemClock,
        ))
}

/// The fleet, its alerts, and capabilities.
fn fleet_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        // Fleet liveness (ADR-0068): the read-only console view of whether each store is up and in
        // sync — a join across the registry, `store_liveness` (captured on config pulls/heartbeats),
        // the config tree, and the order-relay queue, with online/offline derived at read.
        // The config-tree handle is for the single-store read only: it compares the store's region
        // against the country that store publishes (ADR-0114). The listing does not compare — one
        // config-tree load per row is work that grows with the fleet.
        //
        // The same router serves `POST /admin/stores/{store_id}/region-acknowledgement`, because
        // answering that warning needs both halves of the comparison and this is the only router
        // holding both. The acknowledgement store is the same `PostgresConfigTrees` handle — one
        // pool, one adapter — and the audit recorder is the shared one every write route uses.
        .merge(http::fleet_router(
            store.fleet(),
            store.admin(),
            SystemClock,
            store.config_trees(),
            store.config_trees(),
            Arc::clone(audit),
        ))
        // Operational alerts (ADR-0073, Track O2): the console reads the fleet-wide alert list the
        // evaluator loop maintains, and acknowledges/resolves alerts. Reads are behind the session
        // guard; acknowledge/resolve need console.alerts.manage and are audited.
        .merge(http::alerts_router(
            store.alerts(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Capability catalogue (ADR-0071): the §10 flags, presets, and inter-flag rules the Config
        // screen's form editor renders toggles and conflict previews from — static framework data
        // behind the session guard.
        .merge(http::capabilities_router(store.admin(), SystemClock))
}

/// The vendor catalogue, connections, and integrations.
fn integration_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps {
        store,
        config,
        audit,
        providers,
        ..
    } = *deps;
    Router::new()
        // The vendor catalogue (ADR-0153): every provider compiled into this binary, with the
        // settings schema the console draws a connection form from.
        .merge(http::provider_catalogue_router(
            Arc::clone(providers),
            store.admin(),
            SystemClock,
        ))
        // A tenant's vendor connections (ADR-0153): per-record CRUD, every write checked against
        // the provider's schema and every secret sealed under integration_secret before it is
        // stored. Reads answer which secrets are set, never their values.
        .merge(http::connection_router(
            store.connections(),
            Arc::clone(providers),
            config.integration_secret.clone(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Sending a store the connections that serve it (ADR-0153 decision 5): its own, its
        // brand's, its tenant's, resolved into the `integrations` node on its Store layer. A
        // publish like every other node, so it is console.config.publish, not the authoring
        // permission.
        .merge(http::config_integrations_router(
            store.connections(),
            Arc::clone(providers),
            store.registry(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
}

/// Shared settings, fees, and configured capabilities.
fn settings_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        // Settings (ADR-0160 decision 3): a value written once, at the tenant, a brand, a store
        // group or one store, and every store it reaches republished onto its Tenant layer in the
        // same request. Reads are console.data.read; a write publishes, so console.config.publish.
        .merge(http::settings_router(
            store.settings(),
            store.registry(),
            store.store_groups(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Fees (ADR-0159 decision 1): a rule written once, at the tenant, a brand or one store,
        // and every store it reaches republished in the same request, its rules resolved per fee,
        // compiled, and written whole as the `fees` node on its Store layer. A rule's figures are
        // prices, so reads are console.reports.revenue; a write publishes, so
        // console.config.publish.
        .merge(http::fees_router(
            store.fee_rules(),
            store.registry(),
            store.catalog(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Capability publish (ADR-0071): the form editor writes a store's flags here; the flags are
        // merged into the store's Store config layer (preserving menu/layout/permissions) and versioned
        // through the config tree, which runs the §10 inter-flag rules.
        .merge(http::config_capabilities_router(
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
}

/// Device revocation, and the loops' health.
fn device_revoke_and_health_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps {
        store,
        audit,
        expected_tasks,
        ..
    } = *deps;
    Router::new()
        // Remote device retirement (ADR-0118 §6): appends one local device id to the store's
        // `revoked_devices` deny-list, which the box carries out on its next config pull. Its own
        // router because it takes `console.stores.manage`, not the `PublishConfig` norm the other
        // node publishes take — the same standing as the lease bump beside it on the Fleet screen,
        // because this takes a terminal out of service.
        .merge(http::device_revoke_router(
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Background-task health (ADR-0068 slice 4): the read-only console view of whether the
        // off-request loops are alive and keeping up. `expected_tasks` names the loops this
        // deployment actually turned on, so a loop dead since boot reads as unhealthy, not missing.
        .merge(http::health_router(
            store.task_health(),
            store.admin(),
            SystemClock,
            expected_tasks.to_vec(),
        ))
}

/// The catalog and what it holds: items, tax rates, campaigns, inventory, reason codes, vouchers.
fn catalog_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        // Catalog authoring (ADR-0066): the write surface for the menu source of truth — items,
        // menus with inheritance, and per-channel placements — from which a MenuBook is compiled.
        .merge(http::catalog_router(
            store.catalog(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Tax rates (ADR-0074, Track M4): the per-(tax class × channel) rate an operator authors,
        // validated against the tenant's tax classes and published as the `tax` config node.
        .merge(http::tax_rate_router(
            store.tax_rates(),
            store.catalog(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Campaigns (ADR-0077, Track M3): author a tenant's promotions over the finished pricing
        // engine (per-campaign CRUD, behind console.campaigns.manage, audited by summary).
        .merge(http::campaign_router(
            store.campaigns(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Inventory (ADR-0079, Track M6): author a tenant's ingredients, per-item recipes (BOM), and
        // supplier references (per-record CRUD, behind console.inventory.manage, audited by summary —
        // never the recipe amounts). The composed `inventory` node publish is a separate route.
        .merge(http::inventory_router(
            store.inventory(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Reason codes (ADR-0115, roadmap B2.2): author the managed list a void, discount, comp,
        // refund, drawer opening, staff rejection, cash movement or stock correction must cite
        // (per-record CRUD behind console.reason_codes.manage, audited with the actions each entry
        // covers — narrowing those is how the fraud control gets defeated). The composed
        // `reason_codes` node publish is a separate route.
        .merge(http::reason_code_router(
            store.reason_codes(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Vouchers (ADR-0077, Track M3): mint and list the distributable codes a voucher-kind
        // campaign redeems (behind console.campaigns.manage, audited by count only).
        .merge(http::voucher_router(
            store.vouchers(),
            store.campaigns(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
}

/// Scheduled publishes, media, subjects, and the catalog publish.
fn catalog_publish_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        // Scheduled publishes (ADR-0077, Track M3): schedule the campaigns node to publish to a store
        // at a future instant (the Tết-menu case), list a store's pending publishes, and cancel one.
        // A background activator applies them at their time.
        .merge(http::scheduled_publish_router(
            store.scheduled_publishes(),
            store.campaigns(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Media (ADR-0075, Track M5): upload an image → re-encode → store two bounded renditions;
        // serve, list, and delete them.
        .merge(http::media_router(
            store.media(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Subject-request tooling (ADR-0076): per-subject PDPD/GDPR lookup / export / erase over the
        // subject store, owner-only and audited — the Data Protection contact's deliberate instrument.
        .merge(http::subjects_router(
            store.subjects(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Catalog publish (ADR-0066): compile a menu → write the MenuBook onto the store's `menu`
        // config node, so it rides the config tree to the store like every other config change. The
        // store's `fees` node goes in the same version (ADR-0159): a rule naming a category is
        // compiled into the items it holds, so the fee rules and the registry come along.
        .merge(http::catalog_publish_router(
            store.catalog(),
            store.fee_rules(),
            store.registry(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
}

/// The per-node configuration of tax, campaigns, inventory, reason codes, and OTA.
fn config_node_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps { store, audit, .. } = *deps;
    Router::new()
        // Tax publish (ADR-0074, Track M4): assemble the tenant's authored rates into the store's
        // `tax` config node, so the edge applies them to its session's tax table.
        .merge(http::config_tax_router(
            store.tax_rates(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Campaign publish (ADR-0077, Track M3): assemble the tenant's authored campaigns into the
        // store's `campaigns` config node, so the edge holds them for its pricing engine.
        .merge(http::config_campaigns_router(
            store.campaigns(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Inventory publish (ADR-0079, Track M6): assemble the tenant's authored ingredients, recipes,
        // and suppliers into the store's `inventory` config node, so the edge builds its RecipeBook and
        // auto-86 thresholds.
        .merge(http::config_inventory_router(
            store.inventory(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Reason-code publish (ADR-0115, roadmap B2.2): assemble the tenant's authored reason codes
        // into the store's `reason_codes` config node, so the till's void, discount, comp, refund,
        // drawer, rejection, cash-movement and stock-correction pickers offer the operator's own
        // list. The node REPLACES the framework default wholesale, which is why the route refuses an
        // empty publish: that would leave a trading store with no reason it could cite for anything.
        .merge(http::config_reason_codes_router(
            store.reason_codes(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // OTA rollout levers (ADR-0078, Track O3): publish a `fleet_update` rollout or engage its
        // kill switch from typed fields, instead of hand-editing the config node.
        .merge(http::ota_config_router(
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
            // The promote guard's source of truth: a rollout naming a version nobody hosts is
            // refused here rather than discovered as a fleet-wide `404` (ADR-0088 Amendment 2).
            store.ota_artifacts(),
        ))
}

/// Countries, and the per-node configuration of locale, store profile and channels.
fn locale_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps {
        store, audit, cell, ..
    } = *deps;
    Router::new()
        // Which cell this is (ADR-0179 C1): the `[cell]` section, read-only.
        .merge(http::cell_router(
            Arc::clone(cell),
            store.admin(),
            SystemClock,
        ))
        // Which cell this is and where a tenant is homed (ADR-0179 C3): public, answered from the
        // same directory holder, so it names the directory the reload last accepted.
        .merge(http::well_known_cell_router(Arc::clone(cell)))
        // Countries & locales (ADR-0074, Track M4): the compiled country modules surfaced as
        // read-only master data — the currency picker and the translation grid's locale catalogue.
        .merge(http::country_router(
            &countries::registry(),
            store.admin(),
            SystemClock,
        ))
        // Locale publish (ADR-0074, Track M4): a store's currency, timezone, and business-date cutoff
        // as the `locale` config node the edge applies (killing the hardcoded UTC/04:00 bootstrap).
        .merge(http::config_locale_router(
            // The country packs, for the currency exponent the node carries (ADR-0134).
            &countries::registry(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // Who a store legally is (ADR-0106): its registered name, address and tax registration, as
        // the `store_profile` node the edge composes every receipt from. The registration number's
        // *shape* is checked against the country module, which is why the registry is passed in.
        .merge(http::config_store_profile_router(
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
            Arc::new(countries::registry()),
        ))
        // Channels & tender (ADR-0080, Track M7): author which sales channels a store accepts and
        // which payment methods it takes, as the `channels` and `tender` settings nodes; the edge
        // applies each as a policy gate. Publish behind console.config.publish, audited.
        .merge(http::config_channels_router(
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
}

/// The order intake and the relay's store-facing order routes.
fn order_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps {
        store,
        orders_limiter,
        relay_wake,
        ..
    } = *deps;
    Router::new()
        // Public order intake + the cloud→store relay (ADR-0056, ADR-0061). The served `POST/GET
        // /v1/orders` calls the relay (an `OrderIn` over the durable per-store queue); the store
        // pulls and acks its queue over the store-facing `/sync/.../orders` routes. The relay
        // resolves the owning tenant and the per-store `order_relay` config from the config tree,
        // and the handler binds the request's store to the caller's tenant through the same
        // directory.
        .merge(orders::orders_router(
            OrderRelay::new(
                store.store_directory(),
                store.config_trees(),
                store.order_queue(),
                SystemClock,
                relay_wake.clone(),
            ),
            store.api_keys(),
            SystemClock,
            store.store_directory(),
            orders_limiter.clone(),
        ))
        .merge(relay::orders_sync_router(
            store.order_queue(),
            store.api_keys(),
            SystemClock,
            relay_wake.clone(),
        ))
}

/// Guest QR ordering (ADR-0057), only when a signing secret is configured: the guest carries no
/// API key, so the HMAC-signed table token is the only credential and an absent secret leaves the
/// endpoint off (any token would be unverifiable). It forwards accepted orders into the same relay
/// `POST /v1/orders` uses, guardrailed by the store's `qr` config.
pub(crate) fn with_guest_ordering(service: Router, deps: &RouteDeps<'_>) -> Router {
    let RouteDeps {
        store,
        config,
        relay_wake,
        ..
    } = *deps;
    if let Some(secret) = config.table_token_secret.clone() {
        tracing::info!("QR ordering enabled (POST /v1/qr/orders)");
        service
            .merge(qr_http::qr_router(
                TableTokenSecret::new(secret.clone()),
                OrderRelay::new(
                    store.store_directory(),
                    store.config_trees(),
                    store.order_queue(),
                    SystemClock,
                    relay_wake.clone(),
                ),
                store.config_trees(),
                SystemClock,
            ))
            // Table QR minting (ADR-0072): the console reads each table's signed token here to print a
            // QR sheet. Wired only with a secret, the same gate as the guest endpoint above.
            .merge(http::table_qr_router(
                store.floor(),
                store.admin(),
                SystemClock,
                TableTokenSecret::new(secret),
            ))
    } else {
        tracing::warn!("no table_token_secret configured; the QR ordering endpoint is off");
        service
    }
}

/// The finished service: the console as the fallback, then the `/sync` budget, then the console's
/// security headers outermost — in that order, because each layer covers only what is beneath it.
pub(crate) fn served(
    service: Router,
    sync_throttle: SyncThrottle<SystemClock>,
    cell: &Arc<CellDirectory>,
) -> Router {
    // The embedded back-office dashboard (ADR-0060) is the fallback: the API routes above match
    // first, and everything else — `/`, client-routed paths, the built static assets — is served
    // the single-page app, with an unknown path returning index.html for client-side routing.
    let service = service.fallback(assets::serve);
    // The `/sync` budget, over the fully-composed service (roadmap **Q5**). Here rather than inside
    // `http::router` because the store-facing surface spans more than one sub-router — the relay's
    // `/sync/.../orders` routes are merged in above — and a layer applied to only one of them would
    // leave the busiest path unthrottled. The layer passes every other prefix straight through, so
    // `/admin`, `/v1` and the console SPA are untouched.
    let service = service.layer(axum::middleware::from_fn_with_state(
        sync_throttle,
        http::throttle_sync,
    ));
    // The front door (ADR-0179 C5): a request for a tenant homed in another cell is redirected
    // there, 302 for a browser and 307 for an API call, never proxied. Over the fully-composed
    // service, fallback included, because a browser's first request is the console document; and
    // outside the throttle, so a redirected request costs this cell no budget.
    let service = service.layer(axum::middleware::from_fn_with_state(
        Arc::clone(cell),
        pos_cloud::cell::front_door::front_door,
    ));
    // The console security headers over the **fully-composed** service (ADR-0067 slice 5,
    // production-readiness **S3**). `http::router` layers the same middleware over its own routes, and
    // a comment there claimed this line already existed — it did not, so the console's own document,
    // its assets, and every `/admin` sub-router merged in above (devices, registry, people, floor,
    // catalog, audit, fleet, alerts, …) were served with no `Content-Security-Policy`, no
    // `X-Frame-Options` and no `Referrer-Policy`. The SPA fallback is the one that matters most: it is
    // the document a browser renders, and the inner layer never reached it because `.fallback` is
    // added out here.
    //
    // Outermost on purpose, so it also covers the throttle's own `429` and any rejection a layer
    // produces before a route is matched. The headers are `insert`ed, so the two layers agree rather
    // than stacking.
    service.layer(axum::middleware::from_fn(http::security_headers))
}
