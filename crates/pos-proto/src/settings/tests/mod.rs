// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The register's tests: the shapes, the renderings, and every entry read back through its node.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    Confirm, DOC_PATH, SNAPSHOT_PATH, Setting, SettingKind, SettingScope, SettingShape,
    SettingUnit, ValueRefusal, register, render_markdown, render_markdown_of, render_snapshot,
    render_snapshot_of,
};
use crate::backup::{DEFAULT_INTERVAL_HOURS, PublishedBackup};
use crate::counter::PublishedCounter;
use crate::locale::LocaleSettings;
use crate::people::PublishedPermissions;
use crate::printing::{DEFAULT_FONT_SIZE_DOTS, PublishedPrinting};
use crate::qr::PublishedQr;
use crate::session::{DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES, PublishedSession};
use crate::shift::PublishedShift;
use crate::tender_keys::PublishedTenderKeys;
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
    let on_disk = std::fs::read_to_string(&file)
        .unwrap_or_else(|_| panic!("{path} is missing. Generate it with:\n\n    just snapshot\n"));
    assert!(
        on_disk == rendered,
        "{path} no longer matches the register in crates/pos-proto/src/settings.rs. \
         Regenerate it with:\n\n    just snapshot\n"
    );
}

/// A whole-number setting built here rather than taken from the register, so these tests pin the
/// shape and do not move when a register entry does.
fn seconds() -> Setting {
    Setting {
        node: "example",
        field: "wait_seconds",
        shape: SettingShape::Int {
            min: 0,
            max: 3600,
            unit: SettingUnit::Seconds,
            default: 0,
            preset: Some(120),
        },
        scopes: &[SettingScope::Tenant, SettingScope::Store],
        since: "0.15.0",
        confirm: None,
        summary: "How long to wait.",
    }
}

/// A choice built here whose second value needs a recorded confirmation (ADR-0169).
fn confirmed_choice() -> Setting {
    Setting {
        node: "example",
        field: "drawer_owner",
        shape: SettingShape::Choice {
            values: vec!["OWNER_SHARED", "OWNER_ONE_PERSON"],
            default: "OWNER_SHARED",
            preset: None,
        },
        scopes: &[SettingScope::Tenant, SettingScope::Store],
        since: "0.15.0",
        confirm: Some(Confirm {
            value: "OWNER_ONE_PERSON",
            notice: "settings.item.example.drawer_owner.confirm",
        }),
        summary: "Whose a drawer is.",
    }
}

