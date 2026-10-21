// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What holds the console document's `security` and `x-permission` to the code
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 5).
//!
//! Every `/admin` operation says who may call it: the console's `session_cookie`, or
//! `security: []` with an `x-public-reason` naming what gates it instead. And an operation whose
//! handler checks a [`ConsolePermission`] through `require_permission` carries that permission's id
//! as `x-permission`. The handler's call stays the record. These tests read it from the source, as
//! [`crate::openapi_admin`] reads the router's paths, so an annotation that names another
//! permission, or none, fails here. A permission that only redacts part of an answer is not
//! checked through `require_permission`, and is described in prose rather than named here.
//!
//! Its own module because the console document's is at the size limit ADR-0170 sets.

use std::path::{Path, PathBuf};

use pos_cloud_api::auth::console_rbac::ConsolePermission;
use utoipa::OpenApi as _;

use crate::openapi_admin::AdminApiDoc;

/// Every source file of this crate and of each `pos-cloud-*` crate beside it, where a console
/// handler or a helper it calls may live.
fn cloud_sources() -> Vec<String> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut directories: Vec<PathBuf> = std::fs::read_dir(&crates)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("pos-cloud"))
        .map(|entry| entry.path().join("src"))
        .collect();
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
        .iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect()
}

/// The bodies of every function named `name` in `source`: from `fn name` to the first line that
/// closes an item at the left margin.
fn function_bodies<'a>(source: &'a str, name: &str) -> Vec<&'a str> {
    let needle = format!("fn {name}");
    source
        .match_indices(&needle)
        .filter(|(start, _)| {
            let after = source.get(start + needle.len()..).unwrap_or_default();
            after.starts_with('<') || after.starts_with('(')
        })
        .map(|(start, _)| {
            let rest = source.get(start..).unwrap_or_default();
            rest.get(..rest.find("\n}\n").unwrap_or(rest.len()))
                .unwrap_or_default()
        })
        .collect()
}

/// The bodies of the free `async fn name` handlers in `source`: at the left margin, so a trait's
/// or an impl's method of the same name is not mistaken for the route's handler.
fn handler_bodies<'a>(source: &'a str, name: &str) -> Vec<&'a str> {
    function_bodies(source, name)
        .into_iter()
        .filter(|body| {
            let start = body.as_ptr() as usize - source.as_ptr() as usize;
            let line_start = source
                .get(..start)
                .and_then(|before| before.rfind('\n'))
                .map_or(0, |newline| newline + 1);
            let prefix = source.get(line_start..start).unwrap_or_default();
            let visibility = prefix.strip_suffix("async ").unwrap_or("-");
            visibility.is_empty()
                || visibility == "pub "
                || (visibility.starts_with("pub(") && visibility.ends_with(") "))
        })
        .collect()
}

/// The `ConsolePermission` variants a body names, in order, each once.
fn permissions_in(body: &str) -> Vec<&str> {
    let mut named = Vec::new();
    for (start, marker) in body.match_indices("ConsolePermission::") {
        let rest = body.get(start + marker.len()..).unwrap_or_default();
        let end = rest
            .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .unwrap_or(rest.len());
        let variant = rest.get(..end).unwrap_or_default();
        if !variant.is_empty() && !named.contains(&variant) {
            named.push(variant);
        }
    }
    named
}

/// The functions a body calls by name: each identifier followed by `(` or a turbofish.
fn callees(body: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut start = None;
    for (index, character) in body.char_indices() {
        if character.is_ascii_alphanumeric() || character == '_' {
            start.get_or_insert(index);
            continue;
        }
        if let Some(from) = start.take() {
            let rest = body.get(index..).unwrap_or_default();
            if (rest.starts_with('(') || rest.starts_with("::<"))
                && let Some(name) = body.get(from..index)
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
    }
    names
}

/// The permission id the handler named `handler` checks: in its own body, or in the one helper of
/// its file it hands the request to. `None` for a handler that checks only the session.
fn permission_checked_by(sources: &[String], handler: &str) -> Option<&'static str> {
    let found: Vec<(&String, &str)> = sources
        .iter()
        .flat_map(|source| {
            handler_bodies(source, handler)
                .into_iter()
                .map(move |body| (source, body))
        })
        .collect();
    let [(source, body)] = found.as_slice() else {
        panic!(
            "expected one function named {handler}, found {}",
            found.len()
        );
    };
    let mut named = permissions_in(body);
    if named.is_empty() {
        named = callees(body)
            .into_iter()
            .filter(|callee| *callee != handler)
            .flat_map(|callee| function_bodies(source, callee))
            .map(permissions_in)
            .find(|permissions| !permissions.is_empty())
            .unwrap_or_default();
    }
    assert!(
        named.len() <= 1,
        "{handler} checks more than one console permission: {named:?}"
    );
    named.first().map(|variant| {
        ConsolePermission::ALL
            .iter()
            .find(|permission| format!("{permission:?}") == *variant)
            .unwrap_or_else(|| panic!("{variant} is not a console permission"))
            .meta()
            .id
    })
}

/// Every operation in the console document, `METHOD /path`, as it renders.
fn operations() -> Vec<(String, serde_json::Value)> {
    let json = serde_json::to_value(AdminApiDoc::openapi()).expect("serialize the document");
    let mut operations = Vec::new();
    for (path, item) in json["paths"].as_object().into_iter().flatten() {
        for (method, operation) in item.as_object().into_iter().flatten() {
            operations.push((
                format!("{} {path}", method.to_uppercase()),
                operation.clone(),
            ));
        }
    }
    operations
}

/// Every operation says who may call it: the session cookie, or `security: []` with an
/// `x-public-reason`.
#[test]
fn every_admin_operation_says_who_may_call_it() {
    for (route, operation) in operations() {
        let security = operation["security"]
            .as_array()
            .unwrap_or_else(|| panic!("{route} declares no security"));
        let reason = operation["x-public-reason"].as_str().unwrap_or_default();
        if security.is_empty() {
            assert!(
                !reason.trim().is_empty(),
                "{route} is public, so it needs an x-public-reason"
            );
            assert!(
                operation.get("x-permission").is_none(),
                "{route} is public, yet names a permission"
            );
        } else {
            assert_eq!(
                security,
                &vec![serde_json::json!({ "session_cookie": [] })],
                "{route} should require session_cookie and nothing else"
            );
            assert!(
                reason.is_empty(),
                "{route} requires a session, yet gives a public reason"
            );
        }
    }
}

/// Each operation's `x-permission` is the permission its handler checks through
/// `require_permission`, and an operation whose handler checks none carries none.
#[test]
fn x_permission_is_what_the_handler_checks() {
    let sources = cloud_sources();
    let mut checked = 0;
    for (route, operation) in operations() {
        let handler = operation["operationId"].as_str().unwrap_or_default();
        let expected = permission_checked_by(&sources, handler);
        checked += usize::from(expected.is_some());
        assert_eq!(
            operation
                .get("x-permission")
                .and_then(serde_json::Value::as_str),
            expected,
            "{route}'s x-permission disagrees with what {handler} checks"
        );
    }
    assert!(
        checked > 200,
        "only {checked} handlers were found checking a permission, which means the source reader \
         stopped matching rather than that the console stopped checking"
    );
}
