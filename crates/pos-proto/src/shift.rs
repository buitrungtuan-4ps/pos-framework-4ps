// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `shift` config node: how a store runs its shifts
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 2).
//!
//! Every field is a setting in [`crate::settings`], and every default is what the edge did before the
//! field existed. So a document with no `shift` node, or a node without a field, runs a store exactly
//! as it ran before, which is what lets an upgrade change nothing until someone sets a value.
//!
//! The drawer fields, `drawer_model`, `close_report` and `drawer_day_end`
//! ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md)), are typed here ahead of
//! the edge that honours them, and each joins the register with the release that does. Until then
//! nothing writes them and nothing reads them.

use core::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::wire_enum;
use crate::wire_enum::{Open, is_absent};

/// The opening floats a store may set, in the store currency's minor unit, both bounds included.
/// `0` fills in nothing. A billion at the most: ten million in a currency with two decimals, and far
/// beyond any drawer's float in one with none, so a stray digit is refused rather than filled in.
pub const OPENING_FLOAT_MINOR: RangeInclusive<i64> = 0..=1_000_000_000;

/// The opening float when nothing sets one: `0`, which fills in nothing, as the Shift screen did
/// before the setting.
pub const DEFAULT_OPENING_FLOAT_MINOR: i64 = 0;

wire_enum! {
    /// Whether a store sells while no shift is open.
    NoShiftSelling, prefix = "NO_SHIFT_SELLING";
    /// Sell with or without an open shift: the behaviour before the setting existed. A sale made
    /// while no shift is open belongs to no shift, and its cash is in no shift's expected drawer.
    Allow = "ALLOW",
    /// Refuse to seat a table, start a counter order or take a payment until a shift is open, so
    /// that every sale and every payment belongs to a shift.
    Refuse = "REFUSE",
}

wire_enum! {
    /// How many cash drawers a store keeps
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 1).
    DrawerModel, prefix = "DRAWER_MODEL";
    /// One drawer, the store's, which every till's cash goes into: the behaviour before the field.
    PerStore = "PER_STORE",
    /// A drawer per till, each started, counted and closed on its own (decision 2).
    PerTerminal = "PER_TERMINAL",
}

wire_enum! {
    /// What prints when a drawer closes (ADR-0167 decision 10).
    CloseReport, prefix = "CLOSE_REPORT";
    /// Each drawer's report prints where it is closed, as the one shift report does today.
    PerDrawer = "PER_DRAWER",
    /// Closing several drawers prints one slip: the store's totals, then each drawer's.
    Combined = "COMBINED",
    /// Nothing prints, and the console has the figures.
    None = "NONE",
}

wire_enum! {
    /// What a drawer still open past its business day's cutoff does (ADR-0167 decision 11).
    DrawerDayEnd, prefix = "DRAWER_DAY_END";
    /// It is flagged on the till and in the console, and keeps working.
    Flag = "FLAG",
    /// It takes no more cash until it is counted and closed; other methods still work.
    RequireClose = "REQUIRE_CLOSE",
}

/// The `shift` node.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a node
/// that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedShift {
    /// Whether the store sells while no shift is open. Read it through
    /// [`PublishedShift::no_shift_selling`], which gives an absent or unknown value its default.
    #[serde(default)]
    pub no_shift_selling: Open<NoShiftSelling>,
    /// The float the Shift screen fills in when a shift opens, in the store currency's minor unit.
    /// Read it through [`PublishedShift::opening_float_minor`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_float_minor: Option<i64>,
    /// Whether the count at the close is blind. Read it through [`PublishedShift::blind_close`]:
    /// absent is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blind_close: Option<bool>,
    /// How many drawers the store keeps. Read it through [`PublishedShift::drawer_model`]. Left
    /// off the wire while absent, as the drawer fields below are, so a node that sets none is
    /// written as before.
    #[serde(default, skip_serializing_if = "is_absent")]
    pub drawer_model: Open<DrawerModel>,
    /// What prints when a drawer closes. Read it through [`PublishedShift::close_report`].
    #[serde(default, skip_serializing_if = "is_absent")]
    pub close_report: Open<CloseReport>,
    /// What a drawer open past its day's cutoff does. Read it through
    /// [`PublishedShift::drawer_day_end`].
    #[serde(default, skip_serializing_if = "is_absent")]
    pub drawer_day_end: Open<DrawerDayEnd>,
}

impl PublishedShift {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "shift";

    /// Whether the store sells while no shift is open.
    ///
    /// An absent value, `NO_SHIFT_SELLING_UNSPECIFIED`, and a value this release does not know all
    /// read as [`NoShiftSelling::Allow`], the default. A value from a newer release is never offered
    /// to a store that cannot honour it (ADR-0160 decision 5), so the default is what such a store
    /// was running anyway.
    #[must_use]
    pub fn no_shift_selling(&self) -> NoShiftSelling {
        match self.no_shift_selling.known() {
            NoShiftSelling::Unspecified | NoShiftSelling::Allow => NoShiftSelling::Allow,
            NoShiftSelling::Refuse => NoShiftSelling::Refuse,
        }
    }

