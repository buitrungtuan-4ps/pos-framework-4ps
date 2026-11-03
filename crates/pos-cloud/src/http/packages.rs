// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Packages: a tenant's packages and their published versions under `/admin/packages`
//! ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 1).
//!
//! A version is published once and never changes, so there is no update and no delete. Pinning a
//! store to a version is a settings write; compiling the pinned version into a store's document is
//! the config publish's.

use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::get;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::package::TimeSlot;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use pos_cloud_config::packages::{PackageStore, PackageStoreError};

mod authoring;

// The handlers the console's OpenAPI document names.
pub(crate) use authoring::{
    __path_admin_create_package, __path_admin_list_package_versions, __path_admin_list_packages,
    __path_admin_publish_package_version,
};

/// The state the package routes share.
#[derive(Clone)]
struct PackageState<P, A, C> {
    packages: P,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A package create body. The id is server-owned.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct PackageRequest {
    tenant_id: String,
    /// Its name in the console.
    name: String,
}

/// A version publish body: the content a store will run. The ids and the minimum edge release are
/// the server's.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct PackageVersionRequest {
    tenant_id: String,
    /// The version's name in the console.
    version_label: String,
    /// The package's named time slots, in the order that decides which one is current.
    #[serde(default)]
    time_slots: Vec<TimeSlot>,
}

/// Builds the package sub-router. Reads are `console.data.read`; creating a package and publishing
/// a version are `console.packages.manage`.
pub fn package_router<P, A, C>(
    packages: P,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    P: PackageStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/packages",
            get(authoring::admin_list_packages::<P, A, C>)
                .post(authoring::admin_create_package::<P, A, C>),
        )
        .route(
            "/admin/packages/{package_id}/versions",
            get(authoring::admin_list_package_versions::<P, A, C>)
                .post(authoring::admin_publish_package_version::<P, A, C>),
        )
        .with_state(PackageState {
            packages,
            admin,
            clock,
            audit,
        })
}

/// The `503` a create or publish earns when the OS will not give the entropy its id needs.
fn package_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a package id");
    api_error(
        ErrorStatus::Unavailable,
        "the package service is unavailable",
    )
}

/// Maps a package store failure to a retryable `503`, logging the detail rather than leaking it.
fn package_error_response(error: &PackageStoreError) -> Response {
    tracing::error!(%error, "a package store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the package service is unavailable",
    )
}
