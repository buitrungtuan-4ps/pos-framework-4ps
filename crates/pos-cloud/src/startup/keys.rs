// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's keys at boot, in the background and on the command line
//! ([ADR-0217](../../../../docs/adr/0217-secrets-are-sealed-under-versioned-tenant-keys.md)).
//!
//! - [`keys`]: the keeper and the tenant-key store the routes seal under.
//! - [`reseal_loop`]: every [`RESEAL_EVERY`], the bounded re-seal pass over every tenant, which
//!   moves values to their tenant's newest key and retires a version nothing is sealed under. Each
//!   tenant it changed is recorded on the audit trail with its counts, which is the pass's progress.
//! - [`command`]: `pos-cloud keys rotate|reseal|erase`, run by an operator on the cloud's host.
//!   Erasing a tenant's keys needs the tenant id typed twice and the owner's approval reference,
//!   and is audited; it cannot be undone.

use core::time::Duration;
use std::sync::Arc;

use pos_cloud::CloudConfig;
use pos_cloud::audit::AuditRecorder;
use pos_cloud::clock::SystemClock;
use pos_cloud::keys::{EnvironmentKeys, Keys, ResealReport, reseal_tenant};
use pos_cloud_api::auth::admin::system_actor;
use pos_cloud_api::console::audit_action;
use pos_proto::ids::TenantId;
use store_postgres::PostgresStore;
use tokio::task::JoinHandle;

/// How often the re-seal pass runs.
pub(crate) const RESEAL_EVERY: Duration = Duration::from_secs(15 * 60);

/// The most values one tenant's pass re-seals per run, and the most one run re-seals in all.
const RESEAL_PER_TENANT: usize = 200;
const RESEAL_PER_RUN: usize = 2_000;

/// The keys every secret is sealed under: the file keeper over `integration_secret` (development
/// only; ADR-0217 decision 5), each tenant's own key in `tenant_keys`, and `integration_secret`
/// again for a value sealed before tenant keys. `None` while `cloud.toml` has no
/// `integration_secret`.
pub(crate) fn keys(config: &CloudConfig, store: &PostgresStore) -> Option<Keys> {
    config.integration_secret.as_ref().and_then(|secret| {
        EnvironmentKeys::from_file_key(secret).map(|keeper| {
            Keys::new(
                keeper,
                store.tenant_keys(),
                Some(secret.clone()),
                SystemClock,
            )
        })
    })
}

/// Records what a key action did for one tenant, as the system or the operator's command.
async fn record(
    audit: &Arc<dyn AuditRecorder>,
    tenant_id: TenantId,
    action: &str,
    after: serde_json::Value,
) {
    let actor = system_actor();
    let tenant = tenant_id.to_string();
    audit_action(
        audit,
        &SystemClock,
        &actor,
        Some(tenant_id),
        action,
        "tenant_key",
        &tenant,
        None,
        Some(after),
    )
    .await;
}

fn report_json(report: &ResealReport) -> serde_json::Value {
    serde_json::json!({
        "newest_version": report.newest_version,
        "resealed": report.resealed,
        "remaining": report.remaining,
        "retired": report.retired,
    })
}

/// One run of the pass over every tenant, within [`RESEAL_PER_RUN`].
async fn reseal_run(keys: &Keys, store: &PostgresStore, audit: &Arc<dyn AuditRecorder>) {
    let tenants = match store.registry().fetch_tenants().await {
        Ok(tenants) => tenants,
        Err(error) => {
            tracing::warn!(%error, "the re-seal pass could not list the tenants");
            return;
        }
    };
    let mut done = 0_usize;
    for row in tenants {
        if done >= RESEAL_PER_RUN {
            break;
        }
        let Ok(tenant_id) = row.tenant_id.parse::<TenantId>() else {
            continue;
        };
        let limit = RESEAL_PER_TENANT.min(RESEAL_PER_RUN.saturating_sub(done));
        match reseal_tenant(
            keys,
            &store.connections(),
            &store.gateway_endpoints(),
            tenant_id,
            limit,
        )
        .await
        {
            Ok(report) => {
                done = done.saturating_add(report.resealed);
                if report.resealed > 0 || report.retired > 0 {
                    tracing::info!(
                        %tenant_id,
                        newest_version = report.newest_version,
                        resealed = report.resealed,
                        remaining = report.remaining,
                        retired = report.retired,
                        "re-sealed a tenant's secrets under its newest key"
                    );
                    record(audit, tenant_id, "tenant_key.reseal", report_json(&report)).await;
                }
            }
            Err(error) => {
                tracing::warn!(%tenant_id, %error, "the re-seal pass stopped for a tenant");
            }
        }
    }
}