    /// The float the Shift screen fills in when a shift opens, in the store currency's minor unit:
    /// the node's number when it is within [`OPENING_FLOAT_MINOR`], and
    /// [`DEFAULT_OPENING_FLOAT_MINOR`], which fills in nothing, otherwise.
    ///
    /// A starting point and not a rule: the cashier opens the shift with whatever they count into
    /// the drawer, and the edge records the float they send.
    #[must_use]
    pub fn opening_float_minor(&self) -> i64 {
        self.opening_float_minor
            .filter(|minor| OPENING_FLOAT_MINOR.contains(minor))
            .unwrap_or(DEFAULT_OPENING_FLOAT_MINOR)
    }

    /// Whether the count at the close is blind: the edge says nothing about what the drawer should
    /// hold until the shift closes (`docs/pos-spec.md` §11 item 1). `true` unless the store says
    /// otherwise, because every count was blind before the setting existed.
    #[must_use]
    pub fn blind_close(&self) -> bool {
        self.blind_close.unwrap_or(true)
    }

    /// How many drawers the store keeps. An absent, unspecified or unknown value reads as
    /// [`DrawerModel::PerStore`], the one drawer every store kept before the field, read as
    /// [`Self::no_shift_selling`] reads its own.
    #[must_use]
    pub fn drawer_model(&self) -> DrawerModel {
        match self.drawer_model.known() {
            DrawerModel::Unspecified | DrawerModel::PerStore => DrawerModel::PerStore,
            DrawerModel::PerTerminal => DrawerModel::PerTerminal,
        }
    }

    /// What prints when a drawer closes: [`CloseReport::PerDrawer`], the one report printed where
    /// it closes, unless the node says otherwise.
    #[must_use]
    pub fn close_report(&self) -> CloseReport {
        match self.close_report.known() {
            CloseReport::Unspecified | CloseReport::PerDrawer => CloseReport::PerDrawer,
            CloseReport::Combined => CloseReport::Combined,
            CloseReport::None => CloseReport::None,
        }
    }

