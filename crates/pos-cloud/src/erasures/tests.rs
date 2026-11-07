// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use axum::http::StatusCode;
use pos_proto::erasures::MAX_ERASURES;
use pos_proto::ids::SubjectId;
use pos_proto::ulid::Ulid;
use serde_json::json;

use super::{append_erasure, requested};
use crate::config_tree::ConfigTreeState;

fn subject(n: u128) -> SubjectId {
    SubjectId::new(Ulid::from_u128(n))
}

/// A tree whose Store layer names `ids`, and whose Tenant layer names `tenant_ids`.
fn tree(tenant_ids: &[u128], ids: &[u128]) -> ConfigTreeState {
    let listed = |ids: &[u128]| {
        let ids: Vec<String> = ids.iter().map(|n| subject(*n).to_string()).collect();
        json!({ "erasures": { "subject_ids": ids } })
    };
    ConfigTreeState {
        layers: [
            if tenant_ids.is_empty() {
                json!({})
            } else {
                listed(tenant_ids)
            },
            json!({}),
            if ids.is_empty() {
                json!({})
            } else {
                listed(ids)
            },
            json!({}),
        ],
        history: Vec::new(),
        k: 8,
    }
}

fn ids_of(nodes: &[(String, serde_json::Value)]) -> Vec<String> {
    let (key, node) = nodes.first().expect("one node");
    assert_eq!(key, "erasures");
    serde_json::from_value(node["subject_ids"].clone()).expect("a list of ids")
}

#[test]
fn an_erasure_is_appended_once_to_what_the_store_runs() {
    let before = tree(&[], &[1]);
    let nodes = append_erasure(Some(&before), subject(2)).expect("appends");
    assert_eq!(
        ids_of(&nodes),
        vec![subject(1).to_string(), subject(2).to_string()]
    );
    let again = append_erasure(Some(&before), subject(1)).expect("idempotent");
    assert_eq!(ids_of(&again), vec![subject(1).to_string()]);
    let first = append_erasure(None, subject(3)).expect("a store with no tree");
    assert_eq!(ids_of(&first), vec![subject(3).to_string()]);
}

#[test]
fn the_node_is_read_composed_so_a_rolled_back_store_layer_loses_nothing() {
    assert_eq!(requested(Some(&tree(&[4], &[]))), vec![subject(4)]);
}

#[test]
fn an_erasure_that_would_overflow_the_node_is_refused() {
    let full: Vec<u128> = (1..=u128::try_from(MAX_ERASURES).expect("fits")).collect();
    let before = tree(&[], &full);
    let refused = append_erasure(Some(&before), subject(0xFFFF)).expect_err("over the cap");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(
        append_erasure(Some(&before), subject(1)).is_ok(),
        "an id already on a full node is still idempotent"
    );
}
