// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `docs/generated/route-map.md`: every route the four OpenAPI documents describe, one line each
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 4, P11 item 5).
//!
//! Read from the committed documents, never from the routers, so a route is described in one place:
//! its handler's annotation. Each line gives the method and path, the `operationId`, which is the
//! handler's name, who may call it (the security scheme and its role, or the public reason), the
//! permission it needs (`x-permission`), and the file the handler is in. That file is found by the
//! name: the one free `async fn` of that name in the crates that serve the document. A name that
//! matches no function, or more than one, stops the render, because the line would point nowhere.
//!
//! Each set of source roots is read once, into an index of the free `async fn` names each file
//! declares, so finding a handler is a lookup rather than another pass over every file.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::checks::Error;

/// Each document: its file, the surface it describes, and the source roots its handlers live under.
const DOCUMENTS: &[(&str, &str, &[&str])] = &[
    (
        "docs/openapi-edge.json",
        "The till (`/api`, `/healthz`, `/ws`)",
        &["crates/pos-edge/src"],
    ),
    (
        "docs/openapi-admin.json",
        "The console (`/admin`)",
        &["crates/pos-cloud*"],
    ),
    (
        "docs/openapi-device.json",
        "A store box on its cloud (`/sync`, `/activate`, `/claim`)",
        &["crates/pos-cloud*"],
    ),
    (
        "docs/openapi.json",
        "Integrators (`/v1`)",
        &["crates/pos-cloud*"],
    ),
];

/// The methods in the order a line lists them, so a path's operations read the same every time.
const METHODS: [&str; 7] = ["get", "put", "post", "delete", "patch", "head", "options"];

/// Renders `route-map.md` from the committed documents and the source tree under `root`.
///
/// # Errors
///
/// [`Error`] if a document cannot be read or parsed, or an `operationId` names no handler, or more
/// than one.
pub fn render(root: &Path, generated: &str) -> Result<String, Error> {
    let mut out = String::new();
    let mut indexes: BTreeMap<&[&str], Handlers> = BTreeMap::new();
    let _ = writeln!(out, "# Route map\n\n{generated}\n");
    let _ = writeln!(
        out,
        "Every route the OpenAPI documents describe, read from them (ADR-0177). The handler is the \
         operation's `operationId`. **Who** is the security requirement, its role in brackets, or \
         `public` and what gates it instead; **Permission** is `x-permission`, which the documents' \
         tests hold to the code. To change a line, change the handler's annotation and regenerate \
         its document.\n"
    );
    for (document, surface, roots) in DOCUMENTS {
        let text = std::fs::read_to_string(root.join(document))?;
        let json: serde_json::Value = serde_json::from_str(&text)?;
        let handlers = match indexes.entry(roots) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(handlers_under(root, roots)?)
            }
        };
        let _ = writeln!(
            out,
            "## {surface}\n\nFrom [`{document}`](../{}).\n",
            file_name(document)
        );
        let _ = writeln!(out, "| Method | Path | Handler | Who | Permission | File |");
        let _ = writeln!(out, "|---|---|---|---|---|---|");
        for (path, item) in json["paths"].as_object().into_iter().flatten() {
            for method in METHODS {
                let Some(operation) = item.get(method) else {
                    continue;
                };
                let handler = operation["operationId"].as_str().unwrap_or_default();
                let file = handler_file(root, handlers, handler)
                    .map_err(|why| format!("{document}: {method} {path}: {why}"))?;
                let _ = writeln!(
                    out,
                    "| {} | `{path}` | `{handler}` | {} | {} | [`{file}`](../../{file}) |",
                    method.to_uppercase(),
                    who(operation),
                    permission(operation),
                );
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// The last path segment of `path`.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Who may call an operation: each requirement as `scheme` or `scheme (role)`, or `public: reason`.
fn who(operation: &serde_json::Value) -> String {
    let Some(requirements) = operation["security"].as_array() else {
        return "—".to_owned();
    };
    if requirements.is_empty() {
        let reason = operation["x-public-reason"].as_str().unwrap_or_default();
        return cell(&format!("public: {reason}"));
    }
    let named: Vec<String> = requirements
        .iter()
        .flat_map(|requirement| requirement.as_object().into_iter().flatten())
        .map(|(scheme, roles)| {
            let roles: Vec<&str> = roles
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect();
            if roles.is_empty() {
                format!("`{scheme}`")
            } else {
                format!("`{scheme}` ({})", roles.join(", "))
            }
        })
        .collect();
    named.join(" or ")
}

/// An operation's `x-permission`: one id, several, or an em dash.
fn permission(operation: &serde_json::Value) -> String {
    match &operation["x-permission"] {
        serde_json::Value::String(id) => format!("`{id}`"),
        serde_json::Value::Array(ids) => ids
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(|id| format!("`{id}`"))
            .collect::<Vec<_>>()
            .join(", "),
        _ => "—".to_owned(),
    }
}

/// One table cell, with any `|` escaped.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Every `.rs` file under each root, where a root ending in `*` (`crates/pos-cloud*`) means the
/// `src/` of every crate whose directory starts with what comes before it.
fn sources_under(root: &Path, roots: &[&str]) -> Result<Vec<PathBuf>, Error> {
    let mut directories = Vec::new();
    for prefix in roots {
        let Some(prefix) = prefix.strip_suffix('*') else {
            directories.push(root.join(prefix));
            continue;
        };
        let path = root.join(prefix);
        let parent = path.parent().ok_or("a source root has no parent")?;
        let stem = file_name(prefix);
        for entry in std::fs::read_dir(parent)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with(stem) {
                directories.push(entry.path().join("src"));
            }
        }
    }
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
    Ok(files)
}

/// The files that declare each free `async fn`, by name, in the order [`sources_under`] lists them.
type Handlers = BTreeMap<String, Vec<PathBuf>>;

/// Reads every source file under `roots` once and indexes the free `async fn` names it declares.
/// A file that cannot be read is skipped, and a name a file declares twice lists that file once.
fn handlers_under(root: &Path, roots: &[&str]) -> Result<Handlers, Error> {
    let mut handlers = Handlers::new();
    for file in sources_under(root, roots)? {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let mut names: Vec<&str> = text.lines().filter_map(declared_name).collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            handlers
                .entry(name.to_owned())
                .or_default()
                .push(file.clone());
        }
    }
    Ok(handlers)
}

