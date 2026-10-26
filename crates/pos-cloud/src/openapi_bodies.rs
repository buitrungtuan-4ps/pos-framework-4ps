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
    "GET /admin/config/lease",
    "GET /admin/ota/releases/{release}",
    "POST /admin/config/campaigns/schedule",
    "POST /admin/devices/publish",
    "POST /admin/ota/releases",
    "POST /admin/ota/releases/fetch",
    "POST /admin/subjects/{subject_id}/erase",
    "PUT /admin/config/integrations",
    "PUT /admin/config/reason-codes",
];

/// Store-box routes, `METHOD /path`, with a success answer or a request body that has no schema yet.
const DEVICE_NO_SCHEMA_YET: &[&str] = &[
    "GET /sync/stores/{store_id}/config",
    "POST /claim/{claim_id}/collect",
];

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

/// The crates whose `ToSchema` types the cloud's documents can carry.
const SCHEMA_SOURCES: [&str; 11] = [
    "pos-proto",
    "pos-ports",
    "pos-cloud",
    "pos-cloud-api",
    "pos-cloud-config",
    "pos-cloud-catalog",
    "pos-cloud-people",
    "pos-cloud-floor",
    "pos-cloud-inventory",
    "pos-cloud-devices",
    "pos-cloud-reports",
];

/// Names two types share on purpose: `pos-cloud-api`'s error schema mirrors `pos-proto`'s error
/// envelope field for field, held by `the_mirrored_error_envelope_matches_the_real_one`.
const MIRRORED_SCHEMAS: [&str; 3] = ["ErrorBody", "ErrorDetail", "ErrorResponse"];

/// Every schema name a `ToSchema` type in `text` registers: the type's name, or its
/// `#[schema(as = …)]`, and every hand-written `impl ToSchema for …`.
fn schema_names(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut names = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if let Some(rest) = line.split("ToSchema for ").nth(1) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            names.push(name);
            continue;
        }
        if !(line.contains("derive(") && line.contains("ToSchema")) {
            continue;
        }
        let mut renamed = None;
        for next in lines.iter().skip(at + 1).take(12) {
            if let Some(rest) = next.split("schema(as = ").nth(1) {
                let path: String = rest
                    .chars()
                    .take_while(|c| *c != ')' && *c != ',')
                    .collect();
                renamed = path.rsplit("::").next().map(str::to_owned);
            }
            let item = next.trim_start().trim_start_matches("pub ");
            let item = item.strip_prefix("pub(").map_or(item, |rest| {
                rest.split_once(") ").map_or(rest, |(_, after)| after)
            });
            if let Some(rest) = item
                .strip_prefix("struct ")
                .or_else(|| item.strip_prefix("enum "))
            {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                names.push(renamed.unwrap_or(name));
                break;
            }
        }
    }
    names
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// utoipa keys a component by its type's name, so two types that share one overwrite each other in
/// a document without a word. No two `ToSchema` types in the crates the documents draw on may share
/// a name, except the mirrors that are the same schema on purpose.
#[test]
fn no_two_schemas_share_a_name() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut seen: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for name in SCHEMA_SOURCES {
        let mut files = Vec::new();
        rust_files(&crates.join(name).join("src"), &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            // A macro's `ToSchema for $name` names no type of its own.
            let named = schema_names(&text)
                .into_iter()
                .filter(|name| !name.is_empty());
            for schema in named {
                seen.entry(schema)
                    .or_default()
                    .push(file.display().to_string());
            }
        }
    }
    let shared: Vec<(&String, &Vec<String>)> = seen
        .iter()
        .filter(|(name, files)| files.len() > 1 && !MIRRORED_SCHEMAS.contains(&name.as_str()))
        .collect();
    assert!(
        shared.is_empty(),
        "these schema names are registered by more than one type; give one `#[schema(as = …)]`:\n{shared:#?}"
    );
    assert!(
        seen.contains_key("StoredSettingValue"),
        "the scan reads `schema(as = …)`"
    );
}
