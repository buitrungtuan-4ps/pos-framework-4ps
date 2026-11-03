// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `package` node on the edge
//! ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 6).
//!
//! A pulled document whose package fails is refused whole: the store keeps the previous document,
//! and the refusal is kept in [`PackageStanding`] for the heartbeat to report. The package fails
//! when the node does not read as a `package` node, when its content breaks a rule the cloud checks
//! at publish, or when it needs a newer edge than this one.
//!
//! The flows a package carries are checked here too (ADR-0192 decision 2), with the rule evaluator
//! `pos_edge_app::app::flows` holds: limits, ids, references, cycles, and every condition compiled,
//! an edge guard's reading only what the edge holds. Until a package carries its screens (ADR-0190),
//! a step's `screen_id` is not resolved here.

use std::sync::{Arc, Mutex, PoisonError};

use pos_core::flow::check_flows;
use pos_core::ota::ReleaseVersion;
use pos_core::package::check_content;
use pos_edge_app::app::flows::EdgeConditions;
use pos_proto::package::{PackageContent, PublishedPackage};

/// Why the edge refuses a document's package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageRefusal {
    /// The node does not read as a `package` node.
    Malformed,
    /// The content breaks a rule the cloud checks at publish; the message says which.
    Invalid(String),
    /// The version needs a newer edge than this one, or this edge cannot tell its own release.
    NeedsNewerEdge {
        /// The release the version needs.
        min_edge_version: String,
    },
}

impl PackageRefusal {
    /// The reason as a token: `PACKAGE_MALFORMED`, `PACKAGE_INVALID` or `EDGE_TOO_OLD`.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::Malformed => "PACKAGE_MALFORMED",
            Self::Invalid(_) => "PACKAGE_INVALID",
            Self::NeedsNewerEdge { .. } => "EDGE_TOO_OLD",
        }
    }
}

/// A document the edge refused for its package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedPackage {
    /// The configuration version refused.
    pub config_version_id: String,
    /// The package version it carried, when that could be read.
    pub package_version_id: Option<String>,
    /// Why.
    pub refusal: PackageRefusal,
}

/// The last package refusal, shared between the config loop that records it and whoever reports it.
/// Cleared when a document is applied.
#[derive(Debug, Default)]
pub struct PackageStanding {
    refused: Mutex<Option<RefusedPackage>>,
}

impl PackageStanding {
    /// The last refusal, if the latest document was refused.
    #[must_use]
    pub fn refused(&self) -> Option<RefusedPackage> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(super) fn record(&self, refused: Option<RefusedPackage>) {
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = refused;
    }
}

/// The package a document carries, once it passes: `None` when the document pins nothing, which is
/// the default package of this release.
///
/// `edge_release` is this binary's release ([`crate::version::released`]); `None`, a version that
/// does not parse, refuses every package rather than run one it cannot vouch for.
///
/// # Errors
///
/// [`PackageRefusal`] naming why the package cannot run here.
pub fn checked_package(
    document: &serde_json::Value,
    edge_release: Option<ReleaseVersion>,
) -> Result<Option<Arc<PackageContent>>, PackageRefusal> {
    let Some(node) = document.get(PublishedPackage::NODE) else {
        return Ok(None);
    };
    let node: PublishedPackage =
        serde_json::from_value(node.clone()).map_err(|_| PackageRefusal::Malformed)?;
    let Some(content) = node.content else {
        return Ok(None);
    };
    check_content(&content).map_err(|refusal| PackageRefusal::Invalid(refusal.to_string()))?;
    check_flows(
        &content.flows,
        &|_screen| true,
        &EdgeConditions::for_apply(),
    )
    .map_err(|refusal| PackageRefusal::Invalid(refusal.to_string()))?;
    let needed = ReleaseVersion::parse(content.min_edge_version.as_str().trim_start_matches('v'));
    match (needed, edge_release) {
        (Some(needed), Some(release)) if release >= needed => Ok(Some(Arc::new(content))),
        _ => Err(PackageRefusal::NeedsNewerEdge {
            min_edge_version: content.min_edge_version.to_string(),
        }),
    }
}

/// Puts the document's package on the session. The pull refuses a document whose package fails
/// before it gets here; a document restored at boot did not pass that check on this binary, so a
/// package that fails here leaves the session's own, which at boot is none: the default package.
pub(super) fn apply_package(
    session: &mut pos_edge_app::app::EdgeSession,
    document: &serde_json::Value,
) {
    match checked_package(document, crate::version::released()) {
        Ok(package) => session.package = package,
        Err(refusal) => tracing::warn!(
            reason = refusal.reason(),
            "a stored store config carries a package this edge cannot run; running without it"
        ),
    }
}

/// Whether `synced` is refused for its package. A refusal is logged and recorded in `standing`; a
/// document that passes clears it.
pub(super) fn refuses(standing: &PackageStanding, synced: &super::SyncedConfig) -> bool {
    match checked_package(&synced.document, crate::version::released()) {
        Ok(_) => {
            standing.record(None);
            false
        }
        Err(refusal) => {
            tracing::warn!(
                version = %synced.config_version_id,
                reason = refusal.reason(),
                "refused a store config whose package this edge cannot run; keeping the previous one"
            );
            standing.record(Some(RefusedPackage {
                config_version_id: synced.config_version_id.clone(),
                package_version_id: package_version_of(&synced.document),
                refusal,
            }));
            true
        }
    }
}

