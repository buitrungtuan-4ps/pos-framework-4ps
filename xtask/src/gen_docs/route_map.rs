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
        let sources = sources_under(root, roots)?;
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
                let file = handler_file(root, &sources, handler)
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

/// The repository-relative file of the one free `async fn handler` at the left margin of `sources`.
fn handler_file(root: &Path, sources: &[PathBuf], handler: &str) -> Result<String, String> {
    let mut found = Vec::new();
    for file in sources {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        if text.lines().any(|line| declares(line, handler)) {
            found.push(file);
        }
    }
    match found.as_slice() {
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

/// Whether `line` declares the free async function `name`: `async fn name`, after nothing or a
/// visibility, followed by its generics or its parameters.
fn declares(line: &str, name: &str) -> bool {
    let Some((prefix, rest)) = line.split_once("async fn ") else {
        return false;
    };
    let visibility_only = prefix.is_empty()
        || prefix == "pub "
        || (prefix.starts_with("pub(") && prefix.ends_with(") "));
    visibility_only
        && rest
            .strip_prefix(name)
            .is_some_and(|after| after.starts_with('<') || after.starts_with('('))
}

#[cfg(test)]
mod tests {
    use super::{declares, permission, who};

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