/// The re-seal pass, every [`RESEAL_EVERY`], when the cloud has keys.
pub(crate) fn reseal_loop(
    keys: Option<Keys>,
    store: &PostgresStore,
    audit: &Arc<dyn AuditRecorder>,
) -> Option<JoinHandle<()>> {
    let keys = keys?;
    let store = store.clone();
    let audit = Arc::clone(audit);
    tracing::info!(
        interval_secs = RESEAL_EVERY.as_secs(),
        "re-seal pass started"
    );
    Some(tokio::spawn(async move {
        let mut ticks = tokio::time::interval(RESEAL_EVERY);
        loop {
            ticks.tick().await;
            reseal_run(&keys, &store, &audit).await;
        }
    }))
}

/// What `pos-cloud keys` answers when it is used wrongly.
const USAGE: &str = "usage: pos-cloud keys rotate --tenant <tenant_id>\n\
       pos-cloud keys rotate --environment\n\
       pos-cloud keys reseal --tenant <tenant_id>\n\
       pos-cloud keys erase --tenant <tenant_id> --confirm <tenant_id> --approval <reference>";

/// The value after `flag` in `args`, if given.
fn flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|at| args.get(at.saturating_add(1)))
        .map(String::as_str)
}

/// `pos-cloud keys …`, when the arguments ask for it: `None` for an ordinary boot.
pub(crate) async fn command(args: &[String]) -> Option<Result<(), Box<dyn std::error::Error>>> {
    let (Some("keys"), Some(verb)) = (
        args.get(1).map(String::as_str),
        args.get(2).map(String::as_str),
    ) else {
        return None;
    };
    Some(run_command(verb, args.get(3..).unwrap_or_default()).await)
}

async fn run_command(verb: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let config = super::load_config()?;
    let store = PostgresStore::connect(&config.database_url).map_err(|error| error.to_string())?;
    store.migrate().await.map_err(|error| error.to_string())?;
    let keys = keys(&config, &store).ok_or("cloud.toml has no usable integration_secret")?;
    let audit: Arc<dyn AuditRecorder> = Arc::new(pos_cloud::audit::AuditSink::new(store.audit()));
    let tenant = || -> Result<TenantId, Box<dyn std::error::Error>> {
        Ok(flag(args, "--tenant").ok_or(USAGE)?.parse::<TenantId>()?)
    };
    match verb {
        "rotate" if args.iter().any(|arg| arg == "--environment") => {
            let mut moved = 0_usize;
            for row in store.registry().fetch_tenants().await? {
                if let Ok(tenant_id) = row.tenant_id.parse::<TenantId>() {
                    let count = keys.rewrap_tenant(tenant_id).await?;
                    if count > 0 {
                        record(
                            &audit,
                            tenant_id,
                            "tenant_key.rewrap",
                            serde_json::json!({ "rewrapped": count }),
                        )
                        .await;
                    }
                    moved = moved.saturating_add(count);
                }
            }
            tracing::info!(
                rewrapped = moved,
                "re-wrapped tenant keys under the newest environment key"
            );
        }
        "rotate" => {
            let tenant_id = tenant()?;
            let version = keys.rotate_tenant(tenant_id).await?;
            record(
                &audit,
                tenant_id,
                "tenant_key.rotate",
                serde_json::json!({ "key_version": version }),
            )
            .await;
            tracing::info!(%tenant_id, key_version = version, "the tenant now seals under a new key version; the re-seal pass moves the rest");
        }
        "reseal" => {
            let tenant_id = tenant()?;
            let report = reseal_tenant(
                &keys,
                &store.connections(),
                &store.gateway_endpoints(),
                tenant_id,
                usize::MAX,
            )
            .await?;
            record(&audit, tenant_id, "tenant_key.reseal", report_json(&report)).await;
            tracing::info!(%tenant_id, report = %report_json(&report), "re-sealed the tenant's secrets");
        }
        "erase" => {
            let tenant_id = tenant()?;
            if flag(args, "--confirm") != Some(tenant_id.to_string().as_str()) {
                return Err(
                    "erasing a tenant's keys needs --confirm with the same tenant id".into(),
                );
            }
            let approval = flag(args, "--approval")
                .filter(|reference| !reference.trim().is_empty())
                .ok_or(
                    "erasing a tenant's keys needs --approval with the owner's approval reference",
                )?;
            // A value from before tenant keys is under integration_secret, not the tenant's key: move
            // every one first, so erasing the keys leaves nothing of the tenant's readable.
            let moved = reseal_tenant(
                &keys,
                &store.connections(),
                &store.gateway_endpoints(),
                tenant_id,
                usize::MAX,
            )
            .await?;
            if moved.remaining > 0 {
                return Err("some of the tenant's secrets could not be moved under its key; nothing was erased".into());
            }
            let erased = keys.erase_tenant(tenant_id).await?;
            record(
                &audit,
                tenant_id,
                "tenant_key.erase",
                serde_json::json!({ "erased_versions": erased, "approval": approval }),
            )
            .await;
            tracing::warn!(%tenant_id, erased_versions = erased, "erased the tenant's keys: its sealed secrets can no longer be opened");
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}