/// The package version id a document carries, when it can be read, for a refusal's record.
fn package_version_of(document: &serde_json::Value) -> Option<String> {
    document
        .get(PublishedPackage::NODE)?
        .get("content")?
        .get("package_version_id")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use pos_core::ota::ReleaseVersion;
    use serde_json::{Value, json};

    use super::{PackageRefusal, checked_package, package_version_of};

    const V1: &str = "01J00000000000000000000V01";

    fn document(min_edge_version: &str, label: &str) -> Value {
        json!({ "package": {
            "mode": "PACKAGE_MODE_LOCKED",
            "locked_version_id": V1,
            "content": {
                "package_id": "01J00000000000000000000P01",
                "package_version_id": V1,
                "version_label": label,
                "min_edge_version": min_edge_version,
            },
        }})
    }

    const EDGE: Option<ReleaseVersion> = Some(ReleaseVersion::new(0, 14, 1));

    /// The document with `flows` in its content.
    fn with_flows(flows: Value) -> Value {
        let mut document = document("0.14.1", "March");
        document["package"]["content"]["flows"] = flows;
        document
    }

    /// Guest count first, as roadmap-v3 B8.4 asks: the edge sees it from the order's own count.
    fn guest_count_first(done_when: &str) -> Value {
        json!([{ "body": {
            "flow_id": "dine_in",
            "start_action_id": "sales.table.seat",
            "steps": [
                { "step_id": "guests", "action_id": "sales.table.seat",
                  "kind": "STEP_KIND_REQUIRED", "done_when": done_when },
                { "step_id": "first_line", "action_id": "sales.line.add",
                  "kind": "STEP_KIND_REQUIRED", "prerequisites": ["guests"] },
            ],
        }}])
    }

    /// A package's flows are checked on apply (ADR-0192 decision 2): a guard over the order's own
    /// facts passes; one that reads the till's view, does not compile, or waits on itself in a
    /// cycle refuses the whole package.
    #[test]
    fn a_packages_flows_are_checked_on_apply() {
        let passes = with_flows(guest_count_first(
            "has(order.guest_count) && order.guest_count > 0",
        ));
        let content = checked_package(&passes, EDGE)
            .expect("passes")
            .expect("a package");
        assert_eq!(content.flows.len(), 1);

        for done_when in [
            "device == 'DEVICE_CLASS_TABLET'",
            "order.items.all(i, i > 0)",
            "order.total * 1.5 > 0",
        ] {
            assert_eq!(
                checked_package(&with_flows(guest_count_first(done_when)), EDGE)
                    .map_err(|refusal| refusal.reason()),
                Err("PACKAGE_INVALID"),
                "{done_when}"
            );
        }

        let mut cycle = guest_count_first("has(order.guest_count)");
        cycle[0]["body"]["steps"][0]["prerequisites"] = json!(["first_line"]);
        assert_eq!(
            checked_package(&with_flows(cycle), EDGE).map_err(|refusal| refusal.reason()),
            Err("PACKAGE_INVALID")
        );
    }

    #[test]
    fn a_document_that_pins_nothing_carries_no_package() {
        assert_eq!(checked_package(&json!({}), EDGE), Ok(None));
        assert_eq!(
            checked_package(
                &json!({ "package": { "mode": "PACKAGE_MODE_UNSPECIFIED" } }),
                EDGE
            ),
            Ok(None)
        );
    }

    #[test]
    fn a_package_this_edge_runs_is_applied() {
        let content = checked_package(&document("0.14.1", "March"), EDGE)
            .expect("passes")
            .expect("a package");
        assert_eq!(content.package_version_id.to_string(), V1);
        assert!(checked_package(&document("v0.14.0", "March"), EDGE).is_ok());
    }

    #[test]
    fn a_package_this_edge_cannot_run_is_refused() {
        assert_eq!(
            checked_package(&document("0.15.0", "March"), EDGE),
            Err(PackageRefusal::NeedsNewerEdge {
                min_edge_version: "0.15.0".to_owned()
            })
        );
        assert_eq!(
            checked_package(&document("0.14.1", "March"), None).map_err(|refusal| refusal.reason()),
            Err("EDGE_TOO_OLD"),
            "an edge that cannot tell its own release runs no package"
        );
        assert_eq!(
            checked_package(&document("0.14.1", "  "), EDGE).map_err(|refusal| refusal.reason()),
            Err("PACKAGE_INVALID")
        );
        assert_eq!(
            checked_package(&json!({ "package": { "content": 7 } }), EDGE),
            Err(PackageRefusal::Malformed)
        );
        assert_eq!(
            package_version_of(&document("0.15.0", "March")).as_deref(),
            Some(V1)
        );
    }
}
