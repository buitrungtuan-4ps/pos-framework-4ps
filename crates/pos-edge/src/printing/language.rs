// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What language paper prints in: the receipt's second language and the words in both, the language
//! a receipt or a ticket is printed in, and the lines and item names in it.

use std::borrow::Cow;

use pos_core::billing::BillTotals;
use pos_ports::printer::PrintBlock;
use pos_proto::floor::KitchenStation;
use pos_proto::ids::{MenuItemId, StationId};
use pos_proto::money::Money;
use pos_proto::printing::ReceiptLanguage;
use pos_proto::text::DisplayName;

use crate::app::{EdgeSession, ReceiptLine};
use crate::paper_labels::PaperLabels;
use crate::printing::blocks::{MoneyStyle, format_money, plain};
use crate::printing::devices::TillPrinting;
use crate::printing::documents::centred;

/// A receipt's second language
/// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 2): what a bilingual receipt, its copy and a pre-bill print after each of their labels,
/// and under each name the menu or a fee's rule translates into it.
///
/// Worked out from the session before a document is laid out, so the document stays a function of
/// its arguments. A document given none prints one language, exactly as before the setting existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondLanguage {
    /// Its labels.
    pub(super) labels: &'static PaperLabels,
    /// How many characters the printer fits on a line, which decides whether a label and its second
    /// language share one.
    pub(super) columns: usize,
    /// Each row's names in it, in the rows' order.
    pub(super) rows: Vec<SecondNames>,
    /// Each fee line's name in it, in the order of the bill's fee lines.
    pub(super) fees: Vec<Option<DisplayName>>,
}

/// One row's names in the second language: its item's, then each of its modifiers'. `None` where
/// the menu has no name in it, or the name is the one the row already prints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SecondNames {
    pub(super) item: Option<DisplayName>,
    pub(super) modifiers: Vec<Option<DisplayName>>,
}

impl SecondLanguage {
    /// The second language `session` prints `till`'s guest's paper in on a printer `columns` wide,
    /// or `None` for one language: none is set, the receipt's labels are in it already, or this box
    /// has no fonts to draw it with ([`PaperLabels::for_store`]).
    ///
    /// `captured` are the rows as the bill holds them and `printed` the same rows named in the
    /// receipt's language; `totals` names the fees as the receipt prints them.
    pub(super) fn for_receipt(
        session: &EdgeSession,
        till: &TillPrinting,
        first: &PaperLabels,
        can_rasterise: bool,
        columns: u16,
        captured: &[ReceiptLine],
        printed: &[ReceiptLine],
        totals: &BillTotals,
    ) -> Option<Self> {
        let tag = till.receipt_second_language(session).tag()?;
        let labels = PaperLabels::for_store(Some(tag), can_rasterise);
        if labels == first {
            return None;
        }
        // A row captures its names in the display language, so where that is the second language
        // the captured names are the ones to print; otherwise the menu's translations are.
        let captured_in_it = session
            .display_language
            .as_deref()
            .is_some_and(|display| same_language(display, tag));
        let name_in = |item: Option<MenuItemId>, captured: &DisplayName| {
            if captured_in_it {
                return Some(captured.clone());
            }
            let entry = session.menu_entry(item?)?;
            entry.display_name_translations.get(tag).cloned()
        };
        let unless_shown = |name: Option<DisplayName>, shown: Option<&DisplayName>| {
            name.filter(|name| Some(name) != shown)
        };
        let rows = captured
            .iter()
            .zip(printed)
            .map(|(row, shown)| SecondNames {
                item: unless_shown(
                    name_in(Some(row.menu_item_id), &row.display_name),
                    Some(&shown.display_name),
                ),
                modifiers: row
                    .modifier_display_names
                    .iter()
                    .enumerate()
                    .map(|(index, modifier)| {
                        unless_shown(
                            name_in(row.modifier_menu_item_ids.get(index).copied(), modifier),
                            shown.modifier_display_names.get(index),
                        )
                    })
                    .collect(),
            })
            .collect();
        let fees = totals
            .fee_lines
            .iter()
            .map(|fee| {
                unless_shown(
                    session.fee_translation(fee.fee_id, tag).cloned(),
                    Some(&fee.display_name),
                )
            })
            .collect();
        Some(Self {
            labels,
            columns: usize::from(columns),
            rows,
            fees,
        })
    }
}

/// Whether two language tags name one language, as `vi` and `vi-VN` do.
fn same_language(left: &str, right: &str) -> bool {
    let primary = |tag: &str| {
        tag.split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    primary(left) == primary(right)
}

/// One of a table's labels, as `|labels| labels.subtotal` picks it.
type Label = fn(&PaperLabels) -> &'static str;

/// The words a bill document prints: its labels in the receipt's language and, on a bilingual
/// receipt, in its second.
#[derive(Debug, Clone, Copy)]
pub(super) struct Words<'a> {
    pub(super) labels: &'a PaperLabels,
    pub(super) second: Option<&'a SecondLanguage>,
}

