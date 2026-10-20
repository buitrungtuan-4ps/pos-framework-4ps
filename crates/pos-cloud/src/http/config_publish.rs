// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Publishing configuration: the generic preview, version history and rollback, and config
//! releases (ADR-0069, ADR-0125).
//!
//! The shared machinery — a tree loaded for a conditional write, the nodes written onto a layer, and
//! the prerequisite and settings checks a write runs — is `pos_cloud_config::publish`, imported here.

// The machinery every config publish shares lives in pos-cloud-config; imported here, so the
// other areas' routes and this module's own keep reaching it through `super`.
pub(super) use pos_cloud_config::publish::{
    LoadedTree, load_tree_for_write, publish_config_nodes, publish_nodes,
};
pub(crate) use pos_cloud_config::publish::{publish_config_nodes_with, strip_tree_version};

mod config_releases;
mod if_match;
mod preview;
// `pub(super)`: the redaction unit tests in `http.rs` reach the priced-node list and the credential
// strip.
pub(super) mod redaction;
mod release_actions;
mod release_create;
mod store_config;
mod versions;

pub use config_releases::config_release_router;
pub use preview::config_preview_router;
// What the other areas' routes, the router and the release schedule use.
pub(super) use config_releases::{
    ReleaseNodeRequest, ReleaseState, ScheduleReleaseRequest, release_error_response,
};
pub(super) use if_match::{if_match_config, if_match_lease};
pub(super) use preview::preview_config_node;
pub(super) use redaction::{staff_redacted_for, without_prices};
pub(super) use release_actions::release_report_response;
// The raw config-tree write and read the main router mounts.
pub(super) use release_create::{
    release_target_stores, release_target_timezones, unresolvable_moment,
};
pub(super) use store_config::{admin_config_effective, admin_config_publish};
pub(super) use versions::{
    admin_config_nodes, admin_config_rollback, admin_config_version_effective,
    admin_config_versions,
};
// The handlers the console's OpenAPI document names.
pub(crate) use preview::__path_admin_preview_config_node;
pub(crate) use release_actions::{
    __path_admin_cancel_release, __path_admin_list_releases, __path_admin_read_release,
    __path_admin_schedule_release,
};
pub(crate) use release_create::__path_admin_create_release;
pub(crate) use store_config::{__path_admin_config_effective, __path_admin_config_publish};
pub(crate) use versions::{
    __path_admin_config_nodes, __path_admin_config_rollback, __path_admin_config_version_effective,
    __path_admin_config_versions,
};
