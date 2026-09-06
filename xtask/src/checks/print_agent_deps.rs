// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The print agent links three workspace crates and no more
//! ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! That record fixes the list — `printer-escpos`, `pos-ports`, `pos-proto` — and says why in one
//! sentence: *"that list is a rule a `cargo-deny`-style check can hold, and it is the rule that
//! keeps one ESC/POS encoder in the tree rather than two."* The failure it prevents is not abstract.
//! A `pos-core` on this list would put domain code on a device that must decide nothing; a
//! `pos-edge` would put a second renderer on the machine ADR-0112 spends four paragraphs keeping
//! renderers off; a second ESC/POS encoder would let two stores print different bytes from one
//! document.
//!
//! `pos-ports` is on the list because it is where the *contract* lives — `PrintJob`,
//! `PrintDocument`, `PrinterCapabilities`, `Transport` — and a binary handed a `PrintJob` cannot
//! avoid naming its type.
//!
//! Only path (workspace) dependencies are checked. Third-party crates are governed by
//! [ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md) and `cargo deny`, which already run.

use super::{Error, repo_root};
use crate::Finding;

/// The manifest this rule is about.
const MANIFEST: &str = "crates/pos-print-agent/Cargo.toml";

/// The three, in the order ADR-0112 names them.
const ALLOWED: [&str; 3] = ["printer-escpos", "pos-ports", "pos-proto"];

/// Runs the check.
///
/// # Errors
///
/// [`Error`] if the manifest cannot be read.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let path = repo_root().join(MANIFEST);
    let Ok(text) = std::fs::read_to_string(&path) else {
        // The crate not existing is not this check's business to invent; earlier phases of the tree
        // legitimately lack it, exactly as `deps-rule` tolerates a backbone crate not created yet.
        return Ok(Vec::new());
    };
    Ok(check(&text))
}

/// Evaluates the rule over a manifest's text. Pure, so it is testable against fixtures.
#[must_use]
pub fn check(manifest: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for name in declared_paths(manifest) {
        if !ALLOWED.contains(&name.as_str()) {
            findings.push(
                Finding::new(
                    MANIFEST,
                    "print-agent-deps",
                    format!("`pos-print-agent` declares `{name}`, which ADR-0112 does not allow"),
                )
                .with_hint(
                    "the agent links printer-escpos, pos-ports and pos-proto and nothing else — it \
                     decides nothing, so it needs no domain. Widening this list needs ADR-0112 \
                     amended first, which needs an architecture reviewer",
                ),
            );
        }
    }
    findings
}

/// The names of every `{ path = … }` dependency the manifest declares.
///
/// Deliberately a line scan rather than `cargo metadata`: this check must be able to say *the
/// manifest declares this* about a crate that does not compile, and it must run without a network
/// or a lockfile. A workspace dependency in this tree is always spelled `name = { path = "…" }` on
/// one line, which the tests below pin.
fn declared_paths(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_dependencies = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // Every dependency table, including `[dev-dependencies]` and a target-specific one: a
            // dev-dependency on `pos-core` would still put domain code in this crate's test build,
            // and a test that can reach the domain is a test that invites the domain in.
            in_dependencies = line.contains("dependencies]");
            continue;
        }
        if !in_dependencies || !line.contains("path") {
            continue;
        }
        if let Some((name, rest)) = line.split_once('=')
            && rest.contains("path")
        {
            names.push(name.trim().to_owned());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::{check, declared_paths};

    const ALLOWED_MANIFEST: &str = r#"
[package]
name = "pos-print-agent"

[dependencies]
printer-escpos = { path = "../adapters/printer-escpos" }
pos-ports      = { path = "../pos-ports" }
pos-proto      = { path = "../pos-proto" }
serde          = { workspace = true }
hyper          = { version = "1", features = ["client"] }
"#;

    #[test]
    fn the_three_adr_0112_names_are_accepted() {
        assert_eq!(
            declared_paths(ALLOWED_MANIFEST),
            vec!["printer-escpos", "pos-ports", "pos-proto"],
            "third-party lines carry no path and are not this rule's business"
        );
        assert!(check(ALLOWED_MANIFEST).is_empty());
    }

    #[test]
    fn a_domain_dependency_is_refused_and_the_finding_names_it() {
        // The failure this check exists for: domain code on a device that must decide nothing.
        let widened = ALLOWED_MANIFEST.replace(
            "[dependencies]",
            "[dependencies]\npos-core = { path = \"../pos-core\" }",
        );
        let findings = check(&widened);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("pos-core"), "{findings:?}");
    }

    #[test]
    fn a_dev_dependency_on_the_domain_is_refused_too() {
        // A test build that can reach the domain is a test build that invites the domain in.
        let widened = format!(
            "{ALLOWED_MANIFEST}\n[dev-dependencies]\npos-fakes = {{ path = \"../pos-fakes\" }}\n"
        );
        let findings = check(&widened);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("pos-fakes"), "{findings:?}");
    }
}
