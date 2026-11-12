// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's packages and their published versions
//! ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 1).
//!
//! A package is what HQ designs a store's screens, flows, rules and data in. Each publish of it is
//! a **version**: its content as it will ride a store's document ([`PackageContent`]), stored once
//! and never changed, so a store pinned to a version runs the same thing for as long as it is
//! pinned, and pinning the previous version is a rollback. The tables refuse an update or a delete
//! to the role the cloud runs tenant queries as, so the rule holds below this seam too.
//!
//! Authored per tenant and RLS-isolated by tenant, as the catalog is. The store's document carries
//! only a compiled version, never this store's rows.

use core::future::Future;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use pos_core::ota::ReleaseVersion;
use pos_proto::actions::Action;
use pos_proto::ids::{PackageId, PackageVersionId, TenantId};
use pos_proto::package::PackageContent;
use pos_proto::screen::{Component, PropertyValue, ScreenNode};
use pos_proto::text::{DisplayName, ReleaseTag};
use pos_proto::time::Timestamp;

use crate::version::CreateOutcome;

mod persistence;

/// The first release whose edge applies a `package` node. Every version needs at least it, and
/// the parts a version uses can only raise it (ADR-0189 decision 5).
pub const PACKAGE_NODE_SINCE: &str = "0.14.1";

/// The first release whose edge checks a package's data part (ADR-0194). A version that declares
/// data needs at least it.
///
/// It reads "0.14.1" because no release has been tagged since 0.14.0, so the next one is the first
/// to carry `pos_core::package::check_data`. **When the first release that carries `check_data` is
/// tagged, set this to that release** if it is not 0.14.1. Otherwise an edge on an older release
/// could be sent a version with data that it cannot check.
pub const DATA_PART_SINCE: &str = "0.14.1";

/// The most packages, and the most versions of all packages, the in-memory store holds.
pub const IN_MEMORY_ROWS: usize = 1024;

/// One package: what its versions are versions of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PackageRecord {
    /// The package.
    pub package_id: PackageId,
    /// Its name in the console.
    pub name: DisplayName,
    /// When it was created.
    pub create_time: Timestamp,
}

/// One published version: its content, as it rides a store's document, and when it was published.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PackageVersionRecord {
    /// The content. Its `package_version_id`, label and minimum edge release identify the version.
    pub content: PackageContent,
    /// When it was published.
    pub create_time: Timestamp,
}

/// The oldest edge release that runs `content`: [`PACKAGE_NODE_SINCE`] until a part a version
/// uses introduces a later one: the data part, each component and act its screens name, and each
/// act its flows and reactions run (ADR-0190, ADR-0191). Nobody types it.
#[must_use]
pub fn min_edge_version(content: &PackageContent) -> ReleaseTag {
    let floor = if content.data.is_empty() {
        PACKAGE_NODE_SINCE
    } else {
        DATA_PART_SINCE
    };
    let mut pending: Vec<&ScreenNode> = content
        .screens
        .iter()
        .map(|variant| &variant.body.root)
        .collect();
    let mut latest = floor;
    while let Some(node) = pending.pop() {
        let component = Component::find(&node.component_id).map(|found| found.since);
        let acts = node.properties.values().filter_map(|value| match value {
            PropertyValue::Action(binding) => Action::find(&binding.action_id).map(|act| act.since),
            _ => None,
        });
        for since in component.into_iter().chain(acts) {
            if release(since) > release(latest) {
                latest = since;
            }
        }
        pending.extend(node.children.iter());
    }
    // The acts its flows start with and run, and its reactions run (ADR-0192, ADR-0193).
    let flow_acts = content.flows.iter().flat_map(|variant| {
        core::iter::once(variant.body.start_action_id.as_str()).chain(
            variant
                .body
                .steps
                .iter()
                .filter_map(|step| step.action_id.as_deref()),
        )
    });
    let reaction_acts = content
        .reactions
        .iter()
        .map(|variant| variant.body.then.action_id.as_str());
    for since in flow_acts
        .chain(reaction_acts)
        .filter_map(|action_id| Action::find(action_id).map(|act| act.since))
    {
        if release(since) > release(latest) {
            latest = since;
        }
    }
    ReleaseTag::new(latest)
}

/// A catalogue's `since`, ordered; one that does not parse orders first.
fn release(text: &str) -> Option<ReleaseVersion> {
    ReleaseVersion::parse(text)
}