    /// What a drawer open past its day's cutoff does: [`DrawerDayEnd::Flag`], which refuses
    /// nothing, unless the node says otherwise.
    #[must_use]
    pub fn drawer_day_end(&self) -> DrawerDayEnd {
        match self.drawer_day_end.known() {
            DrawerDayEnd::Unspecified | DrawerDayEnd::Flag => DrawerDayEnd::Flag,
            DrawerDayEnd::RequireClose => DrawerDayEnd::RequireClose,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CloseReport, DrawerDayEnd, DrawerModel, NoShiftSelling, PublishedShift};
    use crate::wire_enum::{Open, WireEnum};

    #[test]
    fn an_absent_node_field_sells_as_before() {
        let node: PublishedShift = serde_json::from_str("{}").expect("an empty node parses");
        assert_eq!(node.no_shift_selling(), NoShiftSelling::Allow);
        assert_eq!(
            PublishedShift::default().no_shift_selling(),
            NoShiftSelling::Allow
        );
    }

    #[test]
    fn an_absent_node_fills_in_no_float_and_counts_blind() {
        let node: PublishedShift = serde_json::from_str("{}").expect("an empty node parses");
        assert_eq!(node.opening_float_minor(), 0);
        assert!(node.blind_close());
        assert_eq!(node, PublishedShift::default());
    }

    #[test]
    fn a_float_within_its_bounds_is_read_and_one_outside_them_fills_in_nothing() {
        for (published, read) in [
            ("0", 0),
            ("500000", 500_000),
            ("1000000000", 1_000_000_000),
            ("1000000001", 0),
            ("-1", 0),
        ] {
            let node: PublishedShift =
                serde_json::from_str(&format!(r#"{{ "opening_float_minor": {published} }}"#))
                    .expect("the node parses");
            assert_eq!(node.opening_float_minor(), read, "{published}");
        }
    }

    #[test]
    fn a_store_that_turns_the_blind_close_off_is_read() {
        let node: PublishedShift =
            serde_json::from_str(r#"{ "blind_close": false }"#).expect("the node parses");
        assert!(!node.blind_close());
        let on: PublishedShift =
            serde_json::from_str(r#"{ "blind_close": true }"#).expect("the node parses");
        assert!(on.blind_close());
    }

    #[test]
    fn a_published_refusal_is_read() {
        let node: PublishedShift =
            serde_json::from_str(r#"{ "no_shift_selling": "NO_SHIFT_SELLING_REFUSE" }"#)
                .expect("the node parses");
        assert_eq!(node.no_shift_selling(), NoShiftSelling::Refuse);
    }

    #[test]
    fn a_value_from_a_newer_release_reads_as_the_default() {
        let node: PublishedShift =
            serde_json::from_str(r#"{ "no_shift_selling": "NO_SHIFT_SELLING_LATER" }"#)
                .expect("an unknown token still parses");
        assert!(node.no_shift_selling.is_unrecognised());
        assert_eq!(node.no_shift_selling(), NoShiftSelling::Allow);
    }

    #[test]
    fn a_field_this_release_does_not_know_is_ignored() {
        let node: PublishedShift = serde_json::from_str(
            r#"{ "no_shift_selling": "NO_SHIFT_SELLING_REFUSE", "a_later_field": 500000 }"#,
        )
        .expect("an unknown field does not refuse the node");
        assert_eq!(node.no_shift_selling(), NoShiftSelling::Refuse);
    }

    #[test]
    fn the_node_round_trips() {
        let node = PublishedShift {
            no_shift_selling: Open::from_known(NoShiftSelling::Refuse),
            ..PublishedShift::default()
        };
        let text = serde_json::to_string(&node).expect("serialise");
        assert_eq!(
            text, r#"{"no_shift_selling":"NO_SHIFT_SELLING_REFUSE"}"#,
            "a float and a blind close nobody set are left off the wire"
        );
        let back: PublishedShift = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, node);
        assert_eq!(NoShiftSelling::Refuse.as_wire(), "NO_SHIFT_SELLING_REFUSE");

        let set = PublishedShift {
            opening_float_minor: Some(500_000),
            blind_close: Some(false),
            ..node
        };
        let text = serde_json::to_string(&set).expect("serialise");
        assert_eq!(
            text,
            r#"{"no_shift_selling":"NO_SHIFT_SELLING_REFUSE","opening_float_minor":500000,"blind_close":false}"#
        );
        let back: PublishedShift = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, set);
    }

    #[test]
    fn an_absent_drawer_field_reads_as_the_one_drawer_every_store_kept() {
        // ADR-0167: typed ahead of the edge that honours them, and read as today until then.
        let node: PublishedShift = serde_json::from_str("{}").expect("an empty node parses");
        assert_eq!(node.drawer_model(), DrawerModel::PerStore);
        assert_eq!(node.close_report(), CloseReport::PerDrawer);
        assert_eq!(node.drawer_day_end(), DrawerDayEnd::Flag);

        let later: PublishedShift = serde_json::from_str(
            r#"{ "drawer_model": "DRAWER_MODEL_LATER", "close_report": "CLOSE_REPORT_LATER",
                 "drawer_day_end": "DRAWER_DAY_END_LATER" }"#,
        )
        .expect("unknown tokens still parse");
        assert_eq!(later.drawer_model(), DrawerModel::PerStore);
        assert_eq!(later.close_report(), CloseReport::PerDrawer);
        assert_eq!(later.drawer_day_end(), DrawerDayEnd::Flag);
    }

    #[test]
    fn every_drawer_value_is_read_and_round_trips() {
        for (model, report, day_end) in [
            (
                DrawerModel::PerTerminal,
                CloseReport::Combined,
                DrawerDayEnd::RequireClose,
            ),
            (DrawerModel::PerStore, CloseReport::None, DrawerDayEnd::Flag),
            (
                DrawerModel::PerStore,
                CloseReport::PerDrawer,
                DrawerDayEnd::Flag,
            ),
        ] {
            let text = format!(
                r#"{{"drawer_model":"{}","close_report":"{}","drawer_day_end":"{}"}}"#,
                model.as_wire(),
                report.as_wire(),
                day_end.as_wire()
            );
            let node: PublishedShift = serde_json::from_str(&text).expect("the node parses");
            assert_eq!(
                (
                    node.drawer_model(),
                    node.close_report(),
                    node.drawer_day_end()
                ),
                (model, report, day_end)
            );
            let fields = text.strip_prefix('{').expect("an object");
            assert_eq!(
                serde_json::to_string(&node).expect("serialise"),
                format!(r#"{{"no_shift_selling":"NO_SHIFT_SELLING_UNSPECIFIED",{fields}"#),
                "each set value is written, after the fields before it"
            );
        }
        assert_eq!(
            DrawerModel::PerTerminal.as_wire(),
            "DRAWER_MODEL_PER_TERMINAL"
        );
        assert_eq!(CloseReport::None.as_wire(), "CLOSE_REPORT_NONE");
        assert_eq!(
            DrawerDayEnd::RequireClose.as_wire(),
            "DRAWER_DAY_END_REQUIRE_CLOSE"
        );
    }
}
