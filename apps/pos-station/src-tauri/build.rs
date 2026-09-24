// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Generates the Tauri context, and declares the app's own commands in an app manifest.
//!
//! The manifest is the security-relevant line. Without it, any window showing a bundled page may
//! call every app command; with it, a command is callable only where a capability grants
//! `allow-<command>`, and `capabilities/local-pages.json` grants them to the connect and status
//! windows alone. A remote page — the till, served by the edge — is refused either way: Tauri checks
//! a remote origin against the capabilities regardless, and none names a remote URL (ADR-0147).

use tauri_build::{AppManifest, Attributes};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tauri_build::try_build(
        Attributes::new().app_manifest(AppManifest::new().commands(&[
            "set_language",
            "connect_context",
            "pair",
            "status",
        ])),
    )?;
    Ok(())
}
