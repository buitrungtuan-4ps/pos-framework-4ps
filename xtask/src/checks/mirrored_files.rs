// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Files that are deliberately duplicated across the two front-end build roots must stay identical.
//!
//! The operator UI (`ui/`) and the back-office dashboard (`dashboard/`) are separate pnpm packages
//! with separate Vite builds, so they cannot share a module the way two Rust crates share one. A few
//! files are therefore copied verbatim into both: the design tokens (ADR-0060 has the dashboard reuse
//! the P6 token set, and the WCAG-AA contrast guarantee — `docs/wcag-contrast-audit.md` — depends on
//! the palettes being the same in both) and the contrast gate that checks them. Copies drift silently:
//! a token darkened in one root but not the other would leave one surface failing AA with nothing to
//! catch it. This gate is the substitute for a shared module — it fails the build the moment a
//! mirrored pair diverges, so "deduplicated" is enforced rather than hoped for.

use super::{Error, repo_root};
use crate::Finding;

/// The pairs that must be byte-identical. Left is treated as the source of truth in the message.
const MIRRORED: &[(&str, &str)] = &[
    (
        "ui/src/styles/tokens.css",
        "dashboard/src/styles/tokens.css",
    ),
    (
        "ui/scripts/wcag-contrast.mjs",
        "dashboard/scripts/wcag-contrast.mjs",
    ),
];

/// Compares each mirrored pair and reports any that differ or are missing.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let mut findings = Vec::new();

    for (source, copy) in MIRRORED {
        let source_bytes = std::fs::read(root.join(source));
        let copy_bytes = std::fs::read(root.join(copy));
        match (source_bytes, copy_bytes) {
            (Ok(a), Ok(b)) if a == b => {}
            (Ok(_), Ok(_)) => findings.push(
                Finding::new(
                    *copy,
                    "mirrored-files",
                    format!("differs from its source of truth `{source}`"),
                )
                .with_hint(format!(
                    "copy `{source}` over `{copy}` so the two build roots match"
                )),
            ),
            (Ok(_), Err(error)) => findings.push(Finding::new(
                *copy,
                "mirrored-files",
                format!("could not be read to compare against `{source}`: {error}"),
            )),
            (Err(error), _) => findings.push(Finding::new(
                *source,
                "mirrored-files",
                format!("the source of truth could not be read: {error}"),
            )),
        }
    }

    Ok(findings)
}
