// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The generated OpenAPI document for what a store box calls on the cloud, `docs/openapi-device.json`
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 1).
//!
//! A third audience beside the integrator's `/v1` ([`crate::openapi`]) and the console's `/admin`
//! ([`crate::openapi_admin`]): the edge's own cloud client. It covers the store-facing `/sync`
//! surface, which a box reaches with its device credential or its store's scoped key, and the two
//! ways a box first comes to hold one, `/activate` and `/claim`, which carry no credential at all,
//! and `/.well-known/pos-cell`, which says which cell a box should ask (ADR-0179).
//! It documents what [`crate::openapi_admin`] does, with the same error envelope: paths,
//! parameters, request bodies in prose, and every outcome.
//!
//! The cloud does not serve this document; it is committed. Its tests hold it to the code: the
//! committed file must be what the annotations generate, every `/sync`, `/activate` and `/claim`
//! path a router registers must be documented, no documented path may be one nobody serves, and no
//! `operationId` may be used twice.
//!
//! Each operation says who may call it (ADR-0177 decision 5). A `/sync` route requires
//! `device_credential`, with the scope it checks as the requirement's role; `/activate` and
//! `/claim` are public, `security: []`, each with an `x-public-reason` naming what gates it
//! instead.

use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use pos_cloud_api::error_schema::{ErrorBody, ErrorDetail, ErrorResponse};

/// What a store box calls on the cloud. Only its tests read it, since the cloud does not serve it.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the document is committed by its test, never served by the cloud"
    )
)]
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Pizza 4P's POS Store Box API",
        version = "1.0.0",
        description = "What a store's edge calls on its cloud: the store-facing `/sync` surface, \
                       `/activate` and `/claim`, the two ways a box first comes to hold a \
                       credential, and `/.well-known/pos-cell`, which says which cell to ask. \
                       Generated from the handlers (ADR-0177). Every failure carries the AIP-193 \
                       `ErrorResponse` envelope. Every route but `/.well-known/pos-cell`, which \
                       answers from memory, is rate-limited per client before any credential is \
                       read."
    ),
    paths(
        crate::http::edge_config_sync,
        crate::http::edge_heartbeat,
        crate::http::edge_publish_events,
        crate::http::edge_events_hello,
        crate::http::reconcile::store_reconcile,
        crate::relay::pull_orders,
        crate::relay::ack_order,
        crate::http::ota::receive_store_report,
        crate::http::ota::serve_ota_artifact,
        crate::http::serve_store_archive_key,
        crate::http::receive_store_archive,
        pos_cloud_devices::routes::propose_device,
        pos_cloud_devices::routes::list_store_devices,
        pos_cloud_devices::routes::exchange_activation_code,
        pos_cloud_devices::routes::open_claim,
        pos_cloud_devices::routes::collect_claim,
        crate::http::well_known::well_known_cell,
    ),
    components(schemas(ErrorResponse, ErrorBody, ErrorDetail)),
    modifiers(&DeviceCredential),
    tags(
        (name = "configuration", description = "The store's published configuration, pulled as \
                                                a snapshot or a delta, and its liveness heartbeat \
                                                (ADR-0039, ADR-0068)."),
        (name = "events", description = "The store's own events, published over HTTPS after a \
                                         protocol hello, and the reconciliation that finds what \
                                         the cloud is missing (ADR-0143, ADR-0024, ADR-0078)."),
        (name = "order relay", description = "Orders taken for the store elsewhere, pulled by a \
                                              bounded long-poll and acknowledged with the store's \
                                              outcome (ADR-0061)."),
        (name = "updates", description = "The signed release a store installs, and what it reports \
                                          once it runs it (ADR-0088, ADR-0078)."),
        (name = "archives", description = "The key a store seals its archives with, and the sealed \
                                           archives it ships, which the cloud cannot open \
                                           (ADR-0124)."),
        (name = "devices", description = "Printers and kitchen displays a store discovers and \
                                          proposes, and the ones an operator approved (ADR-0041)."),
        (name = "box activation", description = "How a box first comes to hold its device \
                                                 credential: an activation code from the setup \
                                                 sheet, or a claim the console binds (ADR-0050, \
                                                 ADR-0148)."),
        (name = "cells", description = "Which cell this cloud is, and which cell is a tenant's \
                                        home, from the signed, PII-free cell directory. Asked \
                                        once by a box being set up, which then keeps its \
                                        `cloud_url` (ADR-0179)."),
    )
)]
pub(crate) struct DeviceApiDoc;

/// Declares `device_credential`, what a box presents on `/sync`: the device credential it was
/// issued at activation or claim (ADR-0143), or its store's scoped API key. A requirement's role
/// is the scope the route checks.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "only the document's tests build it")
)]
struct DeviceCredential;

impl Modify for DeviceCredential {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .get_or_insert_with(|| utoipa::openapi::ComponentsBuilder::new().build());
        components.add_security_scheme(
            "device_credential",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "A box's device credential, issued once by `/activate` or a collected \
                         claim, or its store's scoped API key, presented as a bearer token. Each \
                         requirement's role is the scope the route checks, and the credential \
                         answers only for its own store.",
                    ))
                    .build(),
            ),
        );
    }
}

/// Whether `path` is on the surface this document covers.
#[cfg(test)]
fn is_device_path(path: &str) -> bool {
    path == "/activate"
        || path == "/.well-known/pos-cell"
        || path == "/claim"
        || path.starts_with("/claim/")
        || path.starts_with("/sync/")
}

