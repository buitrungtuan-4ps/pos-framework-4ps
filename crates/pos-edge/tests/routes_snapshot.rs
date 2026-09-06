// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An edge `/api/*` route, once published, is not removed or renamed
//! ([ADR-0111](../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
//!
//! # Why this file exists
//!
//! `AGENTS.md` §2 forbids removing or renaming a published **field, event, or permission**, and
//! `docs/snapshots/` holds one file per contract so that the rule is a build failure rather than a
//! promise. Routes were not on that list. ADR-0111 puts them there, because the `pos-edge-version`
//! header it introduces compares **one-sidedly** — the app checks whether it is ahead of its edge,
//! and never the reverse — and that comparison is only safe if a newer edge cannot take away a route
//! an older app calls. Without a snapshot that rests on nobody's memory.
//!
//! The failure it prevents is quiet, which is the reason it is worth a gate. A renamed route does not
//! answer `404` to the app: [`assets`](pos_edge) falls back to `index.html` for an unmatched path, so
//! the app receives `200 text/html` and reports a `SyntaxError` from a JSON parse, naming neither the
//! route nor the version that moved it.
//!
//! # Scope: `/api/*`, exactly as the record draws it
//!
//! `/healthz` and `/ws` are registered by the same router and are **not** here. That is ADR-0111's
//! line, not an oversight, and the two are outside it for different reasons. `/healthz` serves a
//! service manager's liveness probe rather than the app. `/ws` is one route whose name lives in a
//! single expression in `ui/src/api/live.ts`, and renaming it fails at connect time — loudly, at
//! once, on every device — rather than as an unattributable parse error on one call. Widening the
//! rule is a decision for a record, not for the file that enforces it.
//!
//! # How the set is recovered
//!
//! Axum's `Router` does not expose its routes, so — exactly as `pos_cloud::openapi_admin` does for
//! `/admin` — the registered set is read out of the source: take each `.route(`, then the first
//! string literal after it, then the method constructors in the same call. Unlike that one this walks
//! the whole `src/` tree rather than naming a file, so a route registered from a **new** module is
//! caught rather than silently missed; `the_extractor_still_matches_the_source` fails if the shape of
//! the tree ever stops matching, so a broken extractor reads as broken rather than as an empty edge.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Where the snapshot is committed, relative to the repository root.
const SNAPSHOT_PATH: &str = "docs/snapshots/routes.txt";

/// The header written above the generated lines.
const HEADER: &str = "\
# Generated from the .route( registrations in crates/pos-edge/src — do not edit.
# One line per published edge route: METHOD, a space, then the path.
# ADR-0111: an /api/* route, once published, is not removed or renamed — deprecate in place.
# /healthz and /ws are registered by the same router and are deliberately outside that rule.
";

/// The HTTP method constructors a route can be registered with.
const METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];

/// The repository root, from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            into.push(path);
        }
    }
}

/// Whether `byte` can appear inside a Rust identifier.
///
/// This is what keeps `forget(` from being read as a `get` route: the character before the method
/// name has to be something an identifier cannot continue through, so `routing::get(`, `.post(` and
/// `get(` all count while `forget(` does not.
fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The text of one `.route(…)` call's arguments, given everything after its opening parenthesis.
///
/// Depth-counts parentheses so a handler's own `(` — a turbofish's, a closure's — does not end the
/// call early, and skips string literals so a path containing a bracket could never either.
fn call_arguments(tail: &str) -> &str {
    let mut depth = 1_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in tail.bytes().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return tail.get(..index).unwrap_or_default();
                }
            }
            _ => {}
        }
    }
    tail
}

/// The method constructors named in one route call's arguments, upper-cased.
///
/// A route may carry more than one (`get(read).post(write)`), so this collects every match rather
/// than the first: a snapshot that recorded only `GET` would let a `POST` be removed unnoticed.
fn methods_in(arguments: &str) -> Vec<String> {
    let mut found = Vec::new();
    for method in METHODS {
        let needle = format!("{method}(");
        for (index, _match) in arguments.match_indices(&needle) {
            let preceded_by_identifier = index
                .checked_sub(1)
                .and_then(|before| arguments.as_bytes().get(before).copied())
                .is_some_and(is_identifier_byte);
            if !preceded_by_identifier {
                found.push(method.to_uppercase());
                break;
            }
        }
    }
    found
}