/// The repository-relative file of the one free `async fn handler` at the left margin of the
/// indexed sources.
fn handler_file(root: &Path, handlers: &Handlers, handler: &str) -> Result<String, String> {
    match handlers.get(handler).map_or(&[][..], Vec::as_slice) {
        [file] => Ok(file
            .strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string()
            .replace('\\', "/")),
        [] => Err(format!("no `async fn {handler}` in its crates")),
        many => Err(format!(
            "`async fn {handler}` is declared {} times",
            many.len()
        )),
    }
}

/// The free async function `line` declares: the name after `async fn `, when what comes before is
/// nothing or a visibility, up to its generics or its parameters.
fn declared_name(line: &str) -> Option<&str> {
    let (prefix, rest) = line.split_once("async fn ")?;
    let visibility_only = prefix.is_empty()
        || prefix == "pub "
        || (prefix.starts_with("pub(") && prefix.ends_with(") "));
    if !visibility_only {
        return None;
    }
    let end = rest.find(['<', '('])?;
    rest.get(..end)
}

/// Whether `line` declares the free async function `name`.
#[cfg(test)]
fn declares(line: &str, name: &str) -> bool {
    declared_name(line) == Some(name)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Handlers, declared_name, declares, handler_file, permission, who};

    #[test]
    fn finds_a_free_handler_and_nothing_else() {
        assert!(declares(
            "pub(crate) async fn settle_bill<S>(",
            "settle_bill"
        ));
        assert!(declares(
            "async fn open_claim<Cl, A, C>(State(state): …",
            "open_claim"
        ));
        assert!(declares("pub async fn healthz() -> Response {", "healthz"));
        assert!(!declares(
            "    async fn settle_bill(&self) {",
            "settle_bill"
        ));
        assert!(!declares(
            "pub(crate) async fn settle_bill_at<S>(",
            "settle_bill"
        ));
        assert!(!declares("// async fn settle_bill(", "settle_bill"));
    }

    #[test]
    fn a_line_names_the_handler_it_declares() {
        assert_eq!(
            declared_name("pub(crate) async fn settle_bill<S>("),
            Some("settle_bill")
        );
        assert_eq!(
            declared_name("async fn healthz() -> Response {"),
            Some("healthz")
        );
        assert_eq!(declared_name("    async fn settle_bill(&self) {"), None);
        assert_eq!(declared_name("let x = 1;"), None);
    }

    #[test]
    fn a_handler_is_found_once_or_the_render_stops() {
        let root = Path::new("/repo");
        let mut handlers = Handlers::new();
        handlers.insert(
            "settle_bill".to_owned(),
            vec![PathBuf::from("/repo/crates/pos-edge/src/http/bills.rs")],
        );
        handlers.insert(
            "twice".to_owned(),
            vec![PathBuf::from("/repo/a.rs"), PathBuf::from("/repo/b.rs")],
        );
        assert_eq!(
            handler_file(root, &handlers, "settle_bill"),
            Ok("crates/pos-edge/src/http/bills.rs".to_owned())
        );
        assert!(handler_file(root, &handlers, "missing").is_err());
        assert_eq!(
            handler_file(root, &handlers, "twice"),
            Err("`async fn twice` is declared 2 times".to_owned())
        );
    }

    #[test]
    fn says_who_may_call_and_what_it_needs() {
        let gated = serde_json::json!({
            "security": [{ "device_credential": ["read_config"] }],
            "x-permission": ["sales.line.add", "sales.line.void_fired"],
        });
        assert_eq!(who(&gated), "`device_credential` (read_config)");
        assert_eq!(
            permission(&gated),
            "`sales.line.add`, `sales.line.void_fired`"
        );
        let public = serde_json::json!({ "security": [], "x-public-reason": "a | b" });
        assert_eq!(who(&public), "public: a \\| b");
        assert_eq!(permission(&public), "—");
    }
}
