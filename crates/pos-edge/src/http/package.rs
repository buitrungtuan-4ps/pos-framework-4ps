// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /api/package`: which package version this store runs, its flows and reactions for the till
//! to run, and
//! whether the last configuration was refused for its package ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md)
//! decision 6). Read-only: a store's package is chosen in the console, never here (roadmap-v3
//! D18), so this is what the edge's diagnostics show (B7.5).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::flow::Flow;
use pos_proto::package::Variant;
use pos_proto::reaction::Reaction;

use crate::app::Edge;

/// The store's package, as the edge runs it.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct PackageResponse {
    /// The version running, or `null` for the default package of this release.
    #[cfg_attr(feature = "openapi", schema(required = true))]
    running: Option<RunningPackage>,
    /// The last configuration refused for its package, or `null` when the latest was applied.
    #[cfg_attr(feature = "openapi", schema(required = true))]
    refused: Option<RefusedResponse>,
}

/// The version running.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct RunningPackage {
    package_version_id: String,
    /// The version's name in the console.
    version_label: String,
    /// The oldest edge release that runs it.
    min_edge_version: String,
    /// The version's flows, every variant of each in the version's order, for the till to run
    /// (ADR-0192 decision 3); the edge re-checks their guards. Left out when the version has none,
    /// so a till that predates the field reads the same answer as before.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    flows: Vec<Variant<Flow>>,
    /// The version's reactions, every variant of each in the version's order, for the till to run
    /// under the signed-in person's permissions (ADR-0193 decision 5); the edge checks each command
    /// a reaction sends as any other. Left out when the version has none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reactions: Vec<Variant<Reaction>>,
    /// The tenant's labels for the translation keys this version names, `key → { locale → text }`,
    /// from the grid the store pulled (ADR-0043): every key with `en`. A key the grid lacks is not
    /// here. Left out when there are none.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    labels: BTreeMap<String, BTreeMap<String, String>>,
}

/// A configuration refused for its package; the store kept the one before it.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct RefusedResponse {
    config_version_id: String,
    /// The package version it carried, when that could be read.
    #[cfg_attr(feature = "openapi", schema(required = true))]
    package_version_id: Option<String>,
    /// `PACKAGE_MALFORMED`, `PACKAGE_INVALID` or `EDGE_TOO_OLD`.
    reason: &'static str,
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/package",
    security(("till_session" = [])),
    responses(
        (status = 200, body = PackageResponse, description = "The package version this store runs (null: the default package of its \
                                      release), and the last configuration refused for its package (null: the \
                                      latest was applied)"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
    ),
    tag = "published configuration",
))]
/// `GET /api/package` — the package this store runs and its last refusal.
pub(crate) async fn package_standing<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let grid = edge.label_grid();
    let running = edge
        .session()
        .package
        .as_ref()
        .map(|content| RunningPackage {
            package_version_id: content.package_version_id.to_string(),
            version_label: content.version_label.clone(),
            min_edge_version: content.min_edge_version.to_string(),
            flows: content.flows.clone(),
            reactions: content.reactions.clone(),
            labels: if grid.is_empty() {
                BTreeMap::new()
            } else {
                let mut named = Vec::new();
                strings_in(
                    &serde_json::to_value(content.as_ref()).unwrap_or_default(),
                    &mut named,
                );
                grid.subset(named.iter().map(String::as_str))
            },
        });
    let refused = edge
        .package_standing()
        .refused()
        .map(|refused| RefusedResponse {
            config_version_id: refused.config_version_id,
            package_version_id: refused.package_version_id,
            reason: refused.refusal.reason(),
        });
    (StatusCode::OK, Json(PackageResponse { running, refused })).into_response()
}

