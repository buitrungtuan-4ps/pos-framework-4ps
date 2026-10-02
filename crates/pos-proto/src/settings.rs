// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The register of settings: every value a store may run differently
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 9).
//!
//! A setting is a typed field on a published node, and its default is what the edge did before the
//! field existed. [`register`] names each setting once: its node and field, the shape of its value
//! ([`SettingShape`] — one of a list of tokens, a whole number between two bounds, or on and off),
//! its default, where it may be written, and the first release that honours it. Two committed files
//! are rendered from that list:
//!
//! - `docs/snapshots/settings.txt`, one fact per line. `cargo xtask snapshot` diffs it against the
//!   base branch, so a setting, one of its values or one of its scopes cannot disappear unseen.
//! - `docs/configuration.md`, the register an operator reads.
//!
//! [`Setting::check`] is the one rule for whether a value is one a setting takes. The cloud applies
//! it when a value is written and again when it resolves what reaches a store, so a value outside a
//! setting's bounds is neither stored nor sent to a store from the console.
//!
//! A test reads every value of every setting back through its node's own type, so the register
//! cannot offer a value or claim a default that the edge does not have.

use crate::people::PublishedPermissions;
use crate::printing::{PublishedPrinting, ReceiptLanguage, ReceiptSecondLanguage};
use crate::qr::{PublishedQr, TableOrder};
use crate::session::{self, PublishedSession};
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
    /// A whole number between two bounds, counting a [`SettingUnit`].
    Int = "INT",
    /// On or off: `true` or `false`.
    Bool = "BOOL",
}

wire_enum! {
    /// What a whole-number setting counts, so the console can name it in the operator's language.
    ///
    /// A label, not a conversion: what the number means is fixed by the field and by the edge's
    /// reader of it, and the unit only says how to name it.
    SettingUnit, prefix = "SETTING_UNIT";
    /// Seconds.
    Seconds = "SECONDS",
    /// Minutes.
    Minutes = "MINUTES",
    /// A number of times or of things, which the setting's own name says — attempts, copies.
    Count = "COUNT",
}

/// What a setting's value is, and what the register holds it to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingShape {
    /// One value from a fixed list, each value a wire token.
    Choice {
        /// Every value the setting takes.
        values: Vec<&'static str>,
        /// The value a store runs when nothing sets one.
        default: &'static str,
        /// The value a new store is given, when the owner chose one other than the default.
        preset: Option<&'static str>,
    },
    /// A whole number from `min` to `max`, both included.
    Int {
        /// The smallest value it takes.
        min: i64,
        /// The largest value it takes.
        max: i64,
        /// What the number counts.
        unit: SettingUnit,
        /// The value a store runs when nothing sets one.
        default: i64,
        /// The value a new store is given, when the owner chose one other than the default.
        preset: Option<i64>,
    },
    /// On or off.
    Bool {
        /// The value a store runs when nothing sets one.
        default: bool,
        /// The value a new store is given, when the owner chose one other than the default.
        preset: Option<bool>,
    },
}

/// Why a value is not one a setting takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValueRefusal {
    /// A choice's value is not one of its tokens.
    #[error("it is not one of the setting's values")]
    NotOneOfItsValues,
    /// A whole-number setting's value is not a whole number.
    #[error("it is not a whole number")]
    NotAWholeNumber,
    /// A whole-number setting's value is outside its bounds.
    #[error("it is outside the setting's bounds")]
    OutOfRange,
    /// An on-or-off setting's value is not `true` or `false`.
    #[error("it is not true or false")]
    NotTrueOrFalse,
}