/// Every `/api/*` route the source registers, as `METHOD /path`.
fn registered_api_routes() -> BTreeSet<String> {
    let mut sources = Vec::new();
    rust_sources(&repo_root().join("crates/pos-edge/src"), &mut sources);

    let mut routes = BTreeSet::new();
    for source in sources {
        let Ok(text) = std::fs::read_to_string(&source) else {
            continue;
        };
        for tail in text.split(".route(").skip(1) {
            let arguments = call_arguments(tail);
            let Some(open) = arguments.find('"') else {
                continue;
            };
            let after_open = arguments.get(open + 1..).unwrap_or_default();
            let Some(close) = after_open.find('"') else {
                continue;
            };
            let path = after_open.get(..close).unwrap_or_default();
            if !path.starts_with("/api/") {
                continue;
            }
            let rest = after_open.get(close + 1..).unwrap_or_default();
            for method in methods_in(rest) {
                routes.insert(format!("{method} {path}"));
            }
        }
    }
    routes
}

/// The file the snapshot should hold, for the routes the source registers.
fn rendered() -> String {
    let mut out = String::from(HEADER);
    for route in registered_api_routes() {
        out.push_str(&route);
        out.push('\n');
    }
    out
}

/// The committed snapshot is what the router registers.
///
/// Regenerate with `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-edge --test routes_snapshot`, the same
/// escape the other generated files carry. Regenerating is how a route is **added**; a route that
/// disappears from the regenerated file is what `cargo xtask snapshot` refuses, because that check
/// compares against the base branch and this one cannot.
#[test]
fn the_committed_route_snapshot_matches_the_router() {
    let path = repo_root().join(SNAPSHOT_PATH);
    let rendered = rendered();
    if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, &rendered).expect("write the route snapshot");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|_ignored| {
        panic!(
            "{SNAPSHOT_PATH} is missing. Regenerate it:\
             \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-edge --test routes_snapshot\n"
        )
    });
    assert_eq!(
        committed, rendered,
        "{SNAPSHOT_PATH} disagrees with the routes the source registers. If you added a route, \
         regenerate it:\
         \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-edge --test routes_snapshot\
         \nIf a line disappeared, that is ADR-0111's rule: an /api/* route is deprecated in place, \
         never renamed or removed.",
    );
}

/// The extractor is still reading the tree, rather than quietly finding nothing.
///
/// A snapshot generated by a broken extractor is worse than no snapshot: it passes its own equality
/// check forever while covering nothing. The floor is far below the real count and is a shape check,
/// not a census — routes are expected to be added.
#[test]
fn the_extractor_still_matches_the_source() {
    let routes = registered_api_routes();
    assert!(
        routes.len() > 20,
        "the route extractor found only {} /api routes, which means it stopped matching the source \
         rather than that the edge shrank",
        routes.len(),
    );
    assert!(
        routes.contains("POST /api/pair"),
        "pairing is registered in its own sub-router; not finding it means the walk missed a file",
    );
    assert!(
        routes.contains("POST /api/print/jobs/{job_id}/ack"),
        "the acknowledge route's path literal sits on its own line; not finding it means the \
         extractor only reads a `.route(` whose arguments rustfmt kept together",
    );
}

/// Nothing outside `/api/` is in the snapshot, and `/healthz` and `/ws` in particular are not.
///
/// ADR-0111 scopes the additive rule to `/api/*`. Pinning the boundary here means widening it is a
/// visible act — this test fails — rather than something that happens by accident the next time the
/// extractor is touched.
#[test]
fn the_snapshot_covers_the_api_surface_and_not_the_rest() {
    for route in registered_api_routes() {
        assert!(
            route.contains(" /api/"),
            "{route} is in the snapshot but is not an /api route",
        );
    }
    let committed = std::fs::read_to_string(repo_root().join(SNAPSHOT_PATH)).unwrap_or_default();
    for line in committed.lines().filter(|line| !line.starts_with('#')) {
        assert!(
            !line.ends_with("/healthz") && !line.ends_with("/ws"),
            "{line} is outside ADR-0111's rule; widening it is a decision for a record",
        );
    }
}