/// Every string `value` holds, at any depth: what a version can name a translation key with.
fn strings_in(value: &serde_json::Value, found: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => found.push(text.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|item| strings_in(item, found)),
        serde_json::Value::Object(fields) => {
            fields.values().for_each(|field| strings_in(field, found));
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::extract::State;
    use pos_fakes::FakeStore;
    use pos_proto::package::PackageContent;

    use super::package_standing;
    use crate::InMemoryReceipts;
    use crate::app::{Edge, EdgeSession, PackageRefusal, RefusedPackage, StoreIdentity};

    const V1: &str = "01J00000000000000000000V01";

    async fn body(edge: Arc<Edge<FakeStore>>) -> serde_json::Value {
        let response = package_standing(State(edge)).await;
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .expect("read the body");
        serde_json::from_slice(&bytes).expect("a JSON body")
    }

    /// A store with no package and no refusal says so; one running a version names it, and a
    /// refused configuration is shown beside it.
    #[tokio::test]
    async fn the_route_answers_the_running_version_and_the_last_refusal() {
        let edge = Arc::new(
            Edge::new(
                FakeStore::default(),
                StoreIdentity::for_store(pos_proto::ids::StoreId::new(
                    pos_proto::ulid::Ulid::from_u128(7),
                )),
                EdgeSession::bootstrap(),
                Arc::new(InMemoryReceipts::new()),
            )
            .expect("an edge"),
        );
        assert_eq!(
            body(Arc::clone(&edge)).await,
            serde_json::json!({ "running": null, "refused": null })
        );

        let mut session = (*edge.session()).clone();
        session.package = Some(Arc::new(
            serde_json::from_value::<PackageContent>(serde_json::json!({
                "package_id": "01J00000000000000000000P01",
                "package_version_id": V1,
                "version_label": "March",
                "min_edge_version": "0.14.1",
            }))
            .expect("content"),
        ));
        edge.apply_session(session);
        edge.package_standing().record(Some(RefusedPackage {
            config_version_id: "C2".to_owned(),
            package_version_id: None,
            refusal: PackageRefusal::Malformed,
        }));
        assert_eq!(
            body(edge).await,
            serde_json::json!({
                "running": {
                    "package_version_id": V1,
                    "version_label": "March",
                    "min_edge_version": "0.14.1",
                },
                "refused": {
                    "config_version_id": "C2",
                    "package_version_id": null,
                    "reason": "PACKAGE_MALFORMED",
                },
            })
        );
    }

    /// A version's flows ride the answer for the till to run, every variant in order.
    #[tokio::test]
    async fn the_running_version_carries_its_flows() {
        let edge = Arc::new(
            Edge::new(
                FakeStore::default(),
                StoreIdentity::for_store(pos_proto::ids::StoreId::new(
                    pos_proto::ulid::Ulid::from_u128(7),
                )),
                EdgeSession::bootstrap(),
                Arc::new(InMemoryReceipts::new()),
            )
            .expect("an edge"),
        );
        let flows = serde_json::json!([{ "body": {
            "flow_id": "dine_in",
            "start_action_id": "sales.table.seat",
            "steps": [{ "step_id": "guests", "action_id": "sales.table.seat",
                        "kind": "STEP_KIND_OPTIONAL" }],
        }}]);
        let mut session = (*edge.session()).clone();
        session.package = Some(Arc::new(
            serde_json::from_value::<PackageContent>(serde_json::json!({
                "package_id": "01J00000000000000000000P01",
                "package_version_id": V1,
                "version_label": "March",
                "min_edge_version": "0.14.1",
                "flows": flows,
            }))
            .expect("content"),
        ));
        edge.apply_session(session);
        assert_eq!(body(edge).await["running"]["flows"], flows);
    }

    /// A version's reactions ride the answer beside its flows.
    #[tokio::test]
    async fn the_running_version_carries_its_reactions() {
        let edge = Arc::new(
            Edge::new(
                FakeStore::default(),
                StoreIdentity::for_store(pos_proto::ids::StoreId::new(
                    pos_proto::ulid::Ulid::from_u128(7),
                )),
                EdgeSession::bootstrap(),
                Arc::new(InMemoryReceipts::new()),
            )
            .expect("an edge"),
        );
        let reactions = serde_json::json!([{ "body": {
            "reaction_id": "check_on_bill",
            "when": "REACTION_TRIGGER_BILL_OPENED",
            "then": { "action_id": "billing.check.print" },
        }}]);
        let mut session = (*edge.session()).clone();
        session.package = Some(Arc::new(
            serde_json::from_value::<PackageContent>(serde_json::json!({
                "package_id": "01J00000000000000000000P01",
                "package_version_id": V1,
                "version_label": "March",
                "min_edge_version": "0.14.1",
                "reactions": reactions,
            }))
            .expect("content"),
        ));
        edge.apply_session(session);
        let answer = body(edge).await;
        assert_eq!(answer["running"]["reactions"], reactions);
        assert!(answer["running"].get("flows").is_none());
    }

    /// A store holding its tenant's grid answers the labels of the keys the version names, and no
    /// other.
    #[tokio::test]
    async fn the_route_answers_the_labels_the_version_names() {
        let mut session = EdgeSession::bootstrap();
        session.package = Some(Arc::new(
            serde_json::from_value::<PackageContent>(serde_json::json!({
                "package_id": "01J00000000000000000000P01",
                "package_version_id": V1,
                "version_label": "March",
                "min_edge_version": "0.14.1",
                "data": {"object_types": [{"object_type_id": "01J00000000000000000000B01",
                    "label_key": "data.reservation", "fields": []}]},
            }))
            .expect("content"),
        ));
        let edge = Arc::new(
            Edge::new(
                FakeStore::default(),
                StoreIdentity::for_store(pos_proto::ids::StoreId::new(
                    pos_proto::ulid::Ulid::from_u128(7),
                )),
                session,
                Arc::new(InMemoryReceipts::new()),
            )
            .expect("an edge"),
        );
        let grid = serde_json::from_value(serde_json::json!({
            "data.reservation": {"en": "Reservation", "vi": "Đặt bàn"},
            "menu.item.unused": {"en": "Not named by the version"},
        }))
        .expect("a grid");
        edge.apply_grid(crate::labels::LabelGrid::bounded(grid).expect("within bounds"));
        assert_eq!(
            body(edge).await["running"]["labels"],
            serde_json::json!({"data.reservation": {"en": "Reservation", "vi": "Đặt bàn"}})
        );
    }
}
