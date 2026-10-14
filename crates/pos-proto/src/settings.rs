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

use crate::wire_enum;
use crate::wire_enum::WireEnum;

mod nodes;

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
    /// An amount of money in the store currency's minor unit: đồng for VND, cents for USD
    /// (`docs/naming-and-api.md` §3.2). A setting at the tenant may reach stores in more than one
    /// currency, so the number is the store's own currency's, whichever that is.
    MinorUnits = "MINOR_UNITS",
    /// A whole percentage: `10` is ten percent.
    Percent = "PERCENT",
    /// Hours.
    Hours = "HOURS",
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
    /// The value, if any, that is written only with a recorded confirmation of a notice
    /// ([ADR-0169](../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)).
    pub confirm: Option<Confirm>,
    /// What the setting decides, in one sentence.
    pub summary: &'static str,
}

/// A value of a setting that the cloud writes only when the person writing it confirms a notice,
/// and records that they did
/// ([ADR-0169](../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)
/// decision 1).
///
/// For a value that records something about a person, which data protection law lets a tenant do
/// only on a lawful basis it has told its staff about. The register says which value; the notice's
/// wording is its translation's, so nothing here names a country.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Confirm {
    /// The wire token that needs confirming. Never the setting's default or its preset, so a store
    /// runs it only after somebody confirmed it.
    pub value: &'static str,
    /// The translation key of the sentence the person confirms,
    /// `settings.item.<node>.<field>.confirm`.
    pub notice: &'static str,
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

    /// Whether writing `value` needs a recorded confirmation: whether it is the value the setting's
    /// [`Confirm`] names. Any other value, and a setting with no `confirm`, needs none.
    #[must_use]
    pub fn needs_confirming(&self, value: &serde_json::Value) -> bool {
        self.confirm
            .is_some_and(|confirm| value.as_str() == Some(confirm.value))
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

/// The scope of a setting about a tenant's people rather than about a store: the tenant alone. A
/// person may work at every store of the tenant, so a value for one store could not be applied to
/// what is theirs.
const TENANT_ONLY: &[SettingScope] = &[SettingScope::Tenant];

/// The first release after 0.14.0. Tags are cut from `main`, so the next release carries everything
/// merged before it, whatever number it is given.
const NEXT_RELEASE: &str = "0.14.1";

/// The `since` of a setting the cloud applies itself rather than a store's edge: every release
/// honours it, because the release a store runs does not decide it (ADR-0160 decision 5).
const EVERY_RELEASE: &str = "0.0.0";

/// Every setting, in the order the register lists them: each node's settings in turn, in the
/// order `docs/configuration.md` prints them.
#[must_use]
pub fn register() -> Vec<Setting> {
    [
        nodes::shift_settings(),
        nodes::drawer_settings(),
        nodes::session_settings(),
        nodes::permission_settings(),
        nodes::printing_settings(),
        nodes::qr_settings(),
        nodes::locale_settings(),
        nodes::tender_key_settings(),
        nodes::counter_settings(),
        nodes::backup_settings(),
    ]
    .into_iter()
    .flatten()
    .collect()
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
/// `min=`, `max=` and `unit=` lines may change. A `confirm=` line is a contract too: dropping a
/// confirmation takes an ADR (ADR-0169).
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
        if let Some(confirm) = setting.confirm {
            lines.push(format!("{key}\tconfirm={}", confirm.value));
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
         # since, and a whole number's min, max and unit lines may change. ADR-0160 decision 9.\n\
         # A REMOVED confirm line fails CI too: it is a value written only with a recorded\n\
         # confirmation, and dropping that takes an ADR. ADR-0169.\n",
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
         earlier release ignores it.\n",
    );
    // Said only once a setting has such a value, so the page describes the register it prints.
    if settings.iter().any(|setting| setting.confirm.is_some()) {
        out.push_str(
            "- A value marked **confirmed** is written only when the person writing it confirms \
             a notice, and the audit trail records who confirmed it and when \
             ([ADR-0169](adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)).\n",
        );
    }
    out.push('\n');
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
        let values = match setting.confirm {
            Some(confirm) => format!("{values}; `{}` is confirmed", confirm.value),
            None => values,
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
        SettingUnit::MinorUnits => "an amount in the store currency's minor unit",
        SettingUnit::Percent => "a whole percentage",
        SettingUnit::Hours => "a whole number of hours",
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
mod tests;
