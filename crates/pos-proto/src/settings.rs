// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The register of settings: every value a store may run differently
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 9).
//!
//! A setting is a typed field on a published node, and its default is what the edge did before the
//! field existed. [`register`] names each setting once: its node and field, the values it takes, its
//! default, where it may be written, and the first release that honours it. Two committed files are
//! rendered from that list:
//!
//! - `docs/snapshots/settings.txt`, one fact per line. `cargo xtask snapshot` diffs it against the
//!   base branch, so a setting, one of its values or one of its scopes cannot disappear unseen.
//! - `docs/configuration.md`, the register an operator reads.
//!
//! A test reads every value of every setting back through its node's own type, so the register
//! cannot offer a value or claim a default that the edge does not have.

use crate::shift::{NoShiftSelling, PublishedShift};
use crate::wire_enum;
use crate::wire_enum::WireEnum;

/// Where the rendered snapshot is committed, relative to the repository root.
pub const SNAPSHOT_PATH: &str = "docs/snapshots/settings.txt";

/// Where the rendered register is committed, relative to the repository root.
pub const DOC_PATH: &str = "docs/configuration.md";

wire_enum! {
    /// Where a setting may be written (ADR-0160 decision 3).
    SettingScope, prefix = "SETTING_SCOPE";
    /// Every store of the tenant.
    Tenant = "TENANT",
    /// Every store of one brand.
    Brand = "BRAND",
    /// Every store in one store group.
    StoreGroup = "STORE_GROUP",
    /// One store.
    Store = "STORE",
}

wire_enum! {
    /// The shape of a setting's value, from which the console draws its form.
    SettingKind, prefix = "SETTING_KIND";
    /// One value from a fixed list, each value a wire token.
    Choice = "CHOICE",
}

/// One setting: a field on a published node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    /// The published node the value travels in, such as `shift`.
    pub node: &'static str,
    /// The field on that node, such as `no_shift_selling`.
    pub field: &'static str,
    /// The shape of the value.
    pub kind: SettingKind,
    /// For a [`SettingKind::Choice`], every value the setting takes, as wire tokens.
    pub values: Vec<&'static str>,
    /// The value a store runs when nothing sets one, which is what the edge did before the setting
    /// existed (ADR-0160 decision 1).
    pub default: &'static str,
    /// Where the setting may be written.
    pub scopes: &'static [SettingScope],
    /// The first release that honours the setting. A store running an earlier release is not
    /// offered it (ADR-0160 decision 5).
    pub since: &'static str,
    /// What the setting decides, in one sentence.
    pub summary: &'static str,
}

impl Setting {
    /// The setting's key, `node.field`, which names it in the register and in the console.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}.{}", self.node, self.field)
    }
}

/// The scopes of a setting that applies to a whole store: every scope ADR-0160 decision 3 names.
const STORE_WIDE: &[SettingScope] = &[
    SettingScope::Tenant,
    SettingScope::Brand,
    SettingScope::StoreGroup,
    SettingScope::Store,
];

/// The first release after 0.14.0. Tags are cut from `main`, so the next release carries everything
/// merged before it, whatever number it is given.
const NEXT_RELEASE: &str = "0.14.1";

/// Every setting, in the order the register lists them.
#[must_use]
pub fn register() -> Vec<Setting> {
    vec![Setting {
        node: PublishedShift::NODE,
        field: "no_shift_selling",
        kind: SettingKind::Choice,
        values: choices::<NoShiftSelling>(),
        default: PublishedShift::default().no_shift_selling().as_wire(),
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        summary: "Whether a till may seat a table, start a counter order or take a payment while \
                  no shift is open. `NO_SHIFT_SELLING_REFUSE` refuses each of them with \
                  `OPEN_SHIFT_REQUIRED` until a shift opens.",
    }]
}

/// Every value of a wire enum but its `*_UNSPECIFIED`, which is never a choice.
fn choices<E: WireEnum>() -> Vec<&'static str> {
    E::ALL
        .iter()
        .filter(|value| **value != E::UNSPECIFIED)
        .map(|value| value.as_wire())
        .collect()
}

