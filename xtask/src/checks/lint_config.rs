// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Guards the replace-not-merge behaviour of `clippy.toml`.
//!
//! A `clippy.toml` inside a crate **replaces** the workspace-root file rather than
//! merging with it. So a crate that adds one to obtain a single exemption silently
//! loses the float ban, the `SystemTime::now` ban, and the test allowances — with
//! no warning from Cargo or clippy. That is a quiet way to lose the money rule.
//!
//! This check requires every per-crate `clippy.toml` to restate the keys named in
//! `tools/clippy-baseline.toml`.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::{Error, repo_root};
use crate::Finding;

#[derive(Deserialize)]
struct Baseline {
    #[serde(rename = "required-keys")]
    required_keys: Vec<String>,
}

/// Checks every `crates/**/clippy.toml` against the baseline.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let baseline: Baseline = toml::from_str(&std::fs::read_to_string(
        root.join("tools/clippy-baseline.toml"),
    )?)?;
    let required: BTreeSet<&str> = baseline.required_keys.iter().map(String::as_str).collect();

    let mut findings = Vec::new();
    // Every clippy.toml except the workspace-root one, which *is* the baseline.
    let mut per_crate = find_crate_clippy_files(&root.join("crates"))?;
    per_crate.extend(find_crate_clippy_files(&root.join("xtask"))?);
    per_crate.sort();
    for path in per_crate {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let text = std::fs::read_to_string(&path)?;
        let value: toml::Value = toml::from_str(&text)?;
        let present: BTreeSet<&str> = value
            .as_table()
            .map(|t| t.keys().map(String::as_str).collect())
            .unwrap_or_default();

        for key in required.difference(&present) {
            findings.push(
                Finding::new(
                    &rel,
                    "lint-config",
                    format!("missing `{key}`, which the workspace clippy.toml sets"),
                )
                .with_hint(
                    "a per-crate clippy.toml replaces the root file instead of merging with \
                     it, so every key it still wants has to be restated here",
                ),
            );
        }
    }
    Ok(findings)
}

fn find_crate_clippy_files(dir: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            out.extend(find_crate_clippy_files(&path)?);
        } else if path.file_name().is_some_and(|n| n == "clippy.toml") {
            out.push(path);
        }
    }
    Ok(out)
}
