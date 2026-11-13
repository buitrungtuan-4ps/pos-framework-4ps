// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The re-seal pass (ADR-0217 decision 3): a tenant's secrets sealed under an older version of
//! its key, or before tenant keys, are opened and sealed again under the newest, a bounded number
//! at a time; once nothing is left under an older version, that version is retired.
//!
//! A record is rewritten only at the version the pass read it at, so an edit a person makes at the
//! same moment wins and the pass comes back to the record on its next run. What it counts is all it
//! reports: never a value.

use pos_proto::ids::TenantId;

use super::sealer::{Keys, TenantSealer, sealed_version};
use crate::connections::{ConnectionStore, SealError, SealedFor};
use crate::gateway::endpoints::GatewayEndpointStore;
use crate::version::UpdateOutcome;

/// What one run of the pass did for one tenant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResealReport {
    /// The version every value is sealed under once the pass is done.
    pub newest_version: u32,
    /// Values sealed again under it in this run.
    pub resealed: usize,
    /// Values still under an older version: past this run's bound, or in a record edited
    /// meanwhile.
    pub remaining: usize,
    /// Older key versions retired in this run, because nothing is sealed under them any more.
    pub retired: u64,
}

/// Why the pass stopped for a tenant.
#[derive(Debug, thiserror::Error)]
pub enum ResealError {
    /// The tenant's keys, or a value under them, could not be opened or sealed.
    #[error(transparent)]
    Seal(#[from] SealError),
    /// The connection or endpoint store failed.
    #[error("a record store failed: {0}")]
    Store(String),
}

/// Seals `values` again under the newest key, or `None` when none is under an older version.
fn resealed(
    keys: &TenantSealer,
    values: &std::collections::BTreeMap<String, String>,
    bound: impl Fn(&str) -> (String, String),
    tenant_id: TenantId,
) -> Result<Option<std::collections::BTreeMap<String, String>>, SealError> {
    let newest = keys.newest_version();
    if values
        .values()
        .all(|stored| sealed_version(stored) == Some(newest))
    {
        return Ok(None);
    }
    let mut sealed = std::collections::BTreeMap::new();
    for (name, stored) in values {
        let (record_id, field_key) = bound(name);
        let sealed_for = SealedFor {
            tenant_id,
            connection_id: &record_id,
            field_key: &field_key,
        };
        let value = keys.open(sealed_for, stored)?;
        sealed.insert(name.clone(), keys.seal(sealed_for, value.as_str())?);
    }
    Ok(Some(sealed))
}

/// How many of `values` are under another version than `newest`.
fn older(values: &std::collections::BTreeMap<String, String>, newest: u32) -> usize {
    values
        .values()
        .filter(|stored| sealed_version(stored) != Some(newest))
        .count()
}

/// Re-seals at most `limit` of the tenant's values under its newest key, connections first, and
/// retires the older versions once none is left under them.
///
/// # Errors
///
/// [`ResealError`] if the keys or a store failed; what was rewritten before stays rewritten.
pub async fn reseal_tenant<CS, ES>(
    keys: &Keys,
    connections: &CS,
    endpoints: &ES,
    tenant_id: TenantId,
    limit: usize,
) -> Result<ResealReport, ResealError>
where
    CS: ConnectionStore + Sync,
    ES: GatewayEndpointStore + Sync,
{
    let tenant_keys = keys.for_tenant(tenant_id).await?;
    let newest = tenant_keys.newest_version();
    let mut report = ResealReport {
        newest_version: newest,
        ..ResealReport::default()
    };
    let store = |error: &dyn core::fmt::Display| ResealError::Store(error.to_string());
    for held in connections
        .list(tenant_id)
        .await
        .map_err(|error| store(&error))?
    {
        let count = older(&held.record.sealed_secrets, newest);
        if count == 0 {
            continue;
        }
        if report.resealed.saturating_add(count) > limit {
            report.remaining = report.remaining.saturating_add(count);
            continue;
        }
        let id = held.record.connection_id.clone();
        let Some(sealed) = resealed(
            &tenant_keys,
            &held.record.sealed_secrets,
            |name| (id.clone(), name.to_owned()),
            tenant_id,
        )?
        else {
            continue;
        };
        let mut record = held.record;
        record.sealed_secrets = sealed;
        match connections
            .update(tenant_id, &record, &held.etag)
            .await
            .map_err(|error| store(&error))?
        {
            UpdateOutcome::Updated(_) => report.resealed = report.resealed.saturating_add(count),
            UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound => {
                report.remaining = report.remaining.saturating_add(count);
            }
        }
    }
    for held in endpoints
        .list(tenant_id)
        .await
        .map_err(|error| store(&error))?
    {
        let count = older(&held.record.sealed_headers, newest);
        if count == 0 {
            continue;
        }
        if report.resealed.saturating_add(count) > limit {
            report.remaining = report.remaining.saturating_add(count);
            continue;
        }
        let id = held.record.gateway_endpoint_id.clone();
        let Some(sealed) = resealed(
            &tenant_keys,
            &held.record.sealed_headers,
            |name| (id.clone(), format!("header:{name}")),
            tenant_id,
        )?
        else {
            continue;
        };
        let mut record = held.record;
        record.sealed_headers = sealed;
        match endpoints
            .update(tenant_id, &record, &held.etag)
            .await
            .map_err(|error| store(&error))?
        {
            UpdateOutcome::Updated(_) => report.resealed = report.resealed.saturating_add(count),
            UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound => {
                report.remaining = report.remaining.saturating_add(count);
            }
        }
    }
    if report.remaining == 0 && newest > 1 {
        report.retired = keys.retire_below(tenant_id, newest).await?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
