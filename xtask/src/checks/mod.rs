// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The individual repository checks.
//!
//! Each exposes `run(args) -> Result<Vec<Finding>, Error>`. A check returns an
//! `Err` only when it could not run at all; a rule violation is a `Finding`, so
//! one broken rule does not hide the next.

pub mod actions_pinned;
pub mod countries;
pub mod deps_rule;
pub mod links;
pub mod lint_config;
pub mod migrations;
pub mod mirrored_files;
pub mod snapshot;
pub mod vendor_neutral_core;

/// Anything that stopped a check from running.
pub type Error = Box<dyn std::error::Error>;

/// Repository root, resolved from this crate's manifest directory.
pub fn repo_root() -> std::path::PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(manifest).to_path_buf()
}

/// The base ref to diff against, from a `--base <ref>` argument or `origin/main` by default.
///
/// Shared by the checks that ask "what changed since the branch point?" — the snapshot removal gate
/// and the migration immutability gate both need it.
pub fn base_ref(args: &[String]) -> String {
    let mut iter = args.iter();
    while let Some(argument) = iter.next() {
        if argument == "--base"
            && let Some(value) = iter.next()
        {
            return value.clone();
        }
    }
    "origin/main".to_owned()
}

/// A file's contents at `reference`, or `None` if it did not exist there.
pub fn read_at_ref(
    root: &std::path::Path,
    reference: &str,
    path: &str,
) -> Result<Option<String>, Error> {
    let output = std::process::Command::new("git")
        .args(["show", &format!("{reference}:{path}")])
        .current_dir(root)
        .output()?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    } else {
        Ok(None)
    }
}
