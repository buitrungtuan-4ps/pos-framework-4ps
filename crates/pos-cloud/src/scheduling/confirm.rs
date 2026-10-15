// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a scheduled node makes a store run, and whether that is a value that needs a recorded
//! confirmation the store does not run now
//! ([ADR-0169](../../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)
//! decision 3).

use pos_proto::settings::Setting;

use crate::config_tree::ConfigTreeState;
use crate::config_tree::merge::merge_layers;
use crate::confirmed_values::Unconfirmed;

use super::ScheduledPublish;

/// The Store layer a scheduled `node_key` = `node_value` makes on a tree that is `state`, and the
/// node keys it sets.
///
/// A snapshotted `qr` or `channels` node, or a capability flag, is applied as an immediate publish
/// of it is: with what QR ordering's switch makes follow it, composed on the tree as it stands
/// ([`crate::qr_ordering::align`]).
fn store_layer_with(
    state: Option<&ConfigTreeState>,
    node_key: &str,
    node_value: &serde_json::Value,
) -> (serde_json::Value, Vec<String>) {
    let mut store_layer = state.map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if !store_layer.is_object() {
        store_layer = serde_json::Value::Object(serde_json::Map::new());
    }
    let nodes = crate::qr_ordering::align(state, vec![(node_key.to_owned(), node_value.clone())]);
    let node_keys: Vec<String> = nodes.iter().map(|(key, _)| key.clone()).collect();
    if let serde_json::Value::Object(map) = &mut store_layer {
        for (key, value) in nodes {
            map.insert(key, value);
        }
    }
    (store_layer, node_keys)
}

/// The Store layer a due `publish` makes on a store whose tree is `state`, and the node keys it sets;
/// or, for a node that would make the store run a value that needs a recorded confirmation, which it
/// does not run now, the reason the activator records, as it does for a node that no longer
/// validates.
pub(super) fn composed(
    register: &[Setting],
    state: Option<&ConfigTreeState>,
    publish: &ScheduledPublish,
) -> Result<(serde_json::Value, Vec<String>), String> {
    check_confirmations(register, state, &publish.node_key, &publish.node_value)
        .map_err(|unconfirmed| format!("the scheduled node is invalid: {unconfirmed}"))?;
    Ok(store_layer_with(
        state,
        &publish.node_key,
        &publish.node_value,
    ))
}

/// Whether a scheduled `node_key` = `node_value` would make a store whose tree is `state` run a value
/// that needs a recorded confirmation, which it does not run now
/// ([ADR-0169](../../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)
/// decision 3).
///
/// Asked when a release is scheduled, against each store, and again by the activator when the row
/// comes due, since the store may have changed meanwhile. Both compose the node as the activator
/// applies it.
///
/// # Errors
///
/// [`Unconfirmed`] for such a value.
pub fn check_confirmations(
    register: &[Setting],
    state: Option<&ConfigTreeState>,
    node_key: &str,
    node_value: &serde_json::Value,
) -> Result<(), Unconfirmed> {
    let empty = serde_json::Value::Object(serde_json::Map::new());
    let (store_layer, _) = store_layer_with(state, node_key, node_value);
    let (before, after) = match state {
        Some(existing) => (
            existing.effective(),
            merge_layers(&[
                &existing.layers[0],
                &existing.layers[1],
                &store_layer,
                &existing.layers[3],
            ]),
        ),
        None => (
            empty.clone(),
            merge_layers(&[&empty, &empty, &store_layer, &empty]),
        ),
    };
    crate::confirmed_values::check(register, &before, &after)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        FakeConfigTrees, FakeReleases, FakeScheduled, new_publish, store, tenant,
    };
    use super::super::{NewScheduledPublish, ScheduledPublishStatus, ScheduledPublishStore};
    use crate::config_tree::ConfigTreeStore;
    use pos_proto::settings::{Confirm, Setting, SettingScope, SettingShape};

    /// A register with one setting whose `OWNER_ONE_PERSON` needs confirming: the fixture
    /// `pos_cloud_config::confirmed_values`' own tests build, repeated here because a dependency's
    /// test code is not compiled into this crate's tests.
    fn confirming_register() -> Vec<Setting> {
        vec![Setting {
            node: "example",
            field: "drawer_owner",
            shape: SettingShape::Choice {
                values: vec!["OWNER_SHARED", "OWNER_ONE_PERSON"],
                default: "OWNER_SHARED",
                preset: None,
            },
            scopes: &[SettingScope::Tenant, SettingScope::Store],
            since: "0.15.0",
            confirm: Some(Confirm {
                value: "OWNER_ONE_PERSON",
                notice: "settings.item.example.drawer_owner.confirm",
            }),
            summary: "Whose a drawer is.",
        }]
    }

    /// A scheduled `example` node setting the drawer's owner, which [`confirming_register`] says needs
    /// a recorded confirmation.
    fn drawer_owner_publish(id: &str, owner: &str, other: u32) -> NewScheduledPublish {
        NewScheduledPublish {
            node_key: "example".to_owned(),
            node_value: serde_json::json!({ "drawer_owner": owner, "other": other }),
            ..new_publish(id, 1_000)
        }
    }

    /// ADR-0169 decision 3: a scheduled node that would make the store run a value that needs a
    /// recorded confirmation, which it did not run before, is not applied. Nothing is saved, and the
    /// row says why, as it does for a node that no longer validates.
    #[tokio::test]
    async fn a_due_publish_that_would_run_an_unconfirmed_value_is_not_applied() {
        let scheduled = FakeScheduled::default();
        let config_trees = FakeConfigTrees::default();
        scheduled
            .schedule(&drawer_owner_publish(
                "unconfirmed-1",
                "OWNER_ONE_PERSON",
                1,
            ))
            .await
            .expect("schedule");

        let applied = super::super::pass_against(
            &scheduled,
            &config_trees,
            &FakeReleases::default(),
            &confirming_register(),
            5_000,
        )
        .await
        .expect("a refused row is not a failed pass");
        assert_eq!(applied, 0);
        assert!(
            config_trees
                .load(tenant(), store())
                .await
                .expect("load")
                .is_none(),
            "nothing was saved"
        );
        let rows = scheduled
            .list_for_store(tenant(), store())
            .await
            .expect("list");
        let row = rows.first().expect("the row is still there");
        assert_eq!(row.status, ScheduledPublishStatus::Pending);
        let failure = row.failure.clone().unwrap_or_default();
        assert!(
            failure.starts_with("the scheduled node is invalid")
                && failure.contains("example.drawer_owner"),
            "{failure}"
        );
    }

    /// A store that already runs the value, confirmed on Settings, takes a node that keeps it.
    #[tokio::test]
    async fn a_due_publish_that_keeps_a_value_the_store_already_runs_is_applied() {
        let scheduled = FakeScheduled::default();
        let config_trees = FakeConfigTrees::default();
        let mut running =
            crate::config_tree::ConfigTree::new(store(), crate::config_tree::CapabilityValidator)
                .state();
        running.layers[0] =
            serde_json::json!({ "example": { "drawer_owner": "OWNER_ONE_PERSON" } });
        config_trees
            .save(tenant(), store(), &running, None)
            .await
            .expect("seed");
        scheduled
            .schedule(&drawer_owner_publish("kept-1", "OWNER_ONE_PERSON", 2))
            .await
            .expect("schedule");

        let applied = super::super::pass_against(
            &scheduled,
            &config_trees,
            &FakeReleases::default(),
            &confirming_register(),
            5_000,
        )
        .await
        .expect("pass");
        assert_eq!(applied, 1);
    }
}