/// An on-or-off setting built here, for the same reason.
fn switch() -> Setting {
    Setting {
        node: "example",
        field: "print_on_settle",
        shape: SettingShape::Bool {
            default: true,
            preset: None,
        },
        scopes: &[SettingScope::Store],
        since: "0.15.0",
        confirm: None,
        summary: "Whether to print.",
    }
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
fn a_choice_takes_one_of_its_tokens_and_nothing_else() {
    let Some(choice) = register()
        .into_iter()
        .find(|setting| setting.kind() == SettingKind::Choice)
    else {
        panic!("the register has a choice");
    };
    assert_eq!(choice.check(&json!("NO_SHIFT_SELLING_REFUSE")), Ok(()));
    for wrong in [
        json!("NO_SHIFT_SELLING_LATER"),
        json!(1),
        json!(true),
        Value::Null,
    ] {
        assert_eq!(
            choice.check(&wrong),
            Err(ValueRefusal::NotOneOfItsValues),
            "{wrong}"
        );
    }
}

#[test]
fn a_whole_number_takes_a_whole_number_from_its_minimum_to_its_maximum() {
    let setting = seconds();
    for good in [0, 1, 120, 3600] {
        assert_eq!(setting.check(&json!(good)), Ok(()), "{good}");
    }
    for outside in [-1, 3601, i64::MIN, i64::MAX] {
        assert_eq!(
            setting.check(&json!(outside)),
            Err(ValueRefusal::OutOfRange),
            "{outside}"
        );
    }
    // The JSON type is the contract: the edge reads an integer, so a float, a string or a
    // boolean that a lenient reader might coerce is refused here rather than misread there.
    for wrong in [
        json!(1.5),
        json!(120.0),
        json!("120"),
        json!(true),
        Value::Null,
        json!(u64::MAX),
    ] {
        assert_eq!(
            setting.check(&wrong),
            Err(ValueRefusal::NotAWholeNumber),
            "{wrong}"
        );
    }
    assert_eq!(setting.takes(), "a whole number from 0 to 3600");
}

#[test]
fn a_switch_takes_true_or_false_and_nothing_else() {
    let setting = switch();
    assert_eq!(setting.check(&json!(true)), Ok(()));
    assert_eq!(setting.check(&json!(false)), Ok(()));
    for wrong in [json!("true"), json!(1), json!(0), Value::Null] {
        assert_eq!(
            setting.check(&wrong),
            Err(ValueRefusal::NotTrueOrFalse),
            "{wrong}"
        );
    }
}

#[test]
fn a_whole_number_and_a_switch_carry_their_values_as_json() {
    assert_eq!(seconds().kind(), SettingKind::Int);
    assert_eq!(seconds().default_value(), json!(0));
    assert_eq!(seconds().preset_value(), Some(json!(120)));
    assert_eq!(switch().kind(), SettingKind::Bool);
    assert_eq!(switch().default_value(), json!(true));
    assert_eq!(switch().preset_value(), None);
}

#[test]
fn the_snapshot_names_a_whole_numbers_bounds_and_unit_and_a_switchs_two_values() {
    let snapshot = render_snapshot_of(&[seconds(), switch()]);
    for line in [
        "example.wait_seconds",
        "example.wait_seconds\tkind=SETTING_KIND_INT",
        "example.wait_seconds\tmin=0",
        "example.wait_seconds\tmax=3600",
        "example.wait_seconds\tunit=SETTING_UNIT_SECONDS",
        "example.wait_seconds\tdefault=0",
        "example.wait_seconds\tpreset=120",
        "example.wait_seconds\tscope=SETTING_SCOPE_TENANT",
        "example.print_on_settle\tkind=SETTING_KIND_BOOL",
        "example.print_on_settle\tvalue=false",
        "example.print_on_settle\tvalue=true",
        "example.print_on_settle\tdefault=true",
    ] {
        assert!(
            snapshot.lines().any(|held| held == line),
            "the snapshot has no line `{line}`:\n{snapshot}"
        );
    }
    assert!(
        !snapshot.contains("print_on_settle\tpreset="),
        "a switch with no preset has no preset line"
    );
    assert!(
        !snapshot.contains("wait_seconds\tvalue="),
        "a whole number is held by its bounds, not by a list of values"
    );
}

#[test]
fn the_register_names_a_whole_numbers_bounds_in_its_unit() {
    let markdown = render_markdown_of(&[seconds(), switch()]);
    assert!(markdown.contains(
        "| `example.wait_seconds` | a whole number of seconds, `0` to `3600` | `0` | `120` |"
    ));
    assert!(
        markdown.contains("| `example.print_on_settle` | `true`, `false` | `true` | the default |")
    );
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

/// The facts every setting must hold, whatever its shape: a default it takes, a preset it takes
/// that is not the default, a scope, a release and a sentence.
fn assert_well_formed(setting: &Setting) {
    let key = setting.key();
    assert_eq!(
        setting.check(&setting.default_value()),
        Ok(()),
        "{key}: the default is not a value it takes"
    );
    if let Some(preset) = setting.preset_value() {
        assert_eq!(
            setting.check(&preset),
            Ok(()),
            "{key}: the preset is not a value it takes"
        );
        assert_ne!(
            preset,
            setting.default_value(),
            "{key}: a preset equal to the default gives a new store nothing"
        );
    }
    match &setting.shape {
        SettingShape::Choice { values, .. } => {
            assert!(!values.is_empty(), "{key} takes no value");
        }
        SettingShape::Int { min, max, unit, .. } => {
            assert!(min <= max, "{key}: its bounds are the wrong way round");
            assert_ne!(*unit, SettingUnit::Unspecified, "{key} counts nothing");
        }
        SettingShape::Bool { .. } => {}
    }
    assert!(!setting.scopes.is_empty(), "{key} can be set nowhere");
    assert!(
        !setting.scopes.contains(&SettingScope::Unspecified),
        "{key} names an unspecified scope"
    );
    if let Some(confirm) = setting.confirm {
        let value = json!(confirm.value);
        assert_eq!(
            setting.check(&value),
            Ok(()),
            "{key}: the value it confirms is not a value it takes"
        );
        // ADR-0169 decision 1: a store runs a confirmed value only after somebody confirmed
        // it, so neither what a store runs unset nor what a new store is given may be one.
        assert_ne!(
            value,
            setting.default_value(),
            "{key}: the default needs confirming"
        );
        assert_ne!(
            Some(value),
            setting.preset_value(),
            "{key}: a new store would be given a value nobody confirmed"
        );
        assert_eq!(
            confirm.notice,
            format!("settings.item.{key}.confirm"),
            "{key}: the notice is not under the setting's own translation key"
        );
    }
    assert!(!setting.since.is_empty(), "{key} names no release");
    assert!(
        !setting.summary.is_empty(),
        "{key} says nothing about itself"
    );
}

#[test]
fn every_setting_has_a_default_and_a_preset_it_takes_a_scope_and_a_release() {
    for setting in register() {
        assert_well_formed(&setting);
    }
    // And the two built here hold to the same rules.
    assert_well_formed(&seconds());
    assert_well_formed(&switch());
    assert_well_formed(&confirmed_choice());
}

#[test]
#[should_panic(expected = "a new store would be given a value nobody confirmed")]
fn a_preset_that_needs_confirming_is_not_well_formed() {
    let mut setting = confirmed_choice();
    setting.shape = SettingShape::Choice {
        values: vec!["OWNER_SHARED", "OWNER_ONE_PERSON"],
        default: "OWNER_SHARED",
        preset: Some("OWNER_ONE_PERSON"),
    };
    assert_well_formed(&setting);
}

#[test]
#[should_panic(expected = "the default needs confirming")]
fn a_default_that_needs_confirming_is_not_well_formed() {
    let mut setting = confirmed_choice();
    setting.shape = SettingShape::Choice {
        values: vec!["OWNER_SHARED", "OWNER_ONE_PERSON"],
        default: "OWNER_ONE_PERSON",
        preset: None,
    };
    assert_well_formed(&setting);
}

#[test]
fn only_the_value_a_setting_confirms_needs_confirming() {
    let setting = confirmed_choice();
    assert!(setting.needs_confirming(&json!("OWNER_ONE_PERSON")));
    assert!(!setting.needs_confirming(&json!("OWNER_SHARED")));
    assert!(!setting.needs_confirming(&json!(1)));
    assert!(!setting.needs_confirming(&Value::Null));
    assert!(!seconds().needs_confirming(&json!(120)));
    assert!(!switch().needs_confirming(&json!(false)));
}

#[test]
fn a_confirmed_value_is_a_line_of_the_snapshot_and_marked_in_the_register() {
    let snapshot = render_snapshot_of(&[confirmed_choice()]);
    assert!(
        snapshot
            .lines()
            .any(|line| line == "example.drawer_owner\tconfirm=OWNER_ONE_PERSON"),
        "{snapshot}"
    );
    assert!(!render_snapshot_of(&[switch()]).contains("\tconfirm="));

    let markdown = render_markdown_of(&[confirmed_choice()]);
    assert!(markdown.contains(
        "| `example.drawer_owner` | `OWNER_SHARED`, `OWNER_ONE_PERSON`; `OWNER_ONE_PERSON` is \
         confirmed | `OWNER_SHARED` |"
    ));
    assert!(markdown.contains("A value marked **confirmed**"));
    assert!(
        !render_markdown_of(&[switch()]).contains("**confirmed**"),
        "the page explains a confirmed value only where the register has one"
    );
}

/// The values a test reads back through a setting's node: every token of a choice; a whole
/// number's bounds, default and preset; both values of a switch.
fn samples(setting: &Setting) -> Vec<Value> {
    match &setting.shape {
        SettingShape::Choice { values, .. } => values.iter().map(|value| json!(value)).collect(),
        SettingShape::Int {
            min,
            max,
            default,
            preset,
            ..
        } => [Some(*min), Some(*max), Some(*default), *preset]
            .into_iter()
            .flatten()
            .map(|value| json!(value))
            .collect(),
        SettingShape::Bool { .. } => vec![json!(false), json!(true)],
    }
}

/// Reads one field of a node back through the node's own type, as the edge reads it — from a
/// node carrying `value`, or from an empty node when `value` is `None`.
///
/// A setting on a node this function has no arm for fails the tests below, so a new node cannot
/// join the register without the reader that proves the register right about it.
fn read_back(node: &str, field: &str, value: Option<&Value>) -> Option<Value> {
    let document = value.map_or_else(|| json!({}), |value| json!({ field: value }));
    match node {
        PublishedShift::NODE => {
            let shift: PublishedShift = serde_json::from_value(document).ok()?;
            match field {
                "no_shift_selling" => Some(json!(shift.no_shift_selling().as_wire())),
                "opening_float_minor" => Some(json!(shift.opening_float_minor())),
                "blind_close" => Some(json!(shift.blind_close())),
                "drawer_model" => Some(json!(shift.drawer_model().as_wire())),
                "close_report" => Some(json!(shift.close_report().as_wire())),
                "drawer_day_end" => Some(json!(shift.drawer_day_end().as_wire())),
                "variance_reason_minor" => Some(json!(shift.variance_reason_minor())),
                "drawer_access" => Some(json!(shift.drawer_access().as_wire())),
                _ => None,
            }
        }
        PublishedSession::NODE => {
            let session: PublishedSession = serde_json::from_value(document).ok()?;
            match field {
                "idle_lock_seconds" => Some(json!(session.idle_lock_seconds())),
                // A node that sets no window leaves the edge on its deprecated local file and
                // then on this default, so the default is what a store with neither runs.
                "sign_in_idle_timeout_minutes" => Some(json!(
                    session
                        .sign_in_idle_timeout_minutes()
                        .unwrap_or(DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES)
                )),
                "lockout_attempts" => Some(json!(session.lockout_attempts())),
                "lockout_minutes" => Some(json!(session.lockout_minutes())),
                "pin_min_length" => Some(json!(session.pin_min_length())),
                _ => None,
            }
        }
        PublishedPermissions::NODE => {
            let permissions: PublishedPermissions = serde_json::from_value(document).ok()?;
            match field {
                "enforced" => Some(json!(permissions.enforced)),
                _ => None,
            }
        }
        PublishedPrinting::NODE => {
            let printing: PublishedPrinting = serde_json::from_value(document).ok()?;
            match field {
                "receipt_language" => Some(json!(printing.receipt_language().as_wire())),
                "receipt_second_language" => {
                    Some(json!(printing.receipt_second_language().as_wire()))
                }
                "receipt_printed_on_settle" => Some(json!(printing.receipt_printed_on_settle())),
                "receipt_tax_components" => Some(json!(printing.receipt_tax_components())),
                // A node that sets no size leaves the edge on its deprecated local file and
                // then on this default, so the default is what a store with neither runs.
                "font_size_dots" => Some(json!(
                    printing.font_size_dots().unwrap_or(DEFAULT_FONT_SIZE_DOTS)
                )),
                _ => None,
            }
        }
        PublishedQr::NODE => {
            let qr: PublishedQr = serde_json::from_value(document).ok()?;
            match field {
                "table_order" => Some(json!(qr.table_order().as_wire())),
                _ => None,
            }
        }
        LocaleSettings::NODE => {
            let locale: LocaleSettings = serde_json::from_value(document).ok()?;
            match field {
                "tax_rounding" => Some(json!(locale.tax_rounding().as_wire())),
                _ => None,
            }
        }
        PublishedTenderKeys::NODE => {
            let keys: PublishedTenderKeys = serde_json::from_value(document).ok()?;
            match field {
                "first_tip_percent" => Some(json!(keys.first_tip_percent())),
                "second_tip_percent" => Some(json!(keys.second_tip_percent())),
                "third_tip_percent" => Some(json!(keys.third_tip_percent())),
                "split_ways_max" => Some(json!(keys.split_ways_max())),
                _ => None,
            }
        }
        PublishedCounter::NODE => {
            let counter: PublishedCounter = serde_json::from_value(document).ok()?;
            match field {
                "walk_in_channel" => Some(json!(counter.walk_in_channel().as_wire())),
                _ => None,
            }
        }
        PublishedBackup::NODE => {
            let backup: PublishedBackup = serde_json::from_value(document).ok()?;
            match field {
                // A node that sets no interval leaves the edge on its deprecated local file and
                // then on this default, so the default is what a store with neither runs.
                "interval_hours" => Some(json!(
                    backup.interval_hours().unwrap_or(DEFAULT_INTERVAL_HOURS)
                )),
                _ => None,
            }
        }
        _ => None,
    }
}

#[test]
fn every_value_reads_back_through_its_node() {
    for setting in register() {
        for value in samples(&setting) {
            assert_eq!(
                read_back(setting.node, setting.field, Some(&value)),
                Some(value.clone()),
                "{}: the edge does not read `{value}` as the register says",
                setting.key()
            );
        }
    }
}

#[test]
fn the_default_is_what_an_empty_node_reads_as() {
    for setting in register() {
        assert_eq!(
            read_back(setting.node, setting.field, None),
            Some(setting.default_value()),
            "{}: no reader for its node, or an empty node does not read as the default",
            setting.key()
        );
    }
}
