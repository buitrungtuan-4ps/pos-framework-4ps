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
use axum::routing::{get, post};
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::flow::Flow;
use pos_proto::package::{HomeScreen, PackageData, TimeSlot, Variant};
use pos_proto::reaction::Reaction;
use pos_proto::screen::Screen;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::http::grid_keys::{GridKeys, StoreGrid};
use pos_cloud_catalog::translations::TranslationStore;
use pos_cloud_config::packages::{PackageStore, PackageStoreError};

mod authoring;
mod conditions;
mod dry_run;

// The handlers the console's OpenAPI document names.
pub(crate) use authoring::{
    __path_admin_create_package, __path_admin_list_package_versions, __path_admin_list_packages,
    __path_admin_publish_package_version,
};
pub(crate) use dry_run::__path_admin_dry_run_package_version;

/// The state the package routes share.
#[derive(Clone)]
struct PackageState<P, A, C> {
    packages: P,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    /// The tenant's translation grid, which every key a version names needs an `en` value in
    /// (ADR-0190 decision 6).
    grid: Arc<dyn GridKeys>,
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
    /// The package's data part: custom fields, object types and lists (ADR-0194). Each field says
    /// whether it is personal; publishing a personal field is the tenant's decision on its lawful
    /// basis, consent wording and retention, and is refused until personal fields are available
    /// (ADR-0206).
    #[serde(default)]
    data: PackageData,
    /// The package's screens, every variant of each (ADR-0190, ADR-0203): trees of the release's
    /// catalogued components, given only literals.
    #[serde(default)]
    screens: Vec<Variant<Screen>>,
    /// The package's flows, every variant of each (ADR-0192): graphs of required, optional and
    /// conditional steps with their prerequisites. Their conditions are compiled at publish.
    #[serde(default)]
    flows: Vec<Variant<Flow>>,
    /// The package's reactions, every variant of each (ADR-0193 decision 5): when a trigger fires,
    /// if a condition holds, one UI act or edge write.
    #[serde(default)]
    reactions: Vec<Variant<Reaction>>,
    /// The screen each role starts on after sign-in (ADR-0210 decision 1): variants of one
    /// `screen_id` each, stating at most a role and a device class, with a base whenever the
    /// version declares a screen. Leave it out for a version whose tills start where their
    /// release starts them.
    #[serde(default)]
    home_screens: Vec<Variant<HomeScreen>>,
}

/// Builds the package sub-router. Reads are `console.data.read`; creating a package, publishing a
/// version and a publish's dry run are `console.packages.manage`. A version is checked against the
/// tenant's grid in `translations`.
pub fn package_router<P, A, C, Tr>(
    packages: P,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    translations: Tr,
) -> Router
where
    P: PackageStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Tr: TranslationStore + Send + Sync + 'static,
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
        .route(
            "/admin/packages/{package_id}/versions/dry-run",
            post(dry_run::admin_dry_run_package_version::<P, A, C>),
        )
        .with_state(PackageState {
            packages,
            admin,
            clock,
            audit,
            grid: Arc::new(StoreGrid(translations)),
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
