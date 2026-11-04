// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /api/package`: which package version this store runs, and whether the last configuration
//! was refused for its package ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md)
//! decision 6). Read-only: a store's package is chosen in the console, never here (roadmap-v3
//! D18), so this is what the edge's diagnostics show (B7.5).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;

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
    let running = edge
        .session()
        .package
        .as_ref()
        .map(|content| RunningPackage {
            package_version_id: content.package_version_id.to_string(),
            version_label: content.version_label.clone(),
            min_edge_version: content.min_edge_version.to_string(),
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
}
