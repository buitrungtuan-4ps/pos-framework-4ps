// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's routes as its source declares them, for the tests that hold the documents to the
//! router ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md)). Its own module because
//! the console document's is at the size limit ADR-0170 sets.

/// Every `/admin` path the router registers, read out of this crate's own router source.
///
/// Axum's `Router` does not expose its routes, so the registered set is recovered from the source
/// the way the `xtask` gates read the tree: take each `.route(` and the next string literal after
/// it. That literal is the path, whether rustfmt kept it on its line or wrapped it onto the next.
///
/// Deliberately not a regex: the tree carries no regex dependency, and the grammar here is one
/// token wide.
pub(crate) fn registered_admin_paths() -> std::collections::BTreeSet<String> {
    let mut paths = std::collections::BTreeSet::new();
    for source in router_sources() {
        for tail in source.split(".route(").skip(1) {
            let Some(open) = tail.find('"') else { continue };
            let after_open = tail.get(open + 1..).unwrap_or_default();
            let Some(close) = after_open.find('"') else {
                continue;
            };
            let literal = after_open.get(..close).unwrap_or_default();
            if literal == "/admin" || literal.starts_with("/admin/") {
                paths.insert(literal.to_owned());
            }
        }
    }
    paths
}

/// The business-area crates whose routes `pos-cloud`'s router mounts, each declaring them in its own
/// `src/routes.rs` and `src/routes/` (ADR-0170 decision 4).
const ROUTE_CRATES: [&str; 5] = [
    "pos-cloud-catalog",
    "pos-cloud-devices",
    "pos-cloud-floor",
    "pos-cloud-inventory",
    "pos-cloud-people",
];

/// The router sources: `http.rs` and every module under `http/` and `cell/`, where each business
/// area's routes are declared, and the routes of each crate in [`ROUTE_CRATES`]. Every `/admin`
/// route is registered in one of them; the other route-carrying modules (`orders`, `qr_http`,
/// `relay`) serve `/v1` and the relay surface only.
///
/// Read from the tree when the test runs, so a route that moves into a new module is still found.
pub(crate) fn router_sources() -> Vec<String> {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![source.join("http.rs")];
    let mut directories = vec![source.join("http"), source.join("cell")];
    for route_crate in ROUTE_CRATES {
        let crate_source = source.join("../..").join(route_crate).join("src");
        files.push(crate_source.join("routes.rs"));
        directories.push(crate_source.join("routes"));
    }
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
