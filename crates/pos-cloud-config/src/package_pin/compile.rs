// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which package version a store's document carries
//! ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decisions 3,
//! 5 and 7), from its resolved pin, the pinned version and the release the store reported.
//!
//! Pure: the caller reads the version and the release and applies the answer to the `package` node
//! on the store's Tenant layer.

use pos_core::ota::ReleaseVersion;
use pos_proto::ids::PackageVersionId;
use pos_proto::package::{PackageContent, PackageMode};
use pos_proto::wire_enum::WireEnum as _;
use serde::Serialize;

use crate::packages::PackageVersionRecord;
use crate::settings::ResolvedSetting;

/// The version a store's resolved pin names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pin {
    /// No pin: the store runs its release's default package.
    Unpinned,
    /// The pin names this version.
    Version(PackageVersionId),
    /// The pin names nothing it can run: a lock with no version, or a choice whose chosen and
    /// default versions are both outside its allowed versions.
    Unresolved,
}

/// Why a pinned store keeps the version it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WaitReason {
    /// The store's release is older than the version's `min_edge_version`.
    EdgeTooOld,
    /// The store has never reported its release, so the cloud cannot tell.
    ReleaseUnknown,
    /// The pinned version is not a published version of the tenant's packages.
    VersionMissing,
    /// The pin names no version the store may run.
    PinUnresolved,
    /// The package store or the store's release could not be read; the next publish tries again.
    Unreadable,
}

/// What the store's document does with the `package` node's `content`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentChange {
    /// No pin: no content, so the store runs the default package.
    Remove,
    /// The pinned version's content.
    Set(Box<PackageContent>),
    /// The content the store already runs stays, if it runs one.
    Keep(WaitReason),
}

/// The version a store's resolved settings pin it to.
#[must_use]
pub fn pinned_version(resolved: &[ResolvedSetting]) -> Pin {
    let field = |name: &str| {
        resolved
            .iter()
            .find(|setting| setting.node == "package" && setting.field == name)
            .map(|setting| &setting.value)
    };
    let id = |name: &str| {
        field(name)
            .and_then(serde_json::Value::as_str)
            .and_then(|text| text.parse::<PackageVersionId>().ok())
    };
    let mode = field("mode").and_then(serde_json::Value::as_str);
    if mode == Some(PackageMode::Locked.as_wire()) {
        return id("locked_version_id").map_or(Pin::Unresolved, Pin::Version);
    }
    if mode != Some(PackageMode::Choose.as_wire()) {
        return Pin::Unpinned;
    }
    let allowed: Vec<PackageVersionId> = field("allowed_version_ids")
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_str)
                .filter_map(|text| text.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    [id("chosen_version_id"), id("default_version_id")]
        .into_iter()
        .flatten()
        .find(|candidate| allowed.contains(candidate))
        .map_or(Pin::Unresolved, Pin::Version)
}

/// What the store's document carries, given its pin, the pinned version as the package store holds
/// it (`None` when it holds none), and the release the store last reported.
///
/// A version is compiled in only for a store whose reported release meets the version's
/// `min_edge_version`; any other store keeps what it runs (decision 5).
#[must_use]
pub fn compile(
    pin: Pin,
    version: Option<&PackageVersionRecord>,
    installed: Option<&str>,
) -> ContentChange {
    match pin {
        Pin::Unpinned => ContentChange::Remove,
        Pin::Unresolved => ContentChange::Keep(WaitReason::PinUnresolved),
        Pin::Version(_) => {
            let Some(version) = version else {
                return ContentChange::Keep(WaitReason::VersionMissing);
            };
            let Some(installed) = installed.and_then(parse_release) else {
                return ContentChange::Keep(WaitReason::ReleaseUnknown);
            };
            match parse_release(version.content.min_edge_version.as_str()) {
                Some(needed) if installed >= needed => {
                    ContentChange::Set(Box::new(version.content.clone()))
                }
                _ => ContentChange::Keep(WaitReason::EdgeTooOld),
            }
        }
    }
}

/// The settings nodes for a store's Tenant layer with `change` made to the `package` node's
/// `content`: set, removed, or left as the layer held it. A `package` node left with nothing is
/// dropped only when the layer did not hold one.
#[must_use]
pub fn with_content(
    mut nodes: Vec<(String, serde_json::Value)>,
    change: &ContentChange,
) -> Vec<(String, serde_json::Value)> {
    const NODE: &str = "package";
    let content = match change {
        ContentChange::Keep(_) => return nodes,
        ContentChange::Remove => None,
        ContentChange::Set(content) => serde_json::to_value(content.as_ref()).ok(),
    };
    if let Some((_, node)) = nodes.iter_mut().find(|(name, _)| name == NODE) {
        if let Some(fields) = node.as_object_mut() {
            match content {
                Some(content) => {
                    fields.insert("content".to_owned(), content);
                }
                None => {
                    fields.remove("content");
                }
            }
        }
    } else if let Some(content) = content {
        nodes.push((NODE.to_owned(), serde_json::json!({ "content": content })));
    }
    nodes
}