/// Renders the register in the committed snapshot format, one fact per line.
///
/// Sorted, so the output depends on what the register holds and not on its order. A bare key, a
/// `kind=`, a `value=` and a `scope=` line are contracts: a value stored in the cloud and an edge on
/// an older release both rely on them. A `default=` and a `since=` line may change.
#[must_use]
pub fn render_snapshot() -> String {
    let mut lines: Vec<String> = Vec::new();
    for setting in register() {
        let key = setting.key();
        lines.push(key.clone());
        lines.push(format!("{key}\tkind={}", setting.kind));
        for value in &setting.values {
            lines.push(format!("{key}\tvalue={value}"));
        }
        lines.push(format!("{key}\tdefault={}", setting.default));
        for scope in setting.scopes {
            lines.push(format!("{key}\tscope={scope}"));
        }
        lines.push(format!("{key}\tsince={}", setting.since));
    }
    lines.sort();

    let mut out = String::from(
        "# Settings register snapshot. Generated from crates/pos-proto/src/settings.rs — do not\n\
         # hand-edit. Regenerate with:  just snapshot\n\
         #\n\
         # A REMOVED key, kind, value or scope line fails CI: a stored value and an older edge both\n\
         # rely on it, so it may be added to but never renamed or removed. A default or a since\n\
         # line may change. ADR-0160 decision 9.\n",
    );
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Renders `docs/configuration.md`, the register as an operator reads it.
#[must_use]
pub fn render_markdown() -> String {
    let mut out = String::from("# Configuration register\n\n");
    out.push_str(
        "Generated from `crates/pos-proto/src/settings.rs`. Do not edit by hand — run \
         `just snapshot`.\n\n\
         Every value a store may run differently is a setting: a typed field on a node of the \
         store's published configuration \
         ([ADR-0160](adr/0160-everything-a-store-runs-differently-is-published-configuration.md)). \
         A store whose configuration does not set a value runs the default, which is what the \
         edge did before the setting existed, so an upgrade changes nothing until someone sets \
         one. A value the edge does not recognise also reads as the default.\n\n\
         - **Where it is set** lists the scopes a value may be written at (ADR-0160 decision \
         3).\n\
         - **Honoured from** is the first release that honours the setting. A store running an \
         earlier release ignores it.\n\n",
    );
    out.push_str(
        "| Setting | Values | Default | Where it is set | Honoured from | What it decides |\n",
    );
    out.push_str("|---|---|---|---|---|---|\n");
    for setting in register() {
        let values = setting
            .values
            .iter()
            .map(|value| format!("`{value}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let scopes = setting
            .scopes
            .iter()
            .map(|scope| scope_label(*scope))
            .collect::<Vec<_>>()
            .join(", ");
        let cells = [
            format!("`{}`", setting.key()),
            values,
            format!("`{}`", setting.default),
            scopes,
            setting.since.to_owned(),
            setting.summary.to_owned(),
        ];
        for cell in cells {
            out.push_str("| ");
            out.push_str(&cell);
            out.push(' ');
        }
        out.push_str("|\n");
    }
    out
}

/// A scope as the register's reader calls it.
fn scope_label(scope: SettingScope) -> &'static str {
    match scope {
        SettingScope::Unspecified => "unspecified",
        SettingScope::Tenant => "tenant",
        SettingScope::Brand => "brand",
        SettingScope::StoreGroup => "store group",
        SettingScope::Store => "store",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::{DOC_PATH, SNAPSHOT_PATH, SettingKind, register, render_markdown, render_snapshot};
    use crate::shift::PublishedShift;
    use crate::wire_enum::WireEnum;

    fn committed(path: &str) -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    }

    /// Compares a rendering with its committed file, or rewrites the file under
    /// `POS_UPDATE_SNAPSHOTS=1`. Opt-in, because a check that fixes itself is not a check.
    fn check_committed(path: &str, rendered: &str) {
        let file = committed(path);
        if std::env::var_os("POS_UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(&file, rendered).expect("write the committed file");
            return;
        }
        let on_disk = std::fs::read_to_string(&file).unwrap_or_else(|_| {
            panic!("{path} is missing. Generate it with:\n\n    just snapshot\n")
        });
        assert!(
            on_disk == rendered,
            "{path} no longer matches the register in crates/pos-proto/src/settings.rs. \
             Regenerate it with:\n\n    just snapshot\n"
        );
    }

    #[test]
    fn the_committed_settings_snapshot_matches_the_register() {
        check_committed(SNAPSHOT_PATH, &render_snapshot());
    }

    #[test]
    fn the_committed_configuration_doc_matches_the_register_snapshot() {
        // Named for `just snapshot`, which regenerates every test of this crate whose name says
        // "snapshot".
        check_committed(DOC_PATH, &render_markdown());
    }

    #[test]
    fn the_renderings_are_deterministic() {
        assert_eq!(render_snapshot(), render_snapshot());
        assert_eq!(render_markdown(), render_markdown());
    }

    #[test]
    fn every_key_is_unique_and_snake_case() {
        let mut seen = BTreeSet::new();
        for setting in register() {
            let key = setting.key();
            assert!(seen.insert(key.clone()), "{key} is registered twice");
            for part in [setting.node, setting.field] {
                assert!(
                    !part.is_empty()
                        && part
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "{key}: `{part}` is not snake_case"
                );
            }
        }
    }

    #[test]
    fn every_setting_has_a_default_among_its_values_a_scope_and_a_release() {
        for setting in register() {
            let key = setting.key();
            assert_eq!(
                setting.kind,
                SettingKind::Choice,
                "{key}: a new kind needs a reader below"
            );
            assert!(
                setting.values.contains(&setting.default),
                "{key}: the default `{}` is not one of its values",
                setting.default
            );
            assert!(!setting.scopes.is_empty(), "{key} can be set nowhere");
            assert!(!setting.since.is_empty(), "{key} names no release");
            assert!(
                !setting.summary.is_empty(),
                "{key} says nothing about itself"
            );
        }
    }

    /// Reads one value of a setting back through its node's own type, as the edge reads it.
    ///
    /// A setting on a node this function has no arm for fails the test below, so a new node cannot
    /// join the register without the reader that proves the register right about it.
    fn read_back(node: &str, field: &str, value: &str) -> Option<String> {
        let document = serde_json::json!({ field: value });
        match node {
            PublishedShift::NODE => {
                let shift: PublishedShift = serde_json::from_value(document).ok()?;
                match field {
                    "no_shift_selling" => Some(shift.no_shift_selling().as_wire().to_owned()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    #[test]
    fn every_value_reads_back_through_its_node() {
        for setting in register() {
            for value in &setting.values {
                assert_eq!(
                    read_back(setting.node, setting.field, value).as_deref(),
                    Some(*value),
                    "{}: the edge does not read `{value}` as the register says",
                    setting.key()
                );
            }
        }
    }

    #[test]
    fn the_default_is_what_an_empty_node_reads_as() {
        for setting in register() {
            let empty = match setting.node {
                PublishedShift::NODE => {
                    let shift: PublishedShift =
                        serde_json::from_value(serde_json::json!({})).expect("an empty node");
                    shift.no_shift_selling().as_wire()
                }
                other => panic!("no reader for the `{other}` node"),
            };
            assert_eq!(empty, setting.default, "{}", setting.key());
        }
    }
}
