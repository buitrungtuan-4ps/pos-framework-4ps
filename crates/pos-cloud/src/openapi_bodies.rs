// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every body the cloud's documents describe names its schema
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 3).
//!
//! A success answer with a body, and a request body, each carry a schema, except on the routes the
//! two lists below still name. The lists only shrink: a route leaves when its handler's annotation
//! names its bodies, and a list goes when it is empty. The `/v1` document has no list: every one
//! of its bodies has a schema.

use std::collections::BTreeSet;

use utoipa::OpenApi as _;

use crate::openapi::ApiDoc;
use crate::openapi_admin::AdminApiDoc;
use crate::openapi_device::DeviceApiDoc;

/// `/admin` routes, `METHOD /path`, with a success answer or a request body that has no schema yet.
const ADMIN_NO_SCHEMA_YET: &[&str] = &[
    "DELETE /admin/assignments/{assignment_id}",
    "DELETE /admin/fees",
    "DELETE /admin/settings",
    "GET /admin/config-releases",
    "GET /admin/config/channels",
    "GET /admin/config/lease",
    "GET /admin/config/origins",
    "GET /admin/config/ota",
    "GET /admin/config/ota/placement",
    "GET /admin/config/qr",
    "GET /admin/config/retention",
    "GET /admin/config/tender",
    "GET /admin/config/vendors",
    "GET /admin/fees",
    "GET /admin/fees/effective",
    "GET /admin/openapi.json",
    "GET /admin/ota/releases/{release}",
    "GET /admin/ota/releases/{release}/installer",
    "GET /admin/settings",
    "GET /admin/settings/catalogue",
    "GET /admin/settings/effective",
    "GET /admin/stores/{store_id}/config",
    "GET /admin/stores/{store_id}/config/versions/{version_id}",
    "PATCH /admin/employees/{employee_id}",
    "PATCH /admin/roles/{role_id}",
    "POST /admin/assignments",
    "POST /admin/config/campaigns/schedule",
    "POST /admin/config/lease/bump",
    "POST /admin/devices/publish",
    "POST /admin/employees",
    "POST /admin/fees",
    "POST /admin/fees/publish",
    "POST /admin/floor/areas",
    "POST /admin/floor/tables",
    "POST /admin/kitchen/routing",
    "POST /admin/kitchen/stations",
    "POST /admin/ota/releases",
    "POST /admin/ota/releases/fetch",
    "POST /admin/roles",
    "POST /admin/settings/presets",
    "POST /admin/settings/publish",
    "POST /admin/subjects/{subject_id}/erase",
    "PUT /admin/config/integrations",
    "PUT /admin/config/ota",
    "PUT /admin/config/ota/placement",
    "PUT /admin/config/reason-codes",
    "PUT /admin/fees",
    "PUT /admin/settings",
    "PUT /admin/store-groups/{group_id}/members",
];

/// Store-box routes, `METHOD /path`, with a success answer or a request body that has no schema yet.
const DEVICE_NO_SCHEMA_YET: &[&str] = &["POST /claim/{claim_id}/collect"];

/// The operations in `document` with a success answer (other than `204`) that has no content, or
/// a request body with a media type that names no schema.
fn without_schema(document: &utoipa::openapi::OpenApi) -> BTreeSet<String> {
    let json = serde_json::to_value(document).expect("serialize the document");
    let mut missing = BTreeSet::new();
    for (path, item) in json["paths"].as_object().into_iter().flatten() {
        for (method, operation) in item.as_object().into_iter().flatten() {
            let route = format!("{} {path}", method.to_uppercase());
            let answers = operation["responses"].as_object().into_iter().flatten();
            let bare_answer = answers
                .filter(|(status, _)| status.starts_with('2') && *status != "204")
                .any(|(_, response)| response.get("content").is_none());
            let bare_request = operation.get("requestBody").is_some_and(|request| {
                let content = request["content"].as_object();
                content.is_none_or(|media| {
                    media.is_empty() || media.values().any(|one| one.get("schema").is_none())
                })
            });
            if bare_answer || bare_request {
                missing.insert(route);
            }
        }
    }
    missing
}

fn listed(routes: &[&str]) -> BTreeSet<String> {
    routes.iter().map(|&route| route.to_owned()).collect()
}

#[test]
fn every_admin_body_has_a_schema_or_is_listed() {
    assert_eq!(
        without_schema(&AdminApiDoc::openapi()),
        listed(ADMIN_NO_SCHEMA_YET),
        "a body without a schema must be in ADMIN_NO_SCHEMA_YET, and a route whose bodies all have one now \
         must leave it"
    );
}

#[test]
fn every_device_body_has_a_schema_or_is_listed() {
    assert_eq!(
        without_schema(&DeviceApiDoc::openapi()),
        listed(DEVICE_NO_SCHEMA_YET),
        "a body without a schema must be in DEVICE_NO_SCHEMA_YET, and a route whose bodies all have one now \
         must leave it"
    );
}

#[test]
fn every_v1_body_has_a_schema() {
    assert_eq!(without_schema(&ApiDoc::openapi()), BTreeSet::new());
}
