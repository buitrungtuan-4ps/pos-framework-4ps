// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the routes that read or write a store's configuration share: the tenant query, the published
//! version's answer, the refusals for an unpublished store or a bad status or level, and the read of
//! a node as the store runs it.
//!
//! Moved out of `pos-cloud`'s `http.rs` (ADR-0170 decision 4). `pos-cloud`'s `http` module imports
//! them, so `crate::http::read_store_node_as_run` and the rest keep their paths there.

use axum::response::Response;
use pos_cloud_api::console::mint_ulid;
use pos_cloud_api::refusal::enum_refusal;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use serde::Deserialize;

use crate::config_tree::{CapabilityValidator, ConfigLevel, ConfigTree, ConfigTreeStore};
use crate::publish::{only_settings, strip_tree_version};
use crate::registry::EntityStatus;

/// A `?tenant_id=` query for the tenant-scoped listings.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryTenantQuery {
    /// The tenant whose entities to list (a 26-character ULID).
    pub tenant_id: String,
}

/// The version id a successful publish produced.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublishedConfig {
    /// The new config version id (a ULID).
    pub config_version_id: String,
}

/// Maps a config-tree store failure to a retryable `503`, logging the detail rather than leaking it.
pub fn config_store_error_response(error: &crate::config_tree::ConfigStoreError) -> Response {
    tracing::error!(%error, "a config-tree store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the configuration service is unavailable",
    )
}

/// Parses a config-tree level from its path segment, or `None` for an unknown one.
pub fn parse_config_level(level: &str) -> Option<ConfigLevel> {
    ConfigLevel::ORDER
        .into_iter()
        .find(|candidate| candidate.as_str() == level)
}

/// Mints a config version id from `now_ms`, or `None` if the OS entropy source is unavailable.
pub fn mint_version_id(now_ms: i64) -> Option<ConfigVersionId> {
    mint_ulid(now_ms).map(ConfigVersionId::new)
}

/// Parses a status word from a request body; `None` (a `400`) for anything but the two known values.
pub fn parse_entity_status(value: &str) -> Option<EntityStatus> {
    EntityStatus::ALL
        .iter()
        .copied()
        .find(|status| status.as_str() == value)
}

/// The refusal for a `status` field that is not an [`EntityStatus`] token.
///
/// Eighteen routes read this one field, which is why it gets a name of its own: the eighteen call
/// sites now say *which* refusal they mean rather than each restating the accepted set.
pub fn entity_status_refusal() -> Response {
    enum_refusal(
        "status",
        EntityStatus::ALL.iter().map(|status| status.as_str()),
    )
}

/// The refusal for a store's configuration that has not been published yet.
///
/// Its own name because six routes answer it and it is not the `"no such X"` shape: the store
/// exists, and so does its config tree; what is missing is a *published version* of it.
pub fn no_published_configuration() -> Response {
    api_error(
        ErrorStatus::NotFound,
        "the store has no published configuration",
    )
}

/// A store's `node_key` node as the store runs it, without the node's settings fields, or `null`
/// when it runs none.
///
/// Read from the tree's four layers composed ([`ConfigTree::effective`]), not from the Store layer
/// alone. A rollback writes the restored version onto the Tenant layer and empties the others
/// ([`ConfigTree::restore_with`]), so a Store-layer read answered `null` for every node the store
/// still ran: the console's forms went blank, and its checks refused a store that held the node. A
/// node a tenant or brand layer sets is read with the store's own, because the store runs it.
///
/// The fields the settings register puts on the node (`pos_proto::settings::register`) are left
/// out. They are written on the Tenant layer by the settings compile, and they are the settings
/// store's to decide (ADR-0160 decision 3): a form that read one and saved the node back would pin
/// it on the Store layer, where it would shadow every later settings change for the store. A node
/// that holds nothing but settings answers `null`, as [`check_prerequisites`] counts it: a `locale`
/// holding only `tax_rounding` is not the locale a menu waits for.
pub async fn read_store_node_as_run<Cfg>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    store_id: StoreId,
    node_key: &str,
) -> Result<serde_json::Value, Response>
where
    Cfg: ConfigTreeStore,
{
    match config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    {
        Ok(Some(state)) => {
            let composed = ConfigTree::from_state(store_id, CapabilityValidator, state).effective();
            Ok(node_as_run(node_key, composed.get(node_key)))
        }
        Ok(None) => Ok(serde_json::Value::Null),
        Err(error) => Err(config_store_error_response(&error)),
    }
}

/// The answer [`read_store_node_as_run`] gives for `node`, the store's `node_key` as its layers
/// compose it: the node without its settings fields, and `null` for a node the store does not run
/// or one that holds nothing but settings. A node that is not an object is answered as it is.
fn node_as_run(node_key: &str, node: Option<&serde_json::Value>) -> serde_json::Value {
    match node {
        None => serde_json::Value::Null,
        Some(value) if only_settings(node_key, value) => serde_json::Value::Null,
        Some(serde_json::Value::Object(fields)) => {
            let register = pos_proto::settings::register();
            serde_json::Value::Object(
                fields
                    .iter()
                    .filter(|(field, _)| {
                        !register.iter().any(|setting| {
                            setting.node == node_key && setting.field == field.as_str()
                        })
                    })
                    .map(|(field, value)| (field.clone(), value.clone()))
                    .collect(),
            )
        }
        Some(other) => other.clone(),
    }
}

#[cfg(test)]
mod node_as_run_tests {
    //! What a store's node reads as once its layers are composed ([`super::node_as_run`]).
    use super::node_as_run;
    use serde_json::{Value, json};

    #[test]
    fn a_node_reads_without_its_settings_and_one_of_settings_alone_reads_as_none() {
        let locale = json!({
            "currency_code": "VND",
            "timezone": "Asia/Ho_Chi_Minh",
            "tax_rounding": "TAX_ROUNDING_DOWN",
        });
        assert_eq!(
            node_as_run("locale", Some(&locale)),
            json!({ "currency_code": "VND", "timezone": "Asia/Ho_Chi_Minh" })
        );
        for (node, settings) in [
            ("locale", json!({ "tax_rounding": "TAX_ROUNDING_DOWN" })),
            ("qr", json!({ "table_order": "TABLE_ORDER_JOIN" })),
        ] {
            assert_eq!(node_as_run(node, Some(&settings)), Value::Null, "{node}");
        }
        assert_eq!(node_as_run("locale", None), Value::Null);
    }

    #[test]
    fn a_node_with_no_settings_fields_reads_as_it_is_composed() {
        // `table_order` is a setting of the `qr` node, not of `channels`.
        let channels = json!({ "enabled": ["SALES_CHANNEL_DINE_IN"], "table_order": 1 });
        assert_eq!(node_as_run("channels", Some(&channels)), channels);
        // An empty node is a node, as the publish check counts it, and a node that is not an
        // object, `null` included, is answered as it is.
        for node in [json!({}), Value::Null, json!([1, 2]), json!("text")] {
            assert_eq!(node_as_run("qr", Some(&node)), node, "{node}");
        }
    }
}
