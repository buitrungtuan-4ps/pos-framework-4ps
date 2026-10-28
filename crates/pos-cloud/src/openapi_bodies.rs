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
const ADMIN_NO_SCHEMA_YET: &[&str] = &[];

/// Store-box routes, `METHOD /path`, with a success answer or a request body that has no schema yet.
const DEVICE_NO_SCHEMA_YET: &[&str] = &[];

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