/// A release as a store reports it, `0.14.1` or `v0.14.1`.
fn parse_release(text: &str) -> Option<ReleaseVersion> {
    ReleaseVersion::parse(text.trim().trim_start_matches('v'))
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::{PackageId, PackageVersionId};
    use pos_proto::package::PackageContent;
    use pos_proto::settings::SettingScope;
    use pos_proto::text::ReleaseTag;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;
    use serde_json::{Value, json};

    use super::{ContentChange, Pin, WaitReason, compile, pinned_version, with_content};
    use crate::packages::PackageVersionRecord;
    use crate::settings::ResolvedSetting;

    const V1: &str = "01J00000000000000000000V01";
    const V2: &str = "01J00000000000000000000V02";

    fn id(text: &str) -> PackageVersionId {
        text.parse().expect("an id")
    }

    fn pin(fields: &[(&'static str, Value)]) -> Pin {
        let resolved: Vec<ResolvedSetting> = fields
            .iter()
            .map(|(field, value)| ResolvedSetting {
                node: "package",
                field,
                value: value.clone(),
                scope: SettingScope::Tenant,
                scope_id: "T".to_owned(),
            })
            .collect();
        pinned_version(&resolved)
    }

    fn version(min_edge_version: &str) -> PackageVersionRecord {
        PackageVersionRecord {
            content: PackageContent {
                package_id: PackageId::new(Ulid::from_u128(1)),
                package_version_id: id(V1),
                version_label: "March".to_owned(),
                min_edge_version: ReleaseTag::new(min_edge_version),
                time_slots: Vec::new(),
                flows: Vec::new(),
                screens: Vec::new(),
                data: pos_proto::package::PackageData::default(),
                reactions: Vec::new(),
            },
            create_time: Timestamp::EPOCH,
        }
    }

    #[test]
    fn a_lock_pins_its_version_and_no_mode_pins_nothing() {
        assert_eq!(pin(&[]), Pin::Unpinned);
        assert_eq!(
            pin(&[
                ("mode", json!("PACKAGE_MODE_UNSPECIFIED")),
                ("locked_version_id", json!(V1))
            ]),
            Pin::Unpinned
        );
        assert_eq!(
            pin(&[
                ("mode", json!("PACKAGE_MODE_LOCKED")),
                ("locked_version_id", json!(V1))
            ]),
            Pin::Version(id(V1))
        );
        assert_eq!(
            pin(&[("mode", json!("PACKAGE_MODE_LOCKED"))]),
            Pin::Unresolved
        );
    }

    #[test]
    fn a_choice_runs_the_chosen_version_then_the_default_while_allowed() {
        let choose = |chosen: Option<&str>, default: Option<&str>, allowed: &[&str]| {
            let mut fields = vec![
                ("mode", json!("PACKAGE_MODE_CHOOSE")),
                ("allowed_version_ids", json!(allowed)),
            ];
            if let Some(chosen) = chosen {
                fields.push(("chosen_version_id", json!(chosen)));
            }
            if let Some(default) = default {
                fields.push(("default_version_id", json!(default)));
            }
            pin(&fields)
        };
        assert_eq!(choose(Some(V2), Some(V1), &[V1, V2]), Pin::Version(id(V2)));
        assert_eq!(choose(None, Some(V1), &[V1, V2]), Pin::Version(id(V1)));
        assert_eq!(
            choose(Some(V2), Some(V1), &[V1]),
            Pin::Version(id(V1)),
            "a choice no longer allowed falls back to the default"
        );
        assert_eq!(choose(Some(V2), Some(V1), &[]), Pin::Unresolved);
    }

    #[test]
    fn a_version_is_compiled_only_for_a_release_that_meets_it() {
        let march = version("0.14.1");
        let pinned = Pin::Version(id(V1));
        assert_eq!(
            compile(pinned, Some(&march), Some("0.14.1")),
            ContentChange::Set(Box::new(march.content.clone()))
        );
        assert_eq!(
            compile(pinned, Some(&march), Some("v0.15.0")),
            ContentChange::Set(Box::new(march.content.clone()))
        );
        assert_eq!(
            compile(pinned, Some(&march), Some("0.14.0")),
            ContentChange::Keep(WaitReason::EdgeTooOld)
        );
        assert_eq!(
            compile(pinned, Some(&march), None),
            ContentChange::Keep(WaitReason::ReleaseUnknown)
        );
        assert_eq!(
            compile(pinned, Some(&march), Some("dev")),
            ContentChange::Keep(WaitReason::ReleaseUnknown)
        );
        assert_eq!(
            compile(pinned, None, Some("0.14.1")),
            ContentChange::Keep(WaitReason::VersionMissing)
        );
        assert_eq!(
            compile(Pin::Unresolved, None, Some("0.14.1")),
            ContentChange::Keep(WaitReason::PinUnresolved)
        );
        assert_eq!(compile(Pin::Unpinned, None, None), ContentChange::Remove);
    }

    #[test]
    fn the_content_is_set_removed_or_kept_on_the_package_node() {
        let march = version("0.14.1");
        let nodes = vec![(
            "package".to_owned(),
            json!({ "mode": "PACKAGE_MODE_LOCKED", "content": { "old": true } }),
        )];
        let set = with_content(
            nodes.clone(),
            &ContentChange::Set(Box::new(march.content.clone())),
        );
        assert_eq!(set[0].1["content"]["package_version_id"], V1);
        assert_eq!(set[0].1["mode"], "PACKAGE_MODE_LOCKED");
        let removed = with_content(nodes.clone(), &ContentChange::Remove);
        assert_eq!(removed[0].1, json!({ "mode": "PACKAGE_MODE_LOCKED" }));
        let kept = with_content(nodes.clone(), &ContentChange::Keep(WaitReason::EdgeTooOld));
        assert_eq!(kept, nodes);
        assert!(with_content(Vec::new(), &ContentChange::Remove).is_empty());
    }
}