impl Words<'_> {
    /// `first` and the rest of its line, with `second` beside `first` where they fit
    /// ([`bilingual`]).
    pub(super) fn pair(
        self,
        first: &str,
        second: Option<&str>,
        rest: &str,
    ) -> (String, Option<String>) {
        match (self.second, second) {
            (Some(language), Some(second)) => bilingual(first, second, rest, language.columns),
            _ => (format!("{first}{rest}"), None),
        }
    }

    /// A label of each language and the rest of its line.
    pub(super) fn label(self, label: Label, rest: &str) -> (String, Option<String>) {
        let second = self.second.map(|language| label(language.labels));
        self.pair(label(self.labels), second, rest)
    }

    /// A left-aligned line, its second language under it, indented two spaces, where the two do not
    /// share it.
    pub(super) fn lines(line: (String, Option<String>)) -> Vec<PrintBlock> {
        let (first, under) = line;
        let mut blocks = vec![plain(first)];
        blocks.extend(under.map(|second| plain(format!("  {second}"))));
        blocks
    }

    /// A `label  amount` line, as [`amount_line`] prints it in one language.
    pub(super) fn amount(self, label: Label, amount: Money, money: &MoneyStyle) -> Vec<PrintBlock> {
        Self::lines(self.label(label, &format!("  {}", format_money(amount, money))))
    }

    /// A centred label; a second language that does not fit beside it is centred under it.
    pub(super) fn centred(self, label: Label, rest: &str, emphasised: bool) -> Vec<PrintBlock> {
        let (first, under) = self.label(label, rest);
        let mut blocks = vec![centred(first, emphasised)];
        blocks.extend(under.map(|second| centred(second, emphasised)));
        blocks
    }
}

/// `first / second` and `rest` on one line where it fits `columns` characters, as `Tạm tính /
/// Subtotal  VND 10.000`. Otherwise the line a receipt in one language prints, `first` and `rest`,
/// and `second` to print on a line of its own. A second that reads as the first prints once.
fn bilingual(first: &str, second: &str, rest: &str, columns: usize) -> (String, Option<String>) {
    let single = format!("{first}{rest}");
    if second == first {
        return (single, None);
    }
    let joined = format!("{first} / {second}{rest}");
    if joined.chars().count() <= columns {
        (joined, None)
    } else {
        (single, Some(second.to_owned()))
    }
}

/// The language a receipt, its copy and a pre-bill print in for `till`: its terminal's
/// `receipt_language` where it names one (ADR-0160 decision 4), and the `printing` node's
/// otherwise, resolved by [`paper_language`].
#[must_use]
pub fn receipt_language(session: &EdgeSession, till: &TillPrinting) -> Option<String> {
    paper_language(session, till.receipt_language(session))
}

/// The language a kitchen ticket printed at `station` prints in: the station's `ticket_language` on
/// the `stations` node (ADR-0160 decision 2), resolved by [`paper_language`] as a receipt's is.
///
/// `station` is the one whose printer prints the ticket ([`station_printer`]), whose cooks read it.
/// A station that sets no language, and one the plan does not name, prints in the display language,
/// as every kitchen ticket did before a station could say.
#[must_use]
pub fn ticket_language(session: &EdgeSession, station: StationId) -> Option<String> {
    let chosen = session
        .stations
        .station(station)
        .map_or(ReceiptLanguage::Display, KitchenStation::ticket_language);
    paper_language(session, chosen)
}

/// The language `chosen` names for the store's paper: the one resolver for a receipt's language and
/// a station's ticket language, which share the [`ReceiptLanguage`] choices.
///
/// `RECEIPT_LANGUAGE_DISPLAY`, the default, is the store's display language, which is what every
/// receipt and every kitchen ticket printed in before the settings existed.
/// `RECEIPT_LANGUAGE_COUNTRY` is the language of the store's country where the edge has labels in
/// it, and otherwise the display language: paper whose labels and dish names were in two languages
/// would be worse than either. `None` is a store with no language at all, whose paper prints English
/// labels as it always did.
fn paper_language(session: &EdgeSession, chosen: ReceiptLanguage) -> Option<String> {
    match chosen {
        ReceiptLanguage::Unspecified | ReceiptLanguage::Display => session.display_language.clone(),
        ReceiptLanguage::Country => session
            .country_language
            .clone()
            .filter(|language| PaperLabels::in_language(language).is_some())
            .or_else(|| session.display_language.clone()),
        named @ (ReceiptLanguage::Vietnamese | ReceiptLanguage::English) => {
            named.tag().map(str::to_owned)
        }
    }
}

/// A guest's rows with each name in the receipt's language where the menu translates the item, and
/// as the line captured it where it does not.
///
/// Only when the receipt's language is not the display language: a line captures its name in the
/// display language, so a receipt in that language prints exactly the names it always printed —
/// the snapshot a settled bill keeps whatever the menu says next week (§14.2).
pub(super) fn receipt_lines_in<'a>(
    session: &EdgeSession,
    till: &TillPrinting,
    lines: &'a [ReceiptLine],
) -> Cow<'a, [ReceiptLine]> {
    let Some(language) = receipt_language(session, till)
        .filter(|language| session.display_language.as_deref() != Some(language.as_str()))
    else {
        return Cow::Borrowed(lines);
    };
    let translated = |item: MenuItemId| -> Option<DisplayName> {
        session
            .menu_entry(item)
            .and_then(|entry| entry.display_name_translations.get(&language).cloned())
    };
    lines
        .iter()
        .map(|line| {
            let modifier_display_names = line
                .modifier_display_names
                .iter()
                .enumerate()
                .map(|(index, captured)| {
                    line.modifier_menu_item_ids
                        .get(index)
                        .and_then(|item| translated(*item))
                        .unwrap_or_else(|| captured.clone())
                })
                .collect();
            ReceiptLine {
                display_name: translated(line.menu_item_id)
                    .unwrap_or_else(|| line.display_name.clone()),
                modifier_display_names,
                ..line.clone()
            }
        })
        .collect::<Vec<_>>()
        .into()
}

pub(super) fn item_name(session: &EdgeSession, item: MenuItemId, language: Option<&str>) -> String {
    let Some(entry) = session.menu_entry(item) else {
        return item.to_string();
    };
    let name = match language {
        Some(language) if session.display_language.as_deref() != Some(language) => {
            entry.localized_name(language)
        }
        _ => &entry.display_name,
    };
    name.as_str().to_owned()
}
