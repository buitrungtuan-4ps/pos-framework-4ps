// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The individual repository checks.
//!
//! Each exposes `run(args) -> Result<Vec<Finding>, Error>`. A check returns an
//! `Err` only when it could not run at all; a rule violation is a `Finding`, so
//! one broken rule does not hide the next.

pub mod actions_pinned;
pub mod deps_rule;
pub mod links;
pub mod lint_config;
pub mod snapshot;

/// Anything that stopped a check from running.
pub type Error = Box<dyn std::error::Error>;

/// Repository root, resolved from this crate's manifest directory.
pub fn repo_root() -> std::path::PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(manifest).to_path_buf()
}
