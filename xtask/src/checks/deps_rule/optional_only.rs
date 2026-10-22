// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `optional-only` allow-list entries: a crate the backbone may declare only as an optional
//! dependency that no default feature turns on, as `pos-proto` declares `utoipa` behind `openapi`
//! ([ADR-0177](../../../../docs/adr/0177-openapi-is-the-route-map.md) decision 3).

use std::collections::BTreeSet;

use super::{Dependency, Package};
use crate::Finding;

/// An `optional-only` crate that is not optional, or that a default feature turns on.
pub(super) fn findings(pkg: &Package, manifest: &str, dep: &Dependency) -> Vec<Finding> {
    let name = pkg.name.as_str();
    let hint = "declare it `optional = true` and enable it only from a feature `default` does \
                not reach, as `pos-proto`'s `openapi` is";
    if !dep.optional {
        return vec![
            Finding::new(
                manifest,
                "dependency-rule",
                format!(
                    "`{name}` declares `{}` unconditionally, which is allow-listed only as an \
                     optional dependency",
                    dep.name
                ),
            )
            .with_hint(hint),
        ];
    }
    if default_enables(pkg, &dep.name) {
        return vec![
            Finding::new(
                manifest,
                "dependency-rule",
                format!(
                    "`{name}`'s default features enable `{}`, which is allow-listed only when \
                     no default feature turns it on",
                    dep.name
                ),
            )
            .with_hint(hint),
        ];
    }
    Vec::new()
}

/// Whether `pkg`'s `default` feature, followed through the features it enables, turns on the
/// optional dependency `dependency`. A weak `dependency?/feature` does not.
fn default_enables(pkg: &Package, dependency: &str) -> bool {
    let mut pending = vec!["default".to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(feature) = pending.pop() {
        if !seen.insert(feature.clone()) {
            continue;
        }
        for entry in pkg.features.get(&feature).into_iter().flatten() {
            let enables = entry == dependency
                || entry.strip_prefix("dep:") == Some(dependency)
                || entry
                    .split_once('/')
                    .is_some_and(|(crate_part, _)| crate_part == dependency);
            if enables {
                return true;
            }
            if pkg.features.contains_key(entry) {
                pending.push(entry.clone());
            }
        }
    }
    false
}
