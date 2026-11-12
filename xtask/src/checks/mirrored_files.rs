// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The files both front ends share live once, in `packages/shared/`, and nowhere else
//! ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 1).
//!
//! The operator UI (`ui/`) and the console (`dashboard/`) are separate pnpm packages with separate
//! Vite builds. Some files belong to both: the design tokens (ADR-0060 has the console reuse the P6
//! token set, and the WCAG-AA guarantee in `docs/wcag-contrast-audit.md` depends on one palette),
//! the contrast gate that checks them, the two i18n gates
//! ([ADR-0020](../../../docs/adr/0020-i18n-runtime.md)), and ESLint's size rules with their baseline
//! script ([ADR-0172](../../../docs/adr/0172-eslint-checks-the-front-ends.md)). They used to be
//! copied into both roots and held byte-identical by this check. Now each root reaches the one copy
//! as `../packages/shared/…`: a CSS `@import`, a `node ../packages/shared/scripts/…` in its
//! `package.json`, and an import in its `eslint.config.mjs`.
//!
//! The package renderer's core lives the same way in `packages/package-renderer/`
//! ([ADR-0209](../../../docs/adr/0209-the-console-previews-a-screen-with-the-tills-renderer.md)
//! decision 1): the tree walk, the contexts, variant resolution, the components that draw from the
//! package alone and the two catalogues `pos-proto` writes. Neither front end keeps a copy.
//!
//! So the check is now the other way round. It fails if a shared file is missing from
//! `packages/shared/`, or if a copy reappears in a front end, which is how the drift started. The
//! two `eslint.config.mjs` files that import the shared rules are the one remaining pair, and must
//! stay identical.

use super::{Error, repo_root};
use crate::Finding;

const RULE: &str = "mirrored-files";

/// Each shared file, and the paths in the front ends where its copies used to be.
const SHARED: &[(&str, &[&str])] = &[
    (
        "packages/shared/styles/tokens.css",
        &[
            "ui/src/styles/tokens.css",
            "dashboard/src/styles/tokens.css",
        ],
    ),
    (
        "packages/shared/scripts/wcag-contrast.mjs",
        &[
            "ui/scripts/wcag-contrast.mjs",
            "dashboard/scripts/wcag-contrast.mjs",
        ],
    ),
    (
        "packages/shared/scripts/i18n-lint.mjs",
        &[
            "ui/scripts/i18n-lint.mjs",
            "dashboard/scripts/i18n-lint.mjs",
        ],
    ),
    (
        "packages/shared/scripts/i18n-parity.mjs",
        &[
            "ui/scripts/i18n-parity.mjs",
            "dashboard/scripts/i18n-parity.mjs",
        ],
    ),
    (
        "packages/shared/scripts/eslint-baseline.mjs",
        &[
            "ui/scripts/eslint-baseline.mjs",
            "dashboard/scripts/eslint-baseline.mjs",
        ],
    ),
    ("packages/shared/eslint/size.mjs", &[]),
    // The package renderer's core (ADR-0209): the till drew from these, and the console kept its
    // own copy of the two catalogues `pos-proto` writes.
    (
        "packages/package-renderer/src/components.ts",
        &[
            "ui/src/package/components.ts",
            "dashboard/src/package/components.ts",
        ],
    ),
    (
        "packages/package-renderer/src/actions.ts",
        &[
            "ui/src/package/actions.ts",
            "dashboard/src/package/actions.ts",
        ],
    ),
    (
        "packages/package-renderer/src/render.tsx",
        &[
            "ui/src/package/render.tsx",
            "dashboard/src/package/render.tsx",
        ],
    ),
    (
        "packages/package-renderer/src/base.tsx",
        &["dashboard/src/package/base.tsx"],
    ),
    (
        "packages/package-renderer/src/regions.tsx",
        &[
            "ui/src/package/regions.tsx",
            "dashboard/src/package/regions.tsx",
        ],
    ),
    (
        "packages/package-renderer/src/variant.ts",
        &[
            "ui/src/package/variant.ts",
            "dashboard/src/package/variant.ts",
        ],
    ),
];

/// The one pair still held identical: each front end's thin ESLint entry point.
const IDENTICAL: (&str, &str) = ("ui/eslint.config.mjs", "dashboard/eslint.config.mjs");

/// Runs the check.
///
/// # Errors
///
/// Never; an unreadable file is a finding.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let mut findings = Vec::new();
    for (shared, old_copies) in SHARED {
        if !root.join(shared).is_file() {
            findings.push(
                Finding::new(*shared, RULE, "the shared file is missing")
                    .with_hint("both front ends import it from packages/; restore it there"),
            );
        }
        for copy in *old_copies {
            if root.join(copy).exists() {
                findings.push(
                    Finding::new(*copy, RULE, format!("is a copy of `{shared}`")).with_hint(
                        format!(
                            "delete it and use `{shared}` from both front ends, as `../packages/…`"
                        ),
                    ),
                );
            }
        }
    }
    let (left, right) = IDENTICAL;
    match (
        std::fs::read(root.join(left)),
        std::fs::read(root.join(right)),
    ) {
        (Ok(a), Ok(b)) if a == b => {}
        (Ok(_), Ok(_)) => findings.push(
            Finding::new(right, RULE, format!("differs from `{left}`"))
                .with_hint("both only import the shared size rules; make them the same"),
        ),
        (Err(error), _) => findings.push(Finding::new(
            left,
            RULE,
            format!("could not be read: {error}"),
        )),
        (_, Err(error)) => findings.push(Finding::new(
            right,
            RULE,
            format!("could not be read: {error}"),
        )),
    }
    Ok(findings)
}
