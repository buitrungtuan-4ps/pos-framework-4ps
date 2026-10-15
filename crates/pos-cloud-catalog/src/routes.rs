// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The catalog's routes: catalog authoring and its publish, fee rules, media, tax rates and the
//! translation grid, which `pos-cloud`'s router mounts.
//!
//! `pos-cloud`'s `http` module imports each area under its old name (`catalog`, `fees`, `media`,
//! `tax_rates`, `translations`), so `pos_cloud::http::catalog_router` and the rest keep their paths.
//! The helpers the moved handlers named as `crate::http::…` are imported here from where they live
//! (`pos-cloud-api` and `pos-cloud-config`), so they resolve as `crate::routes::…`.

use axum::Json;
use serde::Deserialize;

use pos_cloud_api::conditional::{
    already_exists, if_match, if_match_collection, version_mismatch, versioned_created,
    versioned_ok, with_etag,
};
use pos_cloud_api::paged::{paged_ok, parse_page};
use pos_cloud_api::refusal::{
    FieldRefusal, enum_refusal, not_found, parse_optional_ulid, present_param,
    unprocessable_violations,
};
use pos_cloud_config::publish as config_publish;
use pos_cloud_config::publish::{layer_with_nodes, load_tree_for_write, publish_config_nodes_with};
use pos_cloud_config::route::{
    RegistryTenantQuery, entity_status_refusal, parse_entity_status, registry_error_response,
};

pub mod catalog;
pub mod fees;
pub mod media;
pub mod tax_rates;
pub mod translations;

use catalog::{catalog_error_response, clean_name_translations};

/// Create a course — a named point in the service sequence (ADR-0130).
#[derive(Debug, Clone, Deserialize)]
struct CreateCourseRequest {
    tenant_id: String,
    name: String,
    /// Where it falls in the sequence, ascending. Absent is `0`, which puts a first course first
    /// and leaves the id to break the tie with any other unpositioned one.
    #[serde(default)]
    sort: i32,
}

/// Update a course's name, position and/or status.
#[derive(Debug, Clone, Deserialize)]
struct UpdateCourseRequest {
    tenant_id: String,
    name: String,
    #[serde(default)]
    sort: i32,
    status: String,
}

/// `GET /admin/catalog/items`: the tenant, plus the optional paging bounds.
///
/// The `limit`/`offset` pair is repeated per route for the reason `pos-cloud`'s `VoucherListQuery`
/// gives.
#[derive(Debug, Clone, Deserialize)]
struct ItemListQuery {
    /// The tenant whose items to list (a 26-character ULID).
    tenant_id: String,
    /// How many items to return. **Absent means unpaged**: the whole item master, as an array —
    /// which is what the menu compiler and five of the six console pickers ask for.
    #[serde(default)]
    limit: Option<String>,
    /// How many items to skip. Only meaningful with `limit`.
    #[serde(default)]
    offset: Option<String>,
    /// A case-insensitive substring the item's name or any per-locale name must contain.
    ///
    /// Only meaningful with `limit`: on the unpaged read it is ignored, and the route says so by
    /// refusing rather than by quietly returning the whole master.
    #[serde(default)]
    q: Option<String>,
    /// Which order to return the page in — one of
    /// [`ItemSort::tokens`](crate::catalog::ItemSort::tokens). Only meaningful with `limit`.
    #[serde(default)]
    sort: Option<String>,
    /// `asc` or `desc`, inverting `sort`'s natural direction. Only meaningful with `limit`.
    #[serde(default)]
    order: Option<String>,
}

/// `GET /admin/media`: the tenant, plus the optional paging bounds.
///
/// The `limit`/`offset` pair is repeated per route for the reason `pos-cloud`'s `VoucherListQuery`
/// gives.
#[derive(Debug, Clone, Deserialize)]
struct MediaListQuery {
    /// The tenant whose assets to list (a 26-character ULID).
    tenant_id: String,
    /// How many summaries to return. **Absent means unpaged**: every asset, as an array — which is
    /// what the item image picker asks for.
    #[serde(default)]
    limit: Option<String>,
    /// How many summaries to skip. Only meaningful with `limit`.
    #[serde(default)]
    offset: Option<String>,
}
