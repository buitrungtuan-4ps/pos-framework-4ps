// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The individual repository checks.
//!
//! Each exposes `run(args) -> Result<Vec<Finding>, Error>`. A check returns an
//! `Err` only when it could not run at all; a rule violation is a `Finding`, so
//! one broken rule does not hide the next.

pub mod actions_pinned;
pub mod changelog;
pub mod countries;
pub mod crates;
pub mod deps_rule;
pub mod guides;
pub mod links;
pub mod lint_config;
pub mod migrations;
pub mod mirrored_files;
pub mod print_agent_deps;
pub mod snapshot;
pub mod tls_modes;
pub mod vendor_neutral_core;

/// Anything that stopped a check from running.
pub type Error = Box<dyn std::error::Error>;

/// The repository root: the checkout the command runs in, by `git rev-parse --show-toplevel`,
/// when that checkout has an `xtask/` crate; otherwise the parent of this crate's manifest
/// directory.
///
/// Run time first, because `CARGO_MANIFEST_DIR` is fixed at compile time. Several worktrees sharing
/// one target directory can reuse one `xtask` binary, and a path compiled into it would check
/// whichever worktree built it last.
pub fn repo_root() -> std::path::PathBuf {
    let checkout = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
        .filter(|top| top.join("xtask/Cargo.toml").is_file());
    checkout.unwrap_or_else(|| {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().unwrap_or(manifest).to_path_buf()
    })
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
