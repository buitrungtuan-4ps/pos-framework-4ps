// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge applies a pulled `package` node with last-known-good
//! ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 6): a
//! package it can run is on the session; one it cannot refuses the whole document, the store keeps
//! the previous one, the refusal is kept for the heartbeat, and the version is held so it is not
//! pulled again every interval.
//!
//! This binary is not a release build, so its release is `0.0.0`: a version needing `0.0.0` runs,
//! and any later one needs a newer edge.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use pos_edge::config_client::{ConfigClient, ConfigTransport, ConfigTransportError, SyncedConfig};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};

const V1: &str = "01J00000000000000000000V01";
const V2: &str = "01J00000000000000000000V02";

fn config_version(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

fn edge() -> Arc<Edge<FakeStore>> {
    Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7))),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    )
}

/// A document locking the store to `version`, which needs `min_edge_version`, beside a `counter`
/// node whose value shows whether the document was applied.
fn document(version: &str, min_edge_version: &str, walk_in: &str) -> Value {
    json!({
        "counter": { "walk_in_channel": walk_in },
        "package": {
            "mode": "PACKAGE_MODE_LOCKED",
            "locked_version_id": version,
            "content": {
                "package_id": "01J00000000000000000000P01",
                "package_version_id": version,
                "version_label": "March",
                "min_edge_version": min_edge_version,
            },
        },
    })
}

/// A cloud that publishes its documents in turn and records the held version each pull sent.
struct Cloud {
    documents: Mutex<VecDeque<SyncedConfig>>,
    held: Mutex<Vec<Option<String>>>,
}

impl Cloud {
    fn new(documents: Vec<(String, Value)>) -> Self {
        Self {
            documents: Mutex::new(
                documents
                    .into_iter()
                    .map(|(config_version_id, document)| SyncedConfig {
                        config_version_id,
                        document,
                    })
                    .collect(),
            ),
            held: Mutex::new(Vec::new()),
        }
    }
}

impl ConfigTransport for &Cloud {
    async fn fetch(
        &self,
        held_version: Option<&str>,
    ) -> Result<Option<SyncedConfig>, ConfigTransportError> {
        self.held
            .lock()
            .expect("held lock")
            .push(held_version.map(str::to_owned));
        Ok(self.documents.lock().expect("documents lock").pop_front())
    }
}

fn running(edge: &Edge<FakeStore>) -> Option<String> {
    edge.session()
        .package
        .as_ref()
        .map(|content| content.package_version_id.to_string())
}

fn walk_in(edge: &Edge<FakeStore>) -> String {
    edge.session().counter.walk_in_channel.as_wire().to_owned()
}

#[test]
fn a_package_the_edge_runs_is_applied_and_one_it_cannot_refuses_the_whole_document() {
    let edge = edge();
    let cloud = Cloud::new(vec![
        (
            config_version(1),
            document(V1, "0.0.0", "WALK_IN_CHANNEL_DINE_IN"),
        ),
        (
            config_version(2),
            document(V2, "0.15.0", "WALK_IN_CHANNEL_ASK"),
        ),
        (
            config_version(3),
            document(V2, "0.0.0", "WALK_IN_CHANNEL_ASK"),
        ),
    ]);
    let client = ConfigClient::new(&cloud, edge.clone(), None);
    let standing = edge.package_standing();

    let applied = run_ready(client.pump_once()).expect("pump");
    assert_eq!(applied, Some(config_version(1)));
    assert_eq!(running(&edge).as_deref(), Some(V1));
    assert_eq!(walk_in(&edge), "WALK_IN_CHANNEL_DINE_IN");
    assert_eq!(standing.refused(), None);

    let refused = run_ready(client.pump_once()).expect("pump");
    assert_eq!(refused, None, "nothing applied");
    assert_eq!(
        running(&edge).as_deref(),
        Some(V1),
        "the previous package stays"
    );
    assert_eq!(
        walk_in(&edge),
        "WALK_IN_CHANNEL_DINE_IN",
        "and so does every other node of the previous document"
    );
    let record = standing
        .refused()
        .expect("the refusal is kept for the heartbeat");
    assert_eq!(record.config_version_id, config_version(2));
    assert_eq!(record.package_version_id.as_deref(), Some(V2));
    assert_eq!(record.refusal.reason(), "EDGE_TOO_OLD");

    let applied = run_ready(client.pump_once()).expect("pump");
    assert_eq!(applied, Some(config_version(3)));
    assert_eq!(running(&edge).as_deref(), Some(V2));
    assert_eq!(walk_in(&edge), "WALK_IN_CHANNEL_ASK");
    assert_eq!(
        standing.refused(),
        None,
        "a document applied clears the refusal"
    );

    let held = cloud.held.lock().expect("held lock").clone();
    assert_eq!(
        held,
        vec![None, Some(config_version(1)), Some(config_version(2))],
        "the refused version is held, so the next pull asks for something newer"
    );
}

#[test]
fn a_document_that_stops_pinning_returns_the_default_package() {
    let edge = edge();
    let cloud = Cloud::new(vec![
        (
            config_version(1),
            document(V1, "0.0.0", "WALK_IN_CHANNEL_DINE_IN"),
        ),
        (
            config_version(2),
            json!({ "package": { "mode": "PACKAGE_MODE_UNSPECIFIED" } }),
        ),
    ]);
    let client = ConfigClient::new(&cloud, edge.clone(), None);
    run_ready(client.pump_once()).expect("pump");
    assert_eq!(running(&edge).as_deref(), Some(V1));
    run_ready(client.pump_once()).expect("pump");
    assert_eq!(
        running(&edge),
        None,
        "no pin: the default package of this release"
    );
}
