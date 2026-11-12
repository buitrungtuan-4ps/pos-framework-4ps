// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The integration routes: the vendor catalogue, connections, gateway endpoints, and the
//! integrations publish. Out of `routes.rs`, which is at its size limit (ADR-0170 decision 3).

use std::sync::Arc;

use axum::Router;
use pos_cloud::clock::SystemClock;
use pos_cloud::http;
use pos_cloud::relay::OrderRelay;

use super::routes::RouteDeps;

/// The vendor catalogue, connections, and integrations.
pub(super) fn integration_routes(deps: &RouteDeps<'_>) -> Router {
    let RouteDeps {
        store,
        config,
        audit,
        providers,
        cell,
        relay_wake,
        ..
    } = *deps;
    Router::new()
        // The vendor catalogue (ADR-0153): every provider compiled into this binary, with the
        // settings schema the console draws a connection form from.
        .merge(http::provider_catalogue_router(
            Arc::clone(providers),
            store.admin(),
            SystemClock,
        ))
        // A tenant's vendor connections (ADR-0153): per-record CRUD, every write checked against
        // the provider's schema and every secret sealed under integration_secret before it is
        // stored. Reads answer which secrets are set, never their values.
        .merge(http::connection_router(
            store.connections(),
            Arc::clone(providers),
            config.integration_secret.clone(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
            // Each connection's processing country is compared with this (ADR-0179 C7).
            cell.cell().home_country,
        ))
        // A tenant's gateway endpoints (ADR-0198 decisions 2 to 5): the https targets a store's
        // lookups name by id, each URL vetted by the webhook guard and each secret header sealed
        // under the same integration_secret before it is stored.
        .merge(http::gateway_endpoint_router(
            store.gateway_endpoints(),
            config.integration_secret.clone(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
        // A delivery vendor's own webhook (ADR-0083 decision 3): verified under the secret sealed on
        // the tenant's store-scoped connection, mapped through its item map, and submitted to the
        // same relay `POST /v1/orders` uses. One connector per vendor; the sandbox one for now.
        .merge(pos_cloud::connectors::route::connector_router(
            store.connections(),
            OrderRelay::new(
                store.store_directory(),
                store.config_trees(),
                store.order_queue(),
                SystemClock,
                relay_wake.clone(),
            ),
            SystemClock,
            vec![Arc::new(pos_cloud::connectors::demo::DemoConnector)],
            config.integration_secret.clone(),
        ))
        // Sending a store the connections that serve it (ADR-0153 decision 5): its own, its
        // brand's, its tenant's, resolved into the `integrations` node on its Store layer. A
        // publish like every other node, so it is console.config.publish, not the authoring
        // permission.
        .merge(http::config_integrations_router(
            store.connections(),
            Arc::clone(providers),
            store.registry(),
            store.config_trees(),
            store.admin(),
            SystemClock,
            Arc::clone(audit),
        ))
}
