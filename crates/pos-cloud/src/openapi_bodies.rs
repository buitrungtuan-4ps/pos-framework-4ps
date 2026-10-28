// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every body the cloud's documents describe names its schema
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 3).
//!
//! A success answer with a body, and a request body, each carry a schema, in all three documents:
//! the `/admin` console's, the store box's and the `/v1` integrators'. A route whose handler's
//! annotation leaves one out fails here.

use std::collections::BTreeSet;

use utoipa::OpenApi as _;

use crate::openapi::ApiDoc;
use crate::openapi_admin::AdminApiDoc;
use crate::openapi_device::DeviceApiDoc;

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

/// What a failing test says: which annotation to fix.
const MISSING: &str = "these routes answer, or take, a body with no schema: name it with `body = …` \
                       in the response or `content = …` in the request_body of the handler's annotation";

#[test]
fn every_admin_body_has_a_schema() {
    assert_eq!(
        without_schema(&AdminApiDoc::openapi()),
        BTreeSet::new(),
        "{MISSING}"
    );
}

#[test]
fn every_device_body_has_a_schema() {
    assert_eq!(
        without_schema(&DeviceApiDoc::openapi()),
        BTreeSet::new(),
        "{MISSING}"
    );
}

#[test]
fn every_v1_body_has_a_schema() {
    assert_eq!(
        without_schema(&ApiDoc::openapi()),
        BTreeSet::new(),
        "{MISSING}"
    );
}