/// Every device-facing path the routers register, read out of their source as
/// [`crate::openapi_admin`] reads the `/admin` ones, plus the relay module, which registers the
/// order relay.
///
/// A route registered under a constant (`.route(CLAIM_PATH, …)`) is resolved through that
/// constant's `const NAME: &str = "…";` in the same sources, so a path moved into a constant is
/// still found.
#[cfg(test)]
fn registered_device_paths() -> std::collections::BTreeSet<String> {
    let mut sources = crate::openapi_admin::router_sources();
    let relay = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/relay.rs");
    sources.push(std::fs::read_to_string(relay).unwrap_or_default());
    let constant = |name: &str| {
        let declaration = format!("const {name}: &str = \"");
        sources.iter().find_map(|source| {
            let after = source.split_once(declaration.as_str())?.1;
            after.split_once('"').map(|(value, _)| value.to_owned())
        })
    };
    let mut paths = std::collections::BTreeSet::new();
    for source in &sources {
        for tail in source.split(".route(").skip(1) {
            let first = tail.trim_start();
            let literal = if let Some(quoted) = first.strip_prefix('"') {
                quoted.split_once('"').map(|(value, _)| value.to_owned())
            } else {
                let name: String = first
                    .chars()
                    .take_while(|character| character.is_ascii_uppercase() || *character == '_')
                    .collect();
                if name.is_empty() {
                    None
                } else {
                    constant(&name)
                }
            };
            if let Some(path) = literal.filter(|path| is_device_path(path)) {
                paths.insert(path);
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{DeviceApiDoc, registered_device_paths};
    use utoipa::OpenApi as _;

    /// Where the generated document is committed, relative to the repository root.
    const SNAPSHOT_PATH: &str = "docs/openapi-device.json";

    /// Every path the document describes.
    fn documented_paths() -> BTreeSet<String> {
        DeviceApiDoc::openapi()
            .paths
            .paths
            .keys()
            .cloned()
            .collect()
    }

    /// The committed document is what the code generates.
    #[test]
    fn the_committed_device_openapi_matches_the_code() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(SNAPSHOT_PATH);
        let rendered = DeviceApiDoc::openapi().to_pretty_json().unwrap_or_default();
        if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
            std::fs::write(&path, &rendered).expect("write the device openapi document");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed.trim(),
            rendered.trim(),
            "{SNAPSHOT_PATH} disagrees with the code. Regenerate it:\
             \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi_device\n"
        );
    }

    /// Every registered `/sync`, `/activate` and `/claim` path is documented.
    #[test]
    fn every_device_route_is_documented() {
        let registered = registered_device_paths();
        assert!(
            registered.len() >= 10,
            "the route extractor found only {} paths, which means it stopped matching the source \
             rather than that the surface shrank",
            registered.len(),
        );
        let documented = documented_paths();
        let missing: Vec<&String> = registered
            .iter()
            .filter(|path| !documented.contains(*path))
            .collect();
        assert!(
            missing.is_empty(),
            "these registered device routes are not documented: annotate the handler with \
             #[utoipa::path] and name it in DeviceApiDoc's paths:\n{missing:#?}",
        );
    }

    /// The document describes no path the routers do not serve.
    #[test]
    fn the_device_document_promises_no_route_the_router_does_not_serve() {
        let registered = registered_device_paths();
        let phantom: Vec<String> = documented_paths()
            .into_iter()
            .filter(|path| !registered.contains(path))
            .collect();
        assert!(
            phantom.is_empty(),
            "these paths are documented but not registered:\n{phantom:#?}",
        );
    }

    /// Every operation says who may call it: `device_credential` with one scope the cloud knows as
    /// its role, or `security: []` with an `x-public-reason` (ADR-0177 decision 5).
    #[test]
    fn every_device_operation_says_who_may_call_it() {
        let json = serde_json::to_value(DeviceApiDoc::openapi()).expect("serialize the document");
        for (path, item) in json["paths"].as_object().into_iter().flatten() {
            for (method, operation) in item.as_object().into_iter().flatten() {
                let route = format!("{} {path}", method.to_uppercase());
                let security = operation["security"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{route} declares no security"));
                let reason = operation["x-public-reason"].as_str().unwrap_or_default();
                if security.is_empty() {
                    assert!(
                        !reason.trim().is_empty(),
                        "{route} is public, so it needs an x-public-reason"
                    );
                    continue;
                }
                assert!(
                    reason.is_empty(),
                    "{route} requires a credential, yet gives a public reason"
                );
                let [requirement] = security.as_slice() else {
                    panic!("{route} should name one requirement");
                };
                let roles = requirement["device_credential"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{route} should require device_credential"));
                let [role] = roles.as_slice() else {
                    panic!("{route} should name the one scope it checks");
                };
                let scope = role.as_str().unwrap_or_default();
                assert!(
                    crate::auth::apikey::Scope::from_wire(scope).is_some(),
                    "{route} names {scope}, which is not a scope"
                );
            }
        }
    }

    /// Each `operationId` is its handler's name, so two handlers may not share one (ADR-0177
    /// decision 4).
    #[test]
    fn every_device_operation_id_is_used_once() {
        let json = serde_json::to_value(DeviceApiDoc::openapi()).expect("serialize the document");
        let mut seen = BTreeSet::new();
        for (path, item) in json["paths"].as_object().into_iter().flatten() {
            for (method, operation) in item.as_object().into_iter().flatten() {
                let id = operation["operationId"].as_str().unwrap_or_default();
                assert!(!id.is_empty(), "{method} {path} has no operationId");
                assert!(
                    seen.insert(id.to_owned()),
                    "{method} {path} reuses the operationId {id}: rename a handler"
                );
            }
        }
    }
}