/// One setting: a field on a published node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    /// The published node the value travels in, such as `shift`.
    pub node: &'static str,
    /// The field on that node, such as `no_shift_selling`.
    pub field: &'static str,
    /// What the value is: the values it takes, the value a store runs when nothing sets one — which
    /// is what the edge did before the setting existed (ADR-0160 decision 1) — and the value a new
    /// store is given, which the owner chose.
    pub shape: SettingShape,
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

    /// The shape's kind, from which the console draws its form.
    #[must_use]
    pub const fn kind(&self) -> SettingKind {
        match self.shape {
            SettingShape::Choice { .. } => SettingKind::Choice,
            SettingShape::Int { .. } => SettingKind::Int,
            SettingShape::Bool { .. } => SettingKind::Bool,
        }
    }

    /// The value a store runs when nothing sets one, as the node carries it.
    #[must_use]
    pub fn default_value(&self) -> serde_json::Value {
        match &self.shape {
            SettingShape::Choice { default, .. } => serde_json::Value::from(*default),
            SettingShape::Int { default, .. } => serde_json::Value::from(*default),
            SettingShape::Bool { default, .. } => serde_json::Value::from(*default),
        }
    }

    /// The value a new store is given, as the node carries it, or `None` to leave a new store on
    /// the default.
    #[must_use]
    pub fn preset_value(&self) -> Option<serde_json::Value> {
        match &self.shape {
            SettingShape::Choice { preset, .. } => preset.map(serde_json::Value::from),
            SettingShape::Int { preset, .. } => preset.map(serde_json::Value::from),
            SettingShape::Bool { preset, .. } => preset.map(serde_json::Value::from),
        }
    }

    /// Whether `value` is one the setting takes.
    ///
    /// A choice takes one of its tokens, as a string. A whole number takes a JSON integer from its
    /// minimum to its maximum, both included — not `120.0`, not `"120"`. A switch takes `true` or
    /// `false`, not `"true"` or `1`. Exact on purpose: the node's reader at the edge reads the same
    /// JSON type, and a value it would read as something else is refused here rather than there.
    ///
    /// # Errors
    ///
    /// [`ValueRefusal`] naming what is wrong with the value.
    pub fn check(&self, value: &serde_json::Value) -> Result<(), ValueRefusal> {
        match &self.shape {
            SettingShape::Choice { values, .. } => value
                .as_str()
                .is_some_and(|token| values.contains(&token))
                .then_some(())
                .ok_or(ValueRefusal::NotOneOfItsValues),
            SettingShape::Int { min, max, .. } => {
                let number = value.as_i64().ok_or(ValueRefusal::NotAWholeNumber)?;
                (*min..=*max)
                    .contains(&number)
                    .then_some(())
                    .ok_or(ValueRefusal::OutOfRange)
            }
            SettingShape::Bool { .. } => value
                .is_boolean()
                .then_some(())
                .ok_or(ValueRefusal::NotTrueOrFalse),
        }
    }

    /// What the setting takes, for the sentence a refusal says: its tokens, its bounds, or `true or
    /// false`.
    #[must_use]
    pub fn takes(&self) -> String {
        match &self.shape {
            SettingShape::Choice { values, .. } => values.join(", "),
            SettingShape::Int { min, max, .. } => format!("a whole number from {min} to {max}"),
            SettingShape::Bool { .. } => "true or false".to_owned(),
        }
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
#[expect(
    clippy::too_many_lines,
    reason = "one flat list in the order `docs/configuration.md` prints it, which grows by a \
              self-contained entry per setting; splitting it across helpers would only scatter it"
)]
pub fn register() -> Vec<Setting> {
    // What the edge reads a `session` node that sets nothing as.
    let unset = PublishedSession::default();
    vec![
        Setting {
            node: PublishedShift::NODE,
            field: "no_shift_selling",
            shape: SettingShape::Choice {
                values: choices::<NoShiftSelling>(),
                default: PublishedShift::default().no_shift_selling().as_wire(),
                // Confirmed by the owner on 2026-09-30: a new store refuses, an existing one keeps
                // selling.
                preset: Some(NoShiftSelling::Refuse.as_wire()),
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "Whether a till may seat a table, start a counter order or take a payment \
                      while no shift is open. `NO_SHIFT_SELLING_REFUSE` refuses each of them with \
                      `OPEN_SHIFT_REQUIRED` until a shift opens.",
        },
        Setting {
            node: PublishedSession::NODE,
            field: "idle_lock_seconds",
            shape: SettingShape::Int {
                min: *session::IDLE_LOCK_SECONDS.start(),
                max: *session::IDLE_LOCK_SECONDS.end(),
                unit: SettingUnit::Seconds,
                default: i64::from(unset.idle_lock_seconds()),
                // Confirmed by the owner on 2026-10-01: a new store's till locks after two minutes
                // without a touch. An existing store keeps a till that never locks.
                preset: Some(120),
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "A security setting: how long a till may sit with no touch, key, click or \
                      scroll before it signs its person out and locks, until their PIN opens it \
                      again on the screen they left. `0` never locks. The kitchen board, the pass \
                      and the screens before sign-in never lock.",
        },
        Setting {
            node: PublishedSession::NODE,
            field: "sign_in_idle_timeout_minutes",
            shape: SettingShape::Int {
                min: *session::SIGN_IN_IDLE_TIMEOUT_MINUTES.start(),
                max: *session::SIGN_IN_IDLE_TIMEOUT_MINUTES.end(),
                unit: SettingUnit::Minutes,
                default: i64::from(session::DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "A security setting: how long a signed-in device may go unused before its \
                      sign-in lapses and it asks for a PIN again, which bounds how long a till \
                      carried off while signed in keeps trading. A store that does not set it runs \
                      `sign_in_idle_timeout_minutes` from its local file if that sets one, which \
                      is deprecated.",
        },
        Setting {
            node: PublishedSession::NODE,
            field: "lockout_attempts",
            shape: SettingShape::Int {
                min: *session::LOCKOUT_ATTEMPTS.start(),
                max: *session::LOCKOUT_ATTEMPTS.end(),
                unit: SettingUnit::Count,
                default: i64::from(unset.lockout_attempts()),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "A security setting: how many wrong PINs in a row lock a person out, \
                      counted across sign-in and a manager's approval. No value switches the \
                      lockout off.",
        },
        Setting {
            node: PublishedSession::NODE,
            field: "lockout_minutes",
            shape: SettingShape::Int {
                min: *session::LOCKOUT_MINUTES.start(),
                max: *session::LOCKOUT_MINUTES.end(),
                unit: SettingUnit::Minutes,
                default: i64::from(unset.lockout_minutes()),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "A security setting: how long a person stays locked out after too many wrong \
                      PINs. A lockout already running keeps the end it was given. No value \
                      switches the lockout off.",
        },
        Setting {
            node: PublishedPermissions::NODE,
            field: "enforced",
            shape: SettingShape::Bool {
                default: PublishedPermissions::default().enforced,
                // ADR-0158 Rollout, confirmed by the owner on 2026-10-01: on for a store created
                // after this lands, off for the stores that exist until someone turns it on.
                preset: Some(true),
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "A security setting: whether the till decides every act with the signed-in \
                      person's own permissions. What they hold directly they do alone, what they \
                      hold with approval needs the PIN of someone who holds it directly, and \
                      anything else is refused. Off, every act works as before. Set each role up \
                      before turning it on.",
        },
        Setting {
            node: PublishedPrinting::NODE,
            field: "receipt_language",
            shape: SettingShape::Choice {
                values: choices::<ReceiptLanguage>(),
                default: PublishedPrinting::default().receipt_language().as_wire(),
                // Confirmed by the owner on 2026-10-01: a new store prints in its country's language.
                // A token rather than a language, because the console gives a store these values
                // when it creates it, before anyone has said which country the store is in.
                preset: Some(ReceiptLanguage::Country.as_wire()),
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "The language a receipt, its copy and a pre-bill print their labels and item \
                      names in. `RECEIPT_LANGUAGE_DISPLAY` follows the store's display language. \
                      `RECEIPT_LANGUAGE_COUNTRY` is the language of the store's country, or the \
                      display language while the store's locale names none the edge prints labels \
                      in. An item the menu does not translate keeps its own name, and a box with \
                      no fonts prints English labels.",
        },
        Setting {
            node: PublishedPrinting::NODE,
            field: "receipt_second_language",
            shape: SettingShape::Choice {
                values: choices::<ReceiptSecondLanguage>(),
                default: PublishedPrinting::default()
                    .receipt_second_language()
                    .as_wire(),
                // The owner, 2026-10-01: a bilingual receipt is a choice each store makes, so a new
                // store is given none.
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "A second language a receipt, its copy and a pre-bill print in, after the \
                      first. Each label prints in both, as `Tạm tính / Subtotal`, or with the \
                      second under the first where the two do not fit on one line. An item, a \
                      modifier or a fee prints its name in the second language under its own \
                      where the menu or the fee's rule translates it. `RECEIPT_SECOND_LANGUAGE_NONE` \
                      prints one language, and so does a second language the receipt already \
                      prints in, or one a box with no fonts cannot print.",
        },
        Setting {
            node: PublishedPrinting::NODE,
            field: "receipt_printed_on_settle",
            shape: SettingShape::Bool {
                default: PublishedPrinting::default().receipt_printed_on_settle(),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "Whether settling a bill prints the guest's receipt. Off, a settle prints \
                      nothing, and the till's print button prints a copy.",
        },
        Setting {
            node: PublishedQr::NODE,
            field: "table_order",
            shape: SettingShape::Choice {
                values: choices::<TableOrder>(),
                default: PublishedQr::default().table_order().as_wire(),
                // Confirmed by the owner on 2026-10-01 (ADR-0160 item 2): a new store's guest orders
                // join the table's order. An existing store keeps a separate order until someone
                // sets it.
                preset: Some(TableOrder::Join.as_wire()),
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            summary: "What a guest's QR order does at a table that already has an open order. \
                      `TABLE_ORDER_JOIN` adds its lines to that order, so the table has one order \
                      and one bill; where staff confirm a guest's order, its lines join once they \
                      do. A line that joins keeps the price the guest was shown, and the table's \
                      order sets its tax and fees. `TABLE_ORDER_SEPARATE` gives the guest's order \
                      its own order, as before. A table whose bill is already open or paid takes a \
                      guest's order as its own order either way.",
        },
    ]
}

/// Every value of a wire enum but its `*_UNSPECIFIED`, which is never a choice.
fn choices<E: WireEnum>() -> Vec<&'static str> {
    E::ALL
        .iter()
        .filter(|value| **value != E::UNSPECIFIED)
        .map(|value| value.as_wire())
        .collect()
}

/// A scalar value as one fact of the snapshot or one cell of the register: a token bare, a number
/// or a boolean as JSON writes it.
fn scalar(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

/// Renders the register in the committed snapshot format, one fact per line.
///
/// Sorted, so the output depends on what the register holds and not on its order. A bare key, a
/// `kind=`, a `value=` and a `scope=` line are contracts: a value stored in the cloud and an edge on
/// an older release both rely on them. A `default=`, a `preset=`, a `since=`, and a whole number's
/// `min=`, `max=` and `unit=` lines may change.
#[must_use]
pub fn render_snapshot() -> String {
    render_snapshot_of(&register())
}

/// [`render_snapshot`] for any list of settings, so a test can render a shape the register does not
/// use yet.
fn render_snapshot_of(settings: &[Setting]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for setting in settings {
        let key = setting.key();
        lines.push(key.clone());
        lines.push(format!("{key}\tkind={}", setting.kind()));
        match &setting.shape {
            SettingShape::Choice { values, .. } => {
                for value in values {
                    lines.push(format!("{key}\tvalue={value}"));
                }
            }
            SettingShape::Int { min, max, unit, .. } => {
                lines.push(format!("{key}\tmin={min}"));
                lines.push(format!("{key}\tmax={max}"));
                lines.push(format!("{key}\tunit={unit}"));
            }
            SettingShape::Bool { .. } => {
                lines.push(format!("{key}\tvalue=false"));
                lines.push(format!("{key}\tvalue=true"));
            }
        }
        lines.push(format!(
            "{key}\tdefault={}",
            scalar(&setting.default_value())
        ));
        if let Some(preset) = setting.preset_value() {
            lines.push(format!("{key}\tpreset={}", scalar(&preset)));
        }
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
         # rely on it, so it may be added to but never renamed or removed. A default, a preset, a\n\
         # since, and a whole number's min, max and unit lines may change. ADR-0160 decision 9.\n",
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
    render_markdown_of(&register())
}

/// [`render_markdown`] for any list of settings, so a test can render a shape the register does not
/// use yet.
fn render_markdown_of(settings: &[Setting]) -> String {
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
         - **Values** is what a setting takes: one of a list of tokens, a whole number between \
         two bounds (both included), or `true` and `false`. The cloud refuses any other value \
         when it is written, and does not send a store one it would refuse.\n\
         - **Where it is set** lists the scopes a value may be written at, in the console's \
         settings or with `PUT /admin/settings`. A store runs the value of the most specific \
         scope that sets one: the store, then its store groups (the value written last, if two \
         disagree), then its brand, then the tenant (ADR-0160 decision 3).\n\
         - **New store** is the value the console gives a store it creates, where that differs \
         from the default.\n\
         - **Honoured from** is the first release that honours the setting. A store running an \
         earlier release ignores it.\n\n",
    );
    out.push_str(
        "| Setting | Values | Default | New store | Where it is set | Honoured from | What it decides |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|\n");
    for setting in settings {
        let values = match &setting.shape {
            SettingShape::Choice { values, .. } => values
                .iter()
                .map(|value| format!("`{value}`"))
                .collect::<Vec<_>>()
                .join(", "),
            SettingShape::Int { min, max, unit, .. } => {
                format!("{}, `{min}` to `{max}`", unit_phrase(*unit))
            }
            SettingShape::Bool { .. } => "`true`, `false`".to_owned(),
        };
        let scopes = setting
            .scopes
            .iter()
            .map(|scope| scope_label(*scope))
            .collect::<Vec<_>>()
            .join(", ");
        let cells = [
            format!("`{}`", setting.key()),
            values,
            format!("`{}`", scalar(&setting.default_value())),
            setting.preset_value().map_or_else(
                || "the default".to_owned(),
                |preset| format!("`{}`", scalar(&preset)),
            ),
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

/// A whole number's unit, as the register's reader calls the number.
fn unit_phrase(unit: SettingUnit) -> &'static str {
    match unit {
        SettingUnit::Unspecified => "a whole number",
        SettingUnit::Seconds => "a whole number of seconds",
        SettingUnit::Minutes => "a whole number of minutes",
        SettingUnit::Count => "a count",
    }
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

    use serde_json::{Value, json};

    use super::{
        DOC_PATH, SNAPSHOT_PATH, Setting, SettingKind, SettingScope, SettingShape, SettingUnit,
        ValueRefusal, register, render_markdown, render_markdown_of, render_snapshot,
        render_snapshot_of,
    };
    use crate::people::PublishedPermissions;
    use crate::printing::PublishedPrinting;
    use crate::qr::PublishedQr;
    use crate::session::{DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES, PublishedSession};
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
            summary: "How long to wait.",
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
            markdown
                .contains("| `example.print_on_settle` | `true`, `false` | `true` | the default |")
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
    }

    /// The values a test reads back through a setting's node: every token of a choice; a whole
    /// number's bounds, default and preset; both values of a switch.
    fn samples(setting: &Setting) -> Vec<Value> {
        match &setting.shape {
            SettingShape::Choice { values, .. } => {
                values.iter().map(|value| json!(value)).collect()
            }
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
                    "receipt_printed_on_settle" => {
                        Some(json!(printing.receipt_printed_on_settle()))
                    }
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
}