/// Persists and reads a tenant's packages and their versions.
///
/// Every method is tenant-scoped. There is no update and no delete: a version, once published,
/// never changes ([`PackageStore::publish_version`] refuses a taken id).
pub trait PackageStore {
    /// Every package of the tenant, in id order.
    fn list_packages(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<PackageRecord>, PackageStoreError>> + Send;

    /// One package, or `None` if the tenant has none with that id.
    fn get_package(
        &self,
        tenant_id: TenantId,
        package_id: PackageId,
    ) -> impl Future<Output = Result<Option<PackageRecord>, PackageStoreError>> + Send;

    /// Inserts a package, refusing if one already holds its id.
    fn create_package(
        &self,
        tenant_id: TenantId,
        package: &PackageRecord,
    ) -> impl Future<Output = Result<CreateOutcome, PackageStoreError>> + Send;

    /// Every version of one package, in id order: publish order, but for two published in the same
    /// millisecond.
    fn list_versions(
        &self,
        tenant_id: TenantId,
        package_id: PackageId,
    ) -> impl Future<Output = Result<Vec<PackageVersionRecord>, PackageStoreError>> + Send;

    /// One version of any of the tenant's packages, or `None` if the tenant has none with that id.
    fn get_version(
        &self,
        tenant_id: TenantId,
        package_version_id: PackageVersionId,
    ) -> impl Future<Output = Result<Option<PackageVersionRecord>, PackageStoreError>> + Send;

    /// Stores a version for good, refusing if one already holds its id.
    fn publish_version(
        &self,
        tenant_id: TenantId,
        version: &PackageVersionRecord,
    ) -> impl Future<Output = Result<CreateOutcome, PackageStoreError>> + Send;
}

/// A failure of the package store itself: the database is unreachable, or a stored row could not
/// be decoded.
#[derive(Debug, thiserror::Error)]
#[error("the package store failed: {0}")]
pub struct PackageStoreError(String);

impl PackageStoreError {
    /// Wraps a message, for the server's log.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The store in memory, for tests and for code that runs without a database. Holds at most
/// [`IN_MEMORY_ROWS`] packages and as many versions.
#[derive(Clone, Debug, Default)]
pub struct InMemoryPackages {
    rows: Arc<Mutex<Rows>>,
}

#[derive(Debug, Default)]
struct Rows {
    packages: BTreeMap<(TenantId, PackageId), PackageRecord>,
    versions: BTreeMap<(TenantId, PackageVersionId), PackageVersionRecord>,
}

impl InMemoryPackages {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn rows(&self) -> std::sync::MutexGuard<'_, Rows> {
        self.rows.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The version an in-memory row is created at. Nothing updates a row, so one is enough.
fn first_version() -> crate::version::Version {
    crate::version::Version::new("1")
}

fn full() -> PackageStoreError {
    PackageStoreError::new(format!(
        "the in-memory package store holds at most {IN_MEMORY_ROWS} rows of each kind"
    ))
}

impl PackageStore for InMemoryPackages {
    async fn list_packages(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<PackageRecord>, PackageStoreError> {
        Ok(self
            .rows()
            .packages
            .iter()
            .filter(|((tenant, _), _)| *tenant == tenant_id)
            .map(|(_, package)| package.clone())
            .collect())
    }

    async fn get_package(
        &self,
        tenant_id: TenantId,
        package_id: PackageId,
    ) -> Result<Option<PackageRecord>, PackageStoreError> {
        Ok(self.rows().packages.get(&(tenant_id, package_id)).cloned())
    }

    async fn create_package(
        &self,
        tenant_id: TenantId,
        package: &PackageRecord,
    ) -> Result<CreateOutcome, PackageStoreError> {
        let mut rows = self.rows();
        let key = (tenant_id, package.package_id);
        if rows.packages.contains_key(&key) {
            return Ok(CreateOutcome::AlreadyExists);
        }
        if rows.packages.len() >= IN_MEMORY_ROWS {
            return Err(full());
        }
        rows.packages.insert(key, package.clone());
        Ok(CreateOutcome::Created(first_version()))
    }

    async fn list_versions(
        &self,
        tenant_id: TenantId,
        package_id: PackageId,
    ) -> Result<Vec<PackageVersionRecord>, PackageStoreError> {
        Ok(self
            .rows()
            .versions
            .iter()
            .filter(|((tenant, _), version)| {
                *tenant == tenant_id && version.content.package_id == package_id
            })
            .map(|(_, version)| version.clone())
            .collect())
    }

    async fn get_version(
        &self,
        tenant_id: TenantId,
        package_version_id: PackageVersionId,
    ) -> Result<Option<PackageVersionRecord>, PackageStoreError> {
        Ok(self
            .rows()
            .versions
            .get(&(tenant_id, package_version_id))
            .cloned())
    }

    async fn publish_version(
        &self,
        tenant_id: TenantId,
        version: &PackageVersionRecord,
    ) -> Result<CreateOutcome, PackageStoreError> {
        let mut rows = self.rows();
        let key = (tenant_id, version.content.package_version_id);
        if rows.versions.contains_key(&key) {
            return Ok(CreateOutcome::AlreadyExists);
        }
        if rows.versions.len() >= IN_MEMORY_ROWS {
            return Err(full());
        }
        rows.versions.insert(key, version.clone());
        Ok(CreateOutcome::Created(first_version()))
    }
}
