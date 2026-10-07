// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Turning a settled bill into paper
//! ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md), production-readiness C2).
//!
//! `BillView::print_receipt` has been set on every settle since P5, and the till has rendered
//! "Printing receipt…" over it — while nothing constructed a `PrintJob` and no binary depended on
//! `printer-escpos`. This module is the missing half: which device, what document, and what to do
//! when the device does not answer.
//!
//! # Selection, not configuration
//!
//! Nothing here reads a config file. The devices come from the live [`EdgeSession`], which the
//! config-pull rebuilds and the boot restores — so a store that reboots with its broadband down
//! prints on the same printer it printed on before (C1, ADR-0100). A store with no `devices` node
//! selects nothing and prints nothing, and *says so*: that is the honest state for a LAN-only box or
//! a shop with no printer, and it is the state the till has been misreporting.
//!
//! # A drawer opens where the console says one is, and over USB only
//!
//! *When* to open one is the caller's decision: a cash payment, a paid in or out, a no-sale opening
//! ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)).
//! This module only performs it, through the printer the console marked `drawer_attached`
//! ([`drawer_printer`]).
//! [`PrinterConnection::may_open_a_drawer`](pos_ports::printer::PrinterConnection::may_open_a_drawer)
//! is false for anything but USB, because port 9100 has no authentication and the drawer-kick rides
//! the same channel as everything else (`docs/architecture.md` §5). A published node marking a network
//! printer's drawer is accepted here and the drawer command is still not sent. Behind a print agent
//! the kick is a job of its own, sent only to an agent whose claim says it carries one, and `OPENED`
//! waits on the agent's acknowledgement
//! ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
//!
//! # A printer may name another device as the one that writes the bytes
//!
//! [ADR-0112](../../../docs/adr/0112-print-agents.md): a published printer may carry an
//! `agent_device_id`, and **absent means the edge is the agent**, which is why a fleet that takes
//! this release prints tomorrow the way it printed today. When it is present the last hop moves —
//! and only the last hop. Everything above [`Printers::dispatch`] is unchanged, the rasterising in
//! [`Printers::prepare`] still runs *here*, and what crosses to the agent is a finished
//! [`PrintJob`]: a device that drew its own glyphs would draw them from whatever fonts that device
//! happens to have, and a store with three terminals would print three different tickets from one
//! order.
//!
//! # A till may print its guests' paper at a printer of its own
//!
//! [ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 4: a `TERMINAL` entry may name the receipt printer and the receipt languages of the till
//! it is, and a paired device is that till through the print-agent binding it already holds. A
//! route resolves the till from the device a request came from ([`Printers::till_for`]) and passes
//! the choice down ([`TillPrinting`]); a receipt, its copy and a pre-bill follow it, and nothing
//! else does. A store whose terminals name nothing prints exactly as before.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::num::NonZeroU16;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use pos_render::TextRenderer;
use printer_escpos::device::DeviceTransport;
use printer_escpos::tcp::TcpTransport;
use printer_escpos::{EscPosPrinter, Transport};

use pos_core::billing::BillTotals;
use pos_ports::printer::{
    CodePage, PrintBlock, PrintDocument, PrintJob, PrinterCapabilities, PrinterConnection,
    TextStyle,
};
use pos_ports::{PortError, PortName};
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::floor::{KitchenStation, StationPlan};
use pos_proto::ids::{DeviceId, EventId, MenuItemId, StationId, StoreId};
use pos_proto::locale::NumberFormat;
use pos_proto::money::Money;
use pos_proto::printing::{ReceiptLanguage, ReceiptSecondLanguage};
use pos_proto::quantity::Quantity;
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::DisplayName;

use pos_proto::ClockSource;
use tokio::sync::oneshot;

use crate::app::{
    BuyerDetails, EdgeSession, FiredLine, PreBill, ReceiptCopy, ReceiptLine, ShiftReport,
};
use crate::config::{ValueSource, log_in_force};
use crate::line_notes::NoteText;
use crate::paper_labels::PaperLabels;
use crate::print_agent::{AGENT_SILENCE, JOB_TTL, KICK_WAIT, MAX_QUEUED_PER_PRINTER};
use pos_core::business_date::local_time;

/// The store's receipt printer: the printer bound to no station.
///
/// A station printer serves a kitchen; the receipt printer serves the *bill*, which is why its
/// absent `station_id` is the thing that identifies it rather than a flag someone has to remember to
/// set. A store with two such printers gets the first published — a real ambiguity, and picking one
/// deterministically beats printing the guest's bill twice.
#[must_use]
pub fn receipt_printer(devices: &PublishedDevices) -> Option<&PublishedDevice> {
    devices
        .devices()
        .iter()
        .find(|device| device.station_id.is_none() && is_printer(device))
}

/// What one till prints differently from its store
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 4): the receipt printer and the receipt languages its `TERMINAL` entry names, as
/// [`Printers::till_for`] resolves them for the device a print was asked from.
///
/// Only a guest's paper follows it: a receipt, its copy and a pre-bill. A kitchen ticket goes to its
/// station's printer, a shift report to the store's receipt printer, and the cash drawer opens
/// through the store's ([`drawer_printer`]) until the multi-drawer shift work gives a till its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TillPrinting {
    /// The printer the till's paper goes to, where its terminal names one.
    receipt_printer_id: Option<DeviceId>,
    /// The language it prints in, where its terminal names one.
    receipt_language: Option<ReceiptLanguage>,
    /// Its second language, where its terminal names one.
    receipt_second_language: Option<ReceiptSecondLanguage>,
    /// Which till this is cannot be told: a terminal of the store prints differently, and the
    /// device's binding could not be read.
    unknown: bool,
}

impl TillPrinting {
    /// The store's choices, which every till printed with before a terminal could name its own: a
    /// device bound to no terminal prints with them, as does one whose terminal names nothing.
    pub const STORE: Self = Self {
        receipt_printer_id: None,
        receipt_language: None,
        receipt_second_language: None,
        unknown: false,
    };

    /// A till that cannot be told apart from the others, whose guest's paper is refused rather than
    /// printed at a counter that may be the wrong one.
    const UNKNOWN: Self = Self {
        unknown: true,
        ..Self::STORE
    };

    /// What `terminal`'s entry names, each the store's where it names nothing.
    #[must_use]
    pub fn of(terminal: &PublishedDevice) -> Self {
        Self {
            receipt_printer_id: terminal.receipt_printer_id,
            receipt_language: terminal.receipt_language(),
            receipt_second_language: terminal.receipt_second_language(),
            unknown: false,
        }
    }

    /// The language this till's paper prints in: its own, or the store's `printing` node's.
    fn receipt_language(&self, session: &EdgeSession) -> ReceiptLanguage {
        self.receipt_language
            .unwrap_or_else(|| session.printing.receipt_language())
    }

    /// The second language this till's paper prints in: its own, or the store's `printing` node's.
    fn receipt_second_language(&self, session: &EdgeSession) -> ReceiptSecondLanguage {
        self.receipt_second_language
            .unwrap_or_else(|| session.printing.receipt_second_language())
    }
}

/// Whether a terminal in `devices` names anything of its own to print with.
fn a_till_prints_its_own(devices: &PublishedDevices) -> bool {
    published_terminals(devices).any(|terminal| TillPrinting::of(terminal) != TillPrinting::STORE)
}

/// The printer a guest's paper for `till` goes to, or what the till is told instead.
///
/// The till's own receipt printer where its terminal names one the node lists as a printer that
/// serves no station, and the store's [`receipt_printer`] otherwise, which is also where every till
/// printed before a terminal could name one. A printer the node does not list is the store's case:
/// the cloud publishes none, so one only arrives from a node this edge did not expect.
///
/// No fallback past the till's own: once it is chosen, a print it fails is reported as that
/// printer's failure, because a bill printed at the wrong counter is worse than one that visibly did
/// not print. For the same reason a till that cannot be told apart ([`TillPrinting::UNKNOWN`]) is
/// refused, as [`PrintOutcome::Unavailable`].
fn guest_printer<'a>(
    devices: &'a PublishedDevices,
    till: &TillPrinting,
) -> Result<&'a PublishedDevice, PrintOutcome> {
    if till.unknown {
        tracing::warn!(
            "the till a guest's paper is for could not be told; nothing was printed rather than \
             printing it at a counter that may be the wrong one"
        );
        return Err(PrintOutcome::Unavailable);
    }
    till.receipt_printer_id
        .and_then(|own| {
            devices.devices().iter().find(|device| {
                device.device_id == own && device.station_id.is_none() && is_printer(device)
            })
        })
        .or_else(|| receipt_printer(devices))
        .ok_or_else(|| {
            tracing::info!("no receipt printer is published for this store; nothing to print");
            PrintOutcome::NoPrinter
        })
}

/// The printer serving `station`, falling back to the station plan's declared backup, and the
/// station whose printer it is: `station` itself, or its backup.
///
/// The failover target is the *plan's*, not this module's guess: `KitchenStation::backup_station_id`
/// is what an operator authored and `pos_core::floor` already validates (it must name a different
/// station in the same plan). One hop only — a backup chain that looped would print a ticket
/// somewhere nobody expected, and a ticket printed in the wrong kitchen is worse than one not
/// printed at all, because nobody goes looking for it.
///
/// The station comes back with the printer because a ticket prints in the language of the station
/// whose cooks read it ([`ticket_language`]), and on a failover those are the backup's.
#[must_use]
pub fn station_printer<'a>(
    devices: &'a PublishedDevices,
    plan: &StationPlan,
    station: StationId,
) -> Option<(StationId, &'a PublishedDevice)> {
    if let Some(direct) = printer_for_station(devices, station) {
        return Some((station, direct));
    }
    let backup = plan
        .stations()
        .iter()
        .find(|entry| entry.station_id == station)
        .and_then(|entry| entry.backup_station_id)?;
    printer_for_station(devices, backup).map(|device| (backup, device))
}

fn printer_for_station(devices: &PublishedDevices, station: StationId) -> Option<&PublishedDevice> {
    devices
        .devices()
        .iter()
        .find(|device| device.station_id == Some(station) && is_printer(device))
}

/// Whether this device is something to send bytes to.
///
/// A kind this build does not know is **not** a printer here. `Open` retained the token so the node
/// survived (ADR-0100), and retaining it is not the same as addressing it: sending ESC/POS to a
/// device whose type we cannot name is how a label printer ends up spitting a receipt.
fn is_printer(device: &PublishedDevice) -> bool {
    device.kind.known() == DeviceKind::Printer
}

/// The printer a cash drawer opens through, if this edge may open one
/// ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 4,
/// [ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
///
/// The first printer that serves the bill rather than a station, that the console marked
/// `drawer_attached`, whose connection may open a drawer, which is USB and nothing else, and whose
/// bytes this edge writes itself or a print agent writes that `carries_kick` says carries the kick.
/// Each condition is a way the kick would go wrong: a station printer's drawer is not the till's,
/// port 9100 has no authentication (`docs/architecture.md` §5), and an agent built before kicks
/// cannot read one. The one place the choice is made, so the drawer per till (ADR-0167) changes it
/// here.
#[must_use]
pub fn drawer_printer(
    devices: &PublishedDevices,
    carries_kick: impl Fn(DeviceId) -> bool,
) -> Option<&PublishedDevice> {
    devices.devices().iter().find(|device| {
        device.station_id.is_none()
            && is_printer(device)
            && device.drawer_attached
            && device.agent_device_id.is_none_or(&carries_kick)
            && connection_of(device).may_open_a_drawer()
    })
}

/// How a published device is attached, in the port's vocabulary.
///
/// A connection this build does not know maps to [`PrinterConnection::Network`] — the posture that
/// authorises *least*, because a drawer opens only over USB. Degrading toward the safe end is the
/// rule everywhere the two vocabularies meet.
#[must_use]
pub fn connection_of(device: &PublishedDevice) -> PrinterConnection {
    match device.connection.known() {
        DeviceConnection::Usb => PrinterConnection::Usb,
        DeviceConnection::Serial => PrinterConnection::Serial,
        DeviceConnection::Network | DeviceConnection::Unspecified => PrinterConnection::Network,
    }
}

/// A centred line, emphasised or not — the header block's shape.
fn centred(line: impl Into<String>, emphasised: bool) -> PrintBlock {
    PrintBlock::Text {
        line: line.into(),
        style: TextStyle {
            emphasised,
            double_size: false,
            centred: true,
        },
    }
}

/// The page a manager prints to check a printer is wired, from the Devices screen.
///
/// Says which printer it is, where the edge reached it and when, so the paper in a technician's hand
/// identifies itself — a shop with three printers learns which one is which by printing. The last
/// line answers the next question, whether the printer will print Vietnamese: a sample drawn by the
/// rasteriser when this box has fonts (ADR-0102), or an ASCII line saying it has none. Never a
/// Vietnamese line the box cannot draw — that refuses the whole page, and a wired printer would look
/// broken.
#[must_use]
pub fn test_page_document(
    profile: &StoreProfile,
    device: &PublishedDevice,
    printed_at: &str,
    can_rasterise: bool,
) -> PrintDocument {
    let mut blocks = Vec::new();
    if let Some(name) = profile.display_name() {
        blocks.push(centred(name, true));
    }
    blocks.push(centred("Printer test", true));
    for line in [
        format!("Printer: {}", device.name.as_str()),
        format!("Address: {}", device.address),
        format!("Printed: {printed_at}"),
        if can_rasterise {
            "Tiếng Việt: Phở bò · Bánh mì · Cà phê sữa đá".to_owned()
        } else {
            "Vietnamese text: no fonts on this store PC, so it will not print".to_owned()
        },
    ] {
        blocks.push(PrintBlock::Text {
            line,
            style: TextStyle::default(),
        });
    }
    PrintDocument { blocks }
}

/// The published printers, in publication order — what the Devices screen lists and can test.
#[must_use]
pub fn published_printers(devices: &PublishedDevices) -> Vec<&PublishedDevice> {
    devices
        .devices()
        .iter()
        .filter(|device| is_printer(device))
        .collect()
}

/// The published terminals, in publication order: the tills a paired device can be bound to
/// ([ADR-0112](../../../docs/adr/0112-print-agents.md)), and what the till's **This device** card
/// lists.
///
/// A kind this build does not know is not a terminal here, as it is not a printer either: the edge
/// acts on a device only for a kind it can read.
pub fn published_terminals(devices: &PublishedDevices) -> impl Iterator<Item = &PublishedDevice> {
    devices
        .devices()
        .iter()
        .filter(|device| device.kind.known() == DeviceKind::Terminal)
}

/// A `label            amount` line, which is how every figure on a receipt is read.
/// How this store writes money on paper: how many decimals its currency has
/// ([ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)) and what marks
/// its country groups and points a figure with
/// ([ADR-0136](../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)).
///
/// One value rather than two arguments, because the exponent alone was already threaded through four
/// functions here and a second parallel parameter beside it at every call would be the third
/// occurrence AGENTS.md §2 says to extract on. It also stops a caller pairing a store's exponent with
/// another store's marks, which two positional arguments of different types invite.
#[derive(Debug, Clone)]
pub struct MoneyStyle {
    /// How many decimal places the currency has.
    pub exponent: u8,
    /// The marks the store's country writes numbers with.
    pub format: NumberFormat,
}

impl MoneyStyle {
    /// The style a store's live session is publishing.
    #[must_use]
    pub fn of(session: &EdgeSession) -> Self {
        Self {
            exponent: session.currency_exponent,
            format: session.number_format.clone(),
        }
    }
}

fn amount_line(label: &str, amount: Money, money: &MoneyStyle) -> PrintBlock {
    PrintBlock::Text {
        line: format!("{label}  {}", format_money(amount, money)),
        style: TextStyle::default(),
    }
}

/// A tax rate in basis points as a percentage, without floating point — `250` is `2.50%`.
fn percent(basis_points: u32) -> String {
    format!("{}.{:02}%", basis_points / 100, basis_points % 100)
}

/// A left-aligned line in the printer's ordinary style.
fn plain(line: String) -> PrintBlock {
    PrintBlock::Text {
        line,
        style: TextStyle::default(),
    }
}

/// A receipt's second language
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 2): what a bilingual receipt, its copy and a pre-bill print after each of their labels,
/// and under each name the menu or a fee's rule translates into it.
///
/// Worked out from the session before a document is laid out, so the document stays a function of
/// its arguments. A document given none prints one language, exactly as before the setting existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondLanguage {
    /// Its labels.
    labels: &'static PaperLabels,
    /// How many characters the printer fits on a line, which decides whether a label and its second
    /// language share one.
    columns: usize,
    /// Each row's names in it, in the rows' order.
    rows: Vec<SecondNames>,
    /// Each fee line's name in it, in the order of the bill's fee lines.
    fees: Vec<Option<DisplayName>>,
}

/// One row's names in the second language: its item's, then each of its modifiers'. `None` where
/// the menu has no name in it, or the name is the one the row already prints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SecondNames {
    item: Option<DisplayName>,
    modifiers: Vec<Option<DisplayName>>,
}

impl SecondLanguage {
    /// The second language `session` prints `till`'s guest's paper in on a printer `columns` wide,
    /// or `None` for one language: none is set, the receipt's labels are in it already, or this box
    /// has no fonts to draw it with ([`PaperLabels::for_store`]).
    ///
    /// `captured` are the rows as the bill holds them and `printed` the same rows named in the
    /// receipt's language; `totals` names the fees as the receipt prints them.
    fn for_receipt(
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
struct Words<'a> {
    labels: &'a PaperLabels,
    second: Option<&'a SecondLanguage>,
}

impl Words<'_> {
    /// `first` and the rest of its line, with `second` beside `first` where they fit
    /// ([`bilingual`]).
    fn pair(self, first: &str, second: Option<&str>, rest: &str) -> (String, Option<String>) {
        match (self.second, second) {
            (Some(language), Some(second)) => bilingual(first, second, rest, language.columns),
            _ => (format!("{first}{rest}"), None),
        }
    }

    /// A label of each language and the rest of its line.
    fn label(self, label: Label, rest: &str) -> (String, Option<String>) {
        let second = self.second.map(|language| label(language.labels));
        self.pair(label(self.labels), second, rest)
    }

    /// A left-aligned line, its second language under it, indented two spaces, where the two do not
    /// share it.
    fn lines(line: (String, Option<String>)) -> Vec<PrintBlock> {
        let (first, under) = line;
        let mut blocks = vec![plain(first)];
        blocks.extend(under.map(|second| plain(format!("  {second}"))));
        blocks
    }

    /// A `label  amount` line, as [`amount_line`] prints it in one language.
    fn amount(self, label: Label, amount: Money, money: &MoneyStyle) -> Vec<PrintBlock> {
        Self::lines(self.label(label, &format!("  {}", format_money(amount, money))))
    }

    /// A centred label; a second language that does not fit beside it is centred under it.
    fn centred(self, label: Label, rest: &str, emphasised: bool) -> Vec<PrintBlock> {
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

/// The customer's receipt for a settled bill — and, where a country's law is satisfied by it, its
/// tax invoice ([ADR-0106](../../../docs/adr/0106-the-store-is-a-legal-person.md)).
///
/// Composed from four things and nothing else: who the store is, **what was sold**, what the bill
/// came to, and the store's own gapless receipt number — which is explicitly **not** a legal invoice
/// number ([ADR-0025](../../../docs/adr/0025-receipt-number-authority.md)); a country whose law wants
/// an allocated number gets it from `Fiscalization` and prints it beside this one.
///
/// # Every block is omitted when it has nothing to say
///
/// No registration number, no registration line. No service charge, no service-charge line. No
/// components, no indented parts. A store that has filled nothing in gets the receipt this framework
/// printed before ADR-0106 — a number and a total — rather than a page of blank labels, because an
/// empty label on a legal document reads as a value somebody forgot to type.
///
/// # The buyer block, when there is one
///
/// A Japanese qualified invoice issued to a company must carry that company's name and 登録番号, and
/// an Indian tax invoice under Rule 46 must carry the buyer's name, address and GSTIN — without them
/// the buyer cannot claim input tax, which is the reason a business asks for the document at all.
/// The block is printed only when a buyer was captured
/// ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)); an ordinary retail sale is the
/// receipt it always was.
///
/// # The lines, which every regime asks for
///
/// One row per line sold: the name, then the quantity at the unit price with the extended amount
/// beside it ([ADR-0129](../../../docs/adr/0129-a-receipt-itemises-what-was-sold.md)). Vietnam's
/// Decree 123/2020 Art. 10 asks for exactly this set (minus a unit of measure the catalog has not
/// got), and a Japanese qualified invoice, an Indian tax invoice and an EU one all want the goods
/// named and counted. The rows sum to `subtotal` printed under them, because they are the same
/// records it was assembled from.
///
/// The name gets a line of its own. It is the one field with no bound on its length, and a receipt
/// whose amounts have been pushed off the right-hand edge of the paper is not a document anybody can
/// check.
///
/// # The tax section is per rate, not per line
///
/// Which is what a Japanese qualified invoice (8 % and 10 % separately) and an Indian tax invoice
/// (CGST and SGST separately) both ask for, what `BillTotals::tax_lines` already computes, and what
/// keeps a long bill's receipt short.
///
/// # A second language
///
/// With `second` (ADR-0160 decision 2), each label prints in both languages, as `Tạm tính /
/// Subtotal` where the two fit on one line and with the second under the first, indented, where
/// they do not; and each name the second language has prints under its row's, indented.
#[must_use]
pub fn receipt_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    receipt_number: u64,
    lines: &[ReceiptLine],
    totals: &BillTotals,
    buyer: Option<&BuyerDetails>,
    second: Option<&SecondLanguage>,
) -> PrintDocument {
    bill_document(
        BillDocument::Receipt { receipt_number },
        profile,
        money,
        Words { labels, second },
        lines,
        totals,
        buyer,
    )
}

/// A copy of a settled bill's receipt
/// ([ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)):
/// the receipt as it printed, under the same number, with **COPY** and which copy it is, and when
/// it was printed, under that number. `reprinted` is the store's local time, as the caller formats
/// it. In a second language as [`receipt_document`] is.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "the receipt's own arguments plus the two a copy adds; a struct for them would be \
              built at one call site and taken apart at the next"
)]
pub fn receipt_copy_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    receipt_number: u64,
    copy_number: u32,
    reprinted: &str,
    lines: &[ReceiptLine],
    totals: &BillTotals,
    buyer: Option<&BuyerDetails>,
    second: Option<&SecondLanguage>,
) -> PrintDocument {
    bill_document(
        BillDocument::ReceiptCopy {
            receipt_number,
            copy_number,
            reprinted,
        },
        profile,
        money,
        Words { labels, second },
        lines,
        totals,
        buyer,
    )
}

/// The check a table asks for before it pays: the rows the receipt will print and what they come to,
/// headed as a pre-bill and with **no receipt number** (roadmap-v3 B2.1).
///
/// No number because nothing has been settled. The gapless receipt sequence (ADR-0025) numbers
/// settled bills, and a pre-bill printed three times while a table argues about the wine is not three
/// sales; printing one under a number would leave a hole in the series the day nobody paid. The
/// `reference` is the order's short reference, the one its kitchen tickets carry, so a server holding
/// the paper and a cook holding a ticket are talking about the same table.
///
/// "Not a receipt" is printed under the total because a pre-bill left on the table looks like one to
/// a guest who is not looking closely, and the difference is whether anything was paid. In a second
/// language as [`receipt_document`] is.
#[must_use]
pub fn pre_bill_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    reference: &str,
    lines: &[ReceiptLine],
    totals: &BillTotals,
    second: Option<&SecondLanguage>,
) -> PrintDocument {
    bill_document(
        BillDocument::PreBill { reference },
        profile,
        money,
        Words { labels, second },
        lines,
        totals,
        None,
    )
}

/// A closed shift's drawer on paper: the float, the cash taken, what was paid in and out outside a
/// sale, what that should come to, what was counted, and the difference.
///
/// The drawer's own arithmetic, in the order a supervisor checks it: the float, the cash taken and
/// the paid in, less the paid out, add up to the expectation, and the count less the expectation is
/// the variance. A shift that paid nothing in or out prints neither line. Printed because the domain asks for it on close
/// (`Effect::PrintShiftReport`), which it has done since the shift machine was written while nothing
/// turned the effect into paper — a cashier handing over a drawer had only a screen to show for it.
///
/// "Over" or "short" is written out beside the variance rather than left to its sign, because a
/// minus sign on thermal paper is a single dot-width wide and the word is the thing being checked.
#[must_use]
pub fn shift_report_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    report: &ShiftReport,
) -> PrintDocument {
    let mut blocks = Vec::new();
    if let Some(name) = profile.display_name() {
        blocks.push(centred(name, true));
    }
    blocks.push(centred(labels.shift_report, true));
    blocks.push(centred(
        short_reference(&report.shift_id.to_string()),
        false,
    ));
    blocks.push(amount_line(
        labels.opening_float,
        report.opening_float,
        money,
    ));
    blocks.push(amount_line(labels.cash_taken, report.cash_collected, money));
    // Only when there was one, so a shift that paid nothing in or out prints the report it always
    // did; when there was, the lines are what make the expectation add up on paper (ADR-0165).
    if report.paid_in.amount_minor != 0 {
        blocks.push(amount_line(labels.paid_in, report.paid_in, money));
    }
    if report.paid_out.amount_minor != 0 {
        blocks.push(amount_line(labels.paid_out, report.paid_out, money));
    }
    blocks.push(amount_line(
        labels.expected_in_drawer,
        report.expected_amount,
        money,
    ));
    blocks.push(amount_line(labels.counted, report.counted_amount, money));
    blocks.push(amount_line(labels.variance, report.variance, money));
    let verdict = match report.variance.amount_minor.cmp(&0) {
        core::cmp::Ordering::Less => labels.short,
        core::cmp::Ordering::Equal => labels.balanced,
        core::cmp::Ordering::Greater => labels.over,
    };
    blocks.push(centred(verdict, true));
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

/// Which document a bill's rows and totals are printed as.
///
/// One body with a kind rather than two functions, because the two documents are the same rows and
/// the same arithmetic and differ only in how they are headed and signed off. A second copy of the
/// body is how a receipt and a pre-bill come to disagree about the tax line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BillDocument<'a> {
    /// The guest's receipt for a settled bill, under its gapless number.
    Receipt { receipt_number: u64 },
    /// A copy of that receipt, under the same number and marked as a copy (ADR-0164).
    ReceiptCopy {
        receipt_number: u64,
        copy_number: u32,
        reprinted: &'a str,
    },
    /// The check before payment: headed as such, under the order's reference, and unnumbered.
    PreBill { reference: &'a str },
}

fn bill_document(
    kind: BillDocument<'_>,
    profile: &StoreProfile,
    money: &MoneyStyle,
    words: Words<'_>,
    lines: &[ReceiptLine],
    totals: &BillTotals,
    buyer: Option<&BuyerDetails>,
) -> PrintDocument {
    let mut blocks = Vec::new();

    // Who sold. The trading name leads and the legal name backs it up; the address and registration
    // follow, each only if it is there.
    if let Some(name) = profile.display_name() {
        blocks.push(centred(name, true));
    }
    blocks.extend(
        profile
            .address_lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| centred(line.clone(), false)),
    );
    if let Some(registration) = profile.registration_line() {
        blocks.push(centred(registration, false));
    }

    match kind {
        BillDocument::Receipt { receipt_number } => {
            blocks.push(centred(format!("#{receipt_number}"), false));
        }
        // The number the original printed, then what this paper is: a copy, which copy, and when it
        // was printed. A copy the guest takes away must not read as a second sale.
        BillDocument::ReceiptCopy {
            receipt_number,
            copy_number,
            reprinted,
        } => {
            blocks.push(centred(format!("#{receipt_number}"), false));
            blocks.extend(words.centred(|labels| labels.copy, "", true));
            blocks.extend(words.centred(
                |labels| labels.reprint,
                &format!(" {copy_number} - {reprinted}"),
                false,
            ));
        }
        BillDocument::PreBill { reference } => {
            blocks.extend(words.centred(|labels| labels.pre_bill, "", true));
            blocks.push(centred(reference, false));
        }
    }

    // Who bought, on a B2B invoice. Left-aligned rather than centred: this is a party to the
    // document, not a letterhead, and the two must not read as one block.
    if let Some(buyer) = buyer {
        blocks.extend(Words::lines(
            words.label(|labels| labels.bill_to, &format!(": {}", buyer.name)),
        ));
        for line in [buyer.tax_code.as_ref(), buyer.address.as_ref()]
            .into_iter()
            .flatten()
            .filter(|line| !line.trim().is_empty())
        {
            blocks.push(PrintBlock::Text {
                line: line.clone(),
                style: TextStyle::default(),
            });
        }
    }

    // What was sold, before what it came to. A settle always has rows — an empty bill has nothing to
    // allocate across and `assemble` refuses it — so this is not a "some receipts are itemised"
    // branch; passing none is how the cases below hold the rest of the document still while they
    // check one block of it.
    blocks.extend(row_blocks(lines, money, words.second));

    blocks.extend(totals_blocks(totals, money, words));

    blocks.push(PrintBlock::Text {
        line: format_money(totals.total_due, money),
        style: TextStyle {
            emphasised: true,
            double_size: true,
            centred: true,
        },
    });
    if matches!(kind, BillDocument::PreBill { .. }) {
        blocks.extend(words.centred(|labels| labels.not_a_receipt, "", false));
    }

    blocks.extend(
        profile
            .contact_lines
            .iter()
            .chain(profile.footer_lines.iter())
            .filter(|line| !line.trim().is_empty())
            .map(|line| centred(line.clone(), false)),
    );
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

/// What a kitchen ticket says about a line's guest note
/// ([ADR-0157](../../../docs/adr/0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TicketNote {
    /// No note was written.
    #[default]
    None,
    /// The note, as the server or the guest wrote it.
    Text(String),
    /// A note was written and the edge no longer holds it — it restarted since. The ticket says so,
    /// because a cook who reads nothing makes the dish as the menu describes it.
    Lost,
}

/// One line of a kitchen ticket: what to make, how many, and what was changed about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketLine {
    /// The item's name as the store's published menu spells it.
    pub item: String,
    /// How many, already rendered — [`Quantity`](pos_proto::quantity::Quantity) supports halves for a
    /// split item, and the kitchen reads the same figure the till showed.
    pub quantity: String,
    /// The chosen modifiers, in the order they were added.
    pub modifiers: Vec<String>,
    /// The guest's note, printed last and emphasised.
    pub note: TicketNote,
}

/// The ticket a station's printer produces when a line fires.
///
/// `reference` is what the kitchen calls back across the pass — a table's name, or the tail of the
/// order's id when there is no table. Double-size, because it is read from a metre away over a hot
/// counter; the item is emphasised; modifiers follow indented so a cook scanning the ticket sees
/// "what" before "how". A guest note comes last and emphasised, after a `!`, because it is the part
/// of a ticket that is most often about somebody's health.
#[must_use]
pub fn ticket_document(reference: &str, line: &TicketLine, labels: &PaperLabels) -> PrintDocument {
    let mut blocks = vec![
        PrintBlock::Text {
            line: reference.to_owned(),
            style: TextStyle {
                emphasised: true,
                double_size: true,
                centred: false,
            },
        },
        PrintBlock::Text {
            line: format!("{} x {}", line.quantity, line.item),
            style: TextStyle {
                emphasised: true,
                double_size: false,
                centred: false,
            },
        },
    ];
    blocks.extend(line.modifiers.iter().map(|modifier| PrintBlock::Text {
        line: format!("  + {modifier}"),
        style: TextStyle::default(),
    }));
    let note = match &line.note {
        TicketNote::None => None,
        TicketNote::Text(note) => Some(note.as_str()),
        TicketNote::Lost => Some(labels.note_lost),
    };
    if let Some(note) = note {
        blocks.push(PrintBlock::Text {
            line: format!("  ! {note}"),
            style: TextStyle {
                emphasised: true,
                double_size: false,
                centred: false,
            },
        });
    }
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

/// The ticket a fired line prints, with every id resolved against the store's published menu.
///
/// An item the menu does not name falls back to its identifier rather than to a blank: a cook can
/// still match a ticket to a screen, and a silently empty line is how the wrong dish gets made. The
/// same rule applies to each modifier.
///
/// `language` is the ticket's ([`ticket_language`]). In a language other than the display language
/// each item and modifier prints the menu's translation for it, and the name the menu publishes
/// where it has none; in the display language, or with none, the names are the ones every ticket
/// printed before a station could choose.
///
/// `note` is the guest note the edge holds for the line, if it still holds one (ADR-0157).
#[must_use]
pub fn ticket_line(
    session: &EdgeSession,
    fired: &FiredLine,
    note: Option<&NoteText>,
    language: Option<&str>,
) -> TicketLine {
    TicketLine {
        item: item_name(session, fired.menu_item_id, language),
        quantity: format_quantity(fired.quantity),
        modifiers: fired
            .modifier_menu_item_ids
            .iter()
            .map(|modifier| item_name(session, *modifier, language))
            .collect(),
        note: match note {
            Some(note) => TicketNote::Text(note.as_str().to_owned()),
            None if fired.note_present => TicketNote::Lost,
            None => TicketNote::None,
        },
    }
}

/// The ticket `fired` prints at `printing`, the station whose printer prints it
/// ([`station_printer`]): its labels and its names in that station's language ([`ticket_language`],
/// ADR-0160 decision 2).
///
/// The labels fall back to English on a box that cannot draw the language's, as all of a store's
/// paper does. A station that sets no language prints in the display language, so its ticket is the
/// one every station printed before a station could choose.
#[must_use]
pub fn ticket_at(
    session: &EdgeSession,
    can_rasterise: bool,
    printing: StationId,
    reference: &str,
    fired: &FiredLine,
    note: Option<&NoteText>,
) -> PrintDocument {
    let language = ticket_language(session, printing);
    ticket_document(
        reference,
        &ticket_line(session, fired, note, language.as_deref()),
        PaperLabels::for_store(language.as_deref(), can_rasterise),
    )
}

/// Renders a quantity the way a ticket reads it: `2`, or `0.5` for one half of a split item.
///
/// [`Quantity`] counts thousandths, so the raw figure is `2000`. A kitchen ticket saying "2000 x
/// Margherita" is not a rounding bug an operator would ever guess at, which is why this exists
/// rather than a `Display` on the wire type — the wire keeps its integer.
fn format_quantity(quantity: Quantity) -> String {
    let milli = quantity.as_milli();
    let sign = if milli < 0 { "-" } else { "" };
    let magnitude = milli.unsigned_abs();
    let scale = Quantity::SCALE.unsigned_abs();
    let whole = magnitude / scale;
    let fraction = magnitude % scale;
    if fraction == 0 {
        format!("{sign}{whole}")
    } else {
        let decimals = format!("{fraction:03}");
        format!("{sign}{whole}.{}", decimals.trim_end_matches('0'))
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
fn receipt_lines_in<'a>(
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

fn item_name(session: &EdgeSession, item: MenuItemId, language: Option<&str>) -> String {
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

/// A ticket reference short enough to read across a kitchen.
///
/// The tail of a ULID, which is its random half — six characters distinguish every order a station
/// has open at once, and the whole 26 would be read out wrong.
#[must_use]
pub fn short_reference(id: &str) -> String {
    let tail: String = id.chars().rev().take(6).collect();
    tail.chars().rev().collect()
}

/// Renders money for a receipt — `VND 99,000`, or `INR 261.45` where the currency has decimals.
///
/// This printed minor units with no decimal at all, and said so on purpose: *"the decimal place is a
/// locale question and a receipt that invented one would be wrong in half the countries this
/// framework targets."* That was right while nothing in the tree held the answer. Since
/// [ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md) the store
/// publishes it, so nothing is invented — and what was being printed was not a safe abstention but a
/// wrong number on a tax document: a guest in India who paid ₹261.45 was handed a receipt reading
/// `INR 26145`.
///
/// # Why the ISO code and not the symbol
///
/// The till draws `99,000₫`, and matching it exactly would be better for a guest holding the paper
/// beside the screen. It is not worth what it costs. A thermal printer's own character set does not
/// reach `₫` ([ADR-0102](../../../docs/adr/0102-printing-any-script.md)), so the total line would
/// have to be rasterised — and a store with no font package installed prints what the printer's
/// firmware covers and nothing else. That store prints its total today. It would stop.
///
/// So the line stays ASCII: the figure is corrected, and the one thing a receipt must always carry
/// keeps working on a box nobody has finished installing.
///
/// The marks are the store's own, published on its `locale` node
/// ([ADR-0136](../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)). This used to
/// group with `,` and point with `.` for every store on earth, and said so: *"making a country's
/// typography authoritative is a separate decision from getting its arithmetic right."* It was, it
/// has since been made, and a Vietnamese guest's receipt now reads `VND 97.900` rather than a
/// spelling nobody in the country writes.
fn format_money(amount: Money, money: &MoneyStyle) -> String {
    let negative = amount.amount_minor < 0;
    let absolute = amount.amount_minor.unsigned_abs();
    // Integer the whole way, like every other figure in this crate: `10_u128.pow` rather than a
    // float, and a remainder rather than a fractional part.
    let scale = 10_u128.pow(u32::from(money.exponent));
    let major = u128::from(absolute) / scale;
    let minor = u128::from(absolute) % scale;
    let sign = if negative { "-" } else { "" };
    let grouped = group_digits(major, &money.format);
    if money.exponent == 0 {
        return format!("{} {sign}{grouped}", amount.currency_code);
    }
    let digits = usize::from(money.exponent);
    let point = money.format.decimal_separator;
    format!(
        "{} {sign}{grouped}{point}{minor:0digits$}",
        amount.currency_code
    )
}

/// What was sold, one row per line: its name, then the quantity at the unit price with the extended
/// amount beside it.
///
/// What the line was made with comes between, as the kitchen ticket prints it (ADR-0144): after the
/// name and before the amount, because the unit price already includes every modifier's. On a
/// bilingual receipt each name the second language has prints under the first, indented.
fn row_blocks(
    lines: &[ReceiptLine],
    money: &MoneyStyle,
    second: Option<&SecondLanguage>,
) -> Vec<PrintBlock> {
    let mut blocks = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let names = second.and_then(|second| second.rows.get(index));
        blocks.push(plain(line.display_name.as_str().to_owned()));
        if let Some(name) = names.and_then(|names| names.item.as_ref()) {
            blocks.push(plain(format!("  {name}")));
        }
        for (position, modifier) in line.modifier_display_names.iter().enumerate() {
            blocks.push(plain(format!("  + {}", modifier.as_str())));
            if let Some(name) = names
                .and_then(|names| names.modifiers.get(position))
                .and_then(Option::as_ref)
            {
                blocks.push(plain(format!("    {name}")));
            }
        }
        blocks.push(amount_line(
            &format!(
                "  {} x {}",
                format_quantity(line.quantity),
                format_money(line.unit_price, money)
            ),
            line.line_total,
            money,
        ));
    }
    blocks
}

/// What a bill was charged, above its total: the subtotal, whatever came off or was added, each tax
/// rate, and the rounding.
///
/// `subtotal` reads net of tax under the inclusive posture (ADR-0104), so the column adds up on paper
/// in both postures — which is what makes the document check out when somebody totals it by hand.
/// Its own function so the document around it stays readable; the rows it prints are the same.
fn totals_blocks(totals: &BillTotals, money: &MoneyStyle, words: Words<'_>) -> Vec<PrintBlock> {
    let mut blocks = words.amount(|labels| labels.subtotal, totals.subtotal, money);
    if !totals.discount_total.is_zero() {
        blocks.extend(words.amount(|labels| labels.discount, totals.discount_total, money));
    }
    if !totals.comp_total.is_zero() {
        blocks.extend(words.amount(|labels| labels.comps, totals.comp_total, money));
    }
    blocks.extend(fee_blocks(totals, money, words));
    for line in &totals.tax_lines {
        blocks.extend(tax_block(line, money, words));
    }
    // A copy whose per-rate lines could not be recovered prints the tax the settle recorded as one
    // figure (ADR-0164 decision 5). `assemble` always fills the lines, so no other document gets here.
    if totals.tax_lines.is_empty() && !totals.tax_total.is_zero() {
        blocks.extend(words.amount(|labels| labels.tax, totals.tax_total, money));
    }
    if !totals.rounding_adjustment.is_zero() {
        blocks.extend(words.amount(|labels| labels.rounding, totals.rounding_adjustment, money));
    }
    blocks
}

/// The fees a bill was charged, each on its own line under its own name
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 4), in the order of its
/// rules. A bill with no fee line prints its service charge on one line, as every receipt did
/// before fees were itemised; a bill with fees prints a service charge of its own only for what no
/// fee accounts for, which is nothing on a bill the edge assembled. On a bilingual receipt a fee
/// whose rule names it in the second language prints that name as a label's second language prints.
fn fee_blocks(totals: &BillTotals, money: &MoneyStyle, words: Words<'_>) -> Vec<PrintBlock> {
    let mut blocks: Vec<PrintBlock> = totals
        .fee_lines
        .iter()
        .enumerate()
        .filter(|(_, fee)| !fee.amount.is_zero())
        .flat_map(|(index, fee)| {
            let second = words
                .second
                .and_then(|second| second.fees.get(index))
                .and_then(Option::as_ref);
            Words::lines(words.pair(
                fee.display_name.as_str(),
                second.map(DisplayName::as_str),
                &format!("  {}", format_money(fee.amount, money)),
            ))
        })
        .collect();
    let itemised = totals.fee_lines.iter().try_fold(
        Money::zero(totals.service_charge.currency_code),
        |sum, fee| sum.checked_add(fee.amount),
    );
    let rest = itemised.and_then(|fees| totals.service_charge.checked_sub(fees));
    match rest {
        Ok(rest) if rest.is_zero() => {}
        Ok(rest) => blocks.extend(words.amount(|labels| labels.service_charge, rest, money)),
        // Fees that do not add up in the bill's currency cannot be split from the service charge,
        // so the paper prints the service charge whole, as it did before fees were itemised.
        Err(_) => {
            blocks = words.amount(|labels| labels.service_charge, totals.service_charge, money);
        }
    }
    blocks
}

/// A bill's totals with each fee named in the receipt's language
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 4): the store's current
/// rule's translation where it has one, and the name the bill froze where it does not.
///
/// Unlike an item's, a fee's frozen name is the rule's own rather than one already resolved to the
/// display language, so it is looked up in whatever language the receipt prints in.
fn receipt_totals_in<'a>(
    session: &EdgeSession,
    till: &TillPrinting,
    totals: &'a BillTotals,
) -> Cow<'a, BillTotals> {
    let Some(language) = receipt_language(session, till) else {
        return Cow::Borrowed(totals);
    };
    if !totals
        .fee_lines
        .iter()
        .any(|fee| session.fee_translation(fee.fee_id, &language).is_some())
    {
        return Cow::Borrowed(totals);
    }
    let mut named = totals.clone();
    for fee in &mut named.fee_lines {
        if let Some(name) = session.fee_translation(fee.fee_id, &language) {
            fee.display_name = name.clone();
        }
    }
    Cow::Owned(named)
}

/// One tax rate's line, and the components it is made of beneath it.
///
/// Its own function because `receipt_document` is at the line ceiling and this is the part of it
/// with a shape of its own: a rate, then the parts of that rate, indented under the line they
/// explain. They are allocated out of the *rounded* tax, so they sum to it exactly
/// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)) — which is the property
/// that makes the block printable at all, rather than three figures that nearly add up.
fn tax_block(
    line: &pos_core::billing::TaxLine,
    money: &MoneyStyle,
    words: Words<'_>,
) -> Vec<PrintBlock> {
    let rest = format!(
        " {}  {}",
        percent(line.rate_basis_points),
        format_money(line.tax, money)
    );
    let mut blocks = Words::lines(words.label(|labels| labels.tax, &rest));
    blocks.extend(line.components.iter().map(|component| {
        amount_line(
            &format!(
                "  {} {}",
                component.name,
                percent(component.rate_basis_points)
            ),
            component.tax,
            money,
        )
    }));
    blocks
}

/// `1234567` as `1,234,567`, or as `1.234.567` where that is how the country writes it.
///
/// Written out rather than taken from a formatting crate, because this is a handful of lines and the
/// alternative is a dependency on a receipt path — and because `pos-edge` has no allocator pressure
/// here: a receipt has a few dozen figures on it.
///
/// `digits_per_group` is a single number, so this groups uniformly. India writes `12,34,567` —
/// three digits then pairs — which no single number can express;
/// [ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md) records that gap and
/// [ADR-0136](../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md) deliberately leaves
/// it open, so an Indian receipt reads `1,234,567` after this change exactly as it did before.
///
/// A group size of zero would loop forever here; it cannot arrive, because the edge refuses to apply
/// a published format carrying one, but the guard is written rather than assumed.
fn group_digits(value: u128, format: &NumberFormat) -> String {
    let digits = value.to_string();
    let size = usize::from(format.digits_per_group.max(1));
    let mut grouped = String::with_capacity(digits.len() + digits.len() / size);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(size) {
            grouped.push(format.group_separator);
        }
        grouped.push(digit);
    }
    grouped
}

/// What came of a print attempt, as the till reports it.
///
/// Seven outcomes, and six of them are not errors. The till has spent every release since P5 saying
/// "Printing receipt…" over a store with no printer wired at all; naming what actually happened is
/// most of what this was for (ADR-0100). The last three are ADR-0112's: a printer whose transport
/// belongs to another device is reached through a queue, and a queue has answers a socket does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintOutcome {
    /// The bytes reached a printer.
    Printed,
    /// Nothing was published to print on. An ordinary state, not a fault: a shop with no printer, or
    /// a box that has never synced.
    NoPrinter,
    /// A printer was chosen and could not take the job — unreachable, out of paper, cover open.
    Unavailable,
    /// The document contains a character this printer cannot render as text, and this build cannot
    /// render it as a bitmap either (`docs/pos-spec.md` §13).
    ///
    /// The one outcome that is a gap rather than a state of the world: a Vietnamese item name needs
    /// CP1258 or a rasteriser, and `printer-escpos` carries neither yet. Refusing is the only correct
    /// answer available — sending the bytes anyway prints a line of question marks in front of a
    /// customer. The kitchen still sees the order on its display; it is the *paper* that is missing.
    Unprintable,
    /// The printer names an agent, and the finished document is on that agent's queue (ADR-0112).
    ///
    /// Not [`Self::Printed`], because no paper has come out yet and the till must not claim it has.
    /// The agent claims within its park — twenty seconds at the outside — so what the cashier reads
    /// is *on its way*, not *done*. A job that never gets claimed expires at `JOB_TTL` and the
    /// console hears about the silence before the night ends.
    QueuedToAgent,
    /// The printer names an agent that holds no binding, or that has not asked for work within
    /// `AGENT_SILENCE`. **Nothing was written.**
    ///
    /// Checked before the queue is touched, which is the whole point of the ordering: a queue must
    /// not start building behind a box that is not there.
    AgentUnavailable,
    /// That printer already holds its full allowance of unexpired jobs. Nothing was written.
    ///
    /// Not a dead agent — that is refused a step earlier. This is a *live* agent whose printer is
    /// not consuming: paper out, cover open, a write that errors and a job that returns to the queue
    /// at the claim lease, while the till keeps firing. Adding a 201st job to a printer that has not
    /// consumed 200 is promising paper that is not coming, so the enqueue refuses and the operator
    /// learns it at the till rather than from the absence of a ticket.
    QueueFull,
}

impl PrintOutcome {
    /// The stable token the API reports, which the till maps to its own wording.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Printed => "PRINTED",
            Self::NoPrinter => "NO_PRINTER",
            Self::Unavailable => "PRINTER_UNAVAILABLE",
            Self::Unprintable => "UNPRINTABLE_TEXT",
            Self::QueuedToAgent => "QUEUED_TO_AGENT",
            Self::AgentUnavailable => "PRINT_AGENT_UNAVAILABLE",
            Self::QueueFull => "PRINT_QUEUE_FULL",
        }
    }

    /// Whether paper came out.
    ///
    /// [`Self::QueuedToAgent`] is deliberately `false`. Paper has not come out; a job is on a queue
    /// that a device the edge cannot see is expected to claim. Reporting a queued job as printed
    /// would put the one outcome the edge cannot verify into the one answer that claims certainty.
    #[must_use]
    pub const fn printed(self) -> bool {
        matches!(self, Self::Printed)
    }
}

/// What came of opening the cash drawer, as the till reports it
/// ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 4).
///
/// None of the three is an error. The act the drawer opened for has already been recorded, and a
/// drawer that did not spring is opened with its key; what the till needs is which of these happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawerOutcome {
    /// The kick went down the drawer printer's cable, or, behind a print agent, the agent
    /// acknowledged writing it within [`KICK_WAIT`]. Whether the drawer sprang is the hardware's
    /// part (gate P8), which the edge cannot see.
    Opened,
    /// No printer this edge may open a drawer through: none is marked, or the marked one is not on
    /// USB, serves a station, or prints through an agent that does not carry a kick, one built
    /// before kicks. The ordinary state of a store with no drawer.
    NoDrawer,
    /// The drawer printer was chosen and did not take the kick: unplugged or switched off, or its
    /// agent silent, its queue full, or no acknowledgement within [`KICK_WAIT`].
    Unavailable,
}

impl DrawerOutcome {
    /// The stable token the API reports, which the till maps to its own wording.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Opened => "OPENED",
            Self::NoDrawer => "NO_DRAWER",
            Self::Unavailable => "DRAWER_UNAVAILABLE",
        }
    }
}

/// Where a job goes when its printer names an agent.
///
/// **Dyn-compatible on purpose, unlike the three seams underneath it.** `PrintAgents`, `PrintQueue`
/// and `PrintWake` all return `impl Future`, so none of them can be erased — and [`Printers`] is
/// held as one `Arc<Printers>` in an axum extension by every composition and every route test. Making
/// it generic over three more parameters would push those three into the signature of every caller
/// that only ever wanted to print a receipt. So the erasure happens once, here, at the point where
/// the three become one question: *can this agent take this job, and does it now have it?*
///
/// The binding answers one more question, and this is where a route can ask it: which terminal a
/// paired device is, which decides where that till's guests' paper prints
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 4).
pub trait AgentDispatch: Send + Sync {
    /// Offers `job` to `agent` for `printer`, answering with what the till should say.
    ///
    /// Never an error: a printer that is down must not roll back a bill the guest has already paid,
    /// which is the same rule the direct path follows.
    fn enqueue<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>>;

    /// Offers a kick, a job that is [`PrintDocument::kick`] alone, to `agent` for `printer`, under
    /// the same rules as [`Self::enqueue`] but deliverable only for [`KICK_WAIT`]
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
    ///
    /// A dispatch that cannot carry one refuses it, which is what the default does.
    fn kick<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        let _ = (agent, printer, job);
        Box::pin(async { PrintOutcome::AgentUnavailable })
    }

    /// The terminal paired `device` answers for, if any: the till it is.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the binding cannot be read.
    fn terminal_of<'a>(
        &'a self,
        device: DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<DeviceId>, PortError>> + Send + 'a>>;
}

/// The shipped [`AgentDispatch`]: the binding, the queue and the wake, in ADR-0112's order.
pub struct AgentLane<A, Q, W> {
    agents: A,
    queue: Q,
    wake: W,
}

impl<A, Q, W> fmt::Debug for AgentLane<A, Q, W> {
    /// The name and nothing else. Hand-written rather than derived because the three seams are not
    /// required to be `Debug` — and because the queue holds rendered documents, which
    /// `pos_ports::printer` says are never written to a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AgentLane")
    }
}

impl<A, Q, W> AgentLane<A, Q, W> {
    /// A lane over one binding record, one queue and one wake.
    ///
    /// All three must be the *same* values the agent's routes hold. Two queues over one store would
    /// be two answers to "what is waiting", and a wake nobody is parked on is a wake that never
    /// arrives.
    pub const fn new(agents: A, queue: Q, wake: W) -> Self {
        Self {
            agents,
            queue,
            wake,
        }
    }
}

impl<A, Q, W> AgentLane<A, Q, W>
where
    A: crate::print_agent::PrintAgents,
    Q: crate::print_queue::PrintQueue,
    W: crate::print_wake::PrintWake,
{
    /// Offers `job` to `agent` for `printer`, deliverable for `ttl`: a print job's [`JOB_TTL`], a
    /// kick's [`KICK_WAIT`].
    fn offer<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
        ttl: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        Box::pin(async move {
            let now_ms = crate::clock::SystemClock
                .now()
                .as_milliseconds_since_epoch();

            // 1. The agent, before the table is touched. A queue must not start building behind a
            //    box that is not there, and the ordering is what makes the cap's refusal below mean
            //    something specific.
            let standing = match self.agents.standing(agent).await {
                Ok(standing) => standing,
                Err(error) => {
                    // A store that will not say whether the agent is there is treated as *not*
                    // there: nothing is written, and the till reads a named refusal.
                    tracing::warn!(%agent, %error, "a print agent's standing could not be read");
                    return PrintOutcome::AgentUnavailable;
                }
            };
            let silence_ms = i64::try_from(AGENT_SILENCE.as_millis()).unwrap_or(i64::MAX);
            let heard_from = standing.map(|standing| standing.last_seen_ms);
            if heard_from.is_none_or(|last| now_ms.saturating_sub(last) > silence_ms) {
                tracing::warn!(
                    %agent,
                    %printer,
                    bound = heard_from.is_some(),
                    "the printer's agent is unbound or silent; nothing was queued"
                );
                return PrintOutcome::AgentUnavailable;
            }

            // 2. The cap. The instants are computed here rather than in the adapter, because the TTL
            //    is a constant in one edge module and a store that held it would be a second place
            //    to change it.
            let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
            let job_id = job.job_id;
            let queued = self
                .queue
                .enqueue(
                    agent,
                    printer,
                    job,
                    now_ms,
                    now_ms.saturating_add(ttl_ms),
                    MAX_QUEUED_PER_PRINTER,
                )
                .await;

            // 3. And the row, or the refusal the table decided.
            match queued {
                Ok(
                    crate::print_queue::Enqueued::Queued
                    | crate::print_queue::Enqueued::AlreadyQueued,
                ) => {
                    // Signalled on both. `AlreadyQueued` is a redelivery of the *same* ticket, so
                    // nothing is duplicated — and if the first enqueue signalled while nobody was
                    // parked, this one may be the signal that finally lands. The wake is an
                    // optimisation either way; the row is the truth (ADR-0062).
                    self.wake.queued(agent);
                    // The job's identifier and its destination, never its content
                    // (`pos_ports::printer`).
                    tracing::info!(%job_id, %agent, %printer, "queued to a print agent");
                    PrintOutcome::QueuedToAgent
                }
                Ok(crate::print_queue::Enqueued::QueueFull) => {
                    tracing::warn!(
                        %job_id,
                        %agent,
                        %printer,
                        "that printer holds its full allowance of unexpired jobs; nothing was queued"
                    );
                    PrintOutcome::QueueFull
                }
                Err(error) => {
                    tracing::warn!(%job_id, %agent, %printer, %error, "the print queue refused the job");
                    PrintOutcome::Unavailable
                }
            }
        })
    }
}

impl<A, Q, W> AgentDispatch for AgentLane<A, Q, W>
where
    A: crate::print_agent::PrintAgents,
    Q: crate::print_queue::PrintQueue,
    W: crate::print_wake::PrintWake,
{
    fn enqueue<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        self.offer(agent, printer, job, JOB_TTL)
    }

    fn kick<'a>(
        &'a self,
        agent: DeviceId,
        printer: DeviceId,
        job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        self.offer(agent, printer, job, KICK_WAIT)
    }

    fn terminal_of<'a>(
        &'a self,
        device: DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<DeviceId>, PortError>> + Send + 'a>> {
        Box::pin(self.agents.agent_for(device))
    }
}

/// A kick a till is waiting on, which nobody waits on once this is dropped.
struct AwaitedKick<'a> {
    awaited: &'a Mutex<HashMap<EventId, oneshot::Sender<()>>>,
    job: EventId,
    receiver: oneshot::Receiver<()>,
}

impl AwaitedKick<'_> {
    /// Whether the agent acknowledged the kick within `wait`.
    async fn within(mut self, wait: std::time::Duration) -> bool {
        matches!(
            tokio::time::timeout(wait, &mut self.receiver).await,
            Ok(Ok(()))
        )
    }
}

impl Drop for AwaitedKick<'_> {
    fn drop(&mut self) {
        self.awaited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.job);
    }
}

/// Opens a byte channel to a published device.
///
/// A seam, not an abstraction for its own sake: [`TcpTransports`] is what a store runs, and a test
/// substitutes a recorder so the whole dispatch — selection, failover, the code-page refusal, the
/// idempotency key — is exercised in CI without a printer on the desk.
pub trait TransportFactory: Send + Sync + fmt::Debug {
    /// Opens a channel to `device`.
    ///
    /// # Errors
    ///
    /// [`PortError::failed_precondition`] for a connection this build has no transport for — USB and
    /// serial, which need hardware bring-up (`docs/gate-register.md` §6).
    fn open(&self, device: &PublishedDevice) -> Result<Box<dyn Transport>, PortError>;
}

/// The production factory: raw TCP on port 9100 for a network printer, and nothing else yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct TcpTransports;

impl TransportFactory for TcpTransports {
    fn open(&self, device: &PublishedDevice) -> Result<Box<dyn Transport>, PortError> {
        match connection_of(device) {
            PrinterConnection::Network => Ok(Box::new(TcpTransport::new(&device.address))),
            // A directly-attached printer's address is a device path — `/dev/usb/lp0`,
            // `/dev/ttyUSB0`, `\\.\COM3` — rather than a host
            // ([ADR-0103](../../../docs/adr/0103-directly-attached-printers.md)). Dialling port 9100
            // at one would fail with a message about the network, which is the wrong thing to hand
            // an operator holding a USB cable, so the two are kept apart here.
            PrinterConnection::Usb | PrinterConnection::Serial => {
                Ok(Box::new(DeviceTransport::new(&device.address)))
            }
            // `PrinterConnection` is `#[non_exhaustive]`.
            _ => Err(PortError::failed_precondition(
                PortName::PrinterDriver,
                "this build has no transport for that connection",
            )),
        }
    }
}

/// How many distinct missing characters a font warning names before it stops naming them.
///
/// The warning exists so an operator knows which font to install, and a handful of characters says
/// which script that is. The *whole* set says more than that: a receipt block carries a buyer's name
/// (`pos_ports::printer`), and on a box with no Vietnamese or CJK face every character of that name
/// substitutes — so an unbounded set in a durable log is the name itself, spelled with the
/// duplicates removed ([ADR-0117](../../../docs/adr/0117-a-headless-store-keeps-a-log.md)
/// decision 7). The count is reported alongside, so a truncation is visible as one.
const MISSING_GLYPHS_REPORTED: usize = 6;

/// What this build assumes about a published printer.
///
/// A device proposal carries an address, a kind and — since ADR-0100's approval change — a
/// connection, and the console says what paper a printer takes and whether it cuts
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 2). It does not carry a code page, and no discovery protocol reports one. So every
/// unknown is set to the answer that cannot produce a wrong receipt:
///
/// - `code_page: Ascii` — the repertoire every ESC/POS printer has. Claiming more would print
///   question marks; claiming [`CodePage::Unsupported`] would refuse plain ASCII too. A code page is
///   not a choice the console offers, because the adapter sends no other.
/// - `columns` and `dots_per_line` — the paper the console says the printer takes
///   ([`PublishedDevice::paper_width`]): how many characters a bilingual receipt lays a label out
///   against, and how wide a raster is drawn, so a 58 mm printer is not sent an 80 mm image that
///   shears on its head. Where the console says none, 80 mm's 42 characters and 576 dots, which
///   every printer was taken to be before it could.
/// - `cuts_paper` — what the console says, and `true` where it says nothing. A printer with no
///   cutter is sent no cut ([`Printers::prepare`]).
/// - `prints_bitmaps` — whether a font is loaded, and nothing about the hardware. `GS v 0` is
///   universal on ESC/POS, so the printer is not the constraint; the framework's ability to
///   *produce* a raster is, and a box with no font installed can produce none (ADR-0102).
/// - `kicks_drawer` — what the console says, `drawer_attached`
///   ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)),
///   because nothing a printer reports says so. The port adds the USB rule on top, so a mark on a
///   network printer opens nothing.
///
/// Public because a printer's agent needs the capabilities the **edge** assumed, not its own: the
/// document it receives was prepared under these, so `prints_bitmaps` disagreeing would make an
/// agent refuse a raster the edge had just drawn for it (ADR-0112).
#[must_use]
pub fn assumed_capabilities(device: &PublishedDevice, can_rasterise: bool) -> PrinterCapabilities {
    let paper = device.paper_width();
    PrinterCapabilities {
        connection: connection_of(device),
        code_page: CodePage::Ascii,
        columns: NonZeroU16::new(paper.columns()).unwrap_or(NonZeroU16::MIN),
        dots_per_line: NonZeroU16::new(paper.dots_per_line()).unwrap_or(NonZeroU16::MIN),
        prints_bitmaps: can_rasterise,
        cuts_paper: device.cuts_paper(),
        kicks_drawer: device.drawer_attached,
    }
}

/// The store's printers, and the dispatch that puts a document on one.
///
/// Holds one [`EscPosPrinter`] per device for the life of the process, which is what keeps a socket
/// open between receipts and what makes the adapter's idempotency set mean anything: a retry of the
/// same `job_id` prints once.
pub struct Printers {
    transports: Arc<dyn TransportFactory>,
    open: Mutex<HashMap<DeviceId, Held>>,
    /// The renderer that turns a line no code page covers into a raster, or `None` on a box with no
    /// font installed — which prints ASCII and refuses the rest, the behaviour before ADR-0102.
    renderer: Option<TextRenderer>,
    /// This box's own font size, `config.toml`'s deprecated `font_size_dots`, which applies while
    /// the store's configuration sets none (ADR-0160 decision 6).
    local_font_size: Option<NonZeroU16>,
    /// The font size last said and where it came from, so the log says it at start-up and again
    /// only when it changes.
    font_size_said: Mutex<Option<(NonZeroU16, ValueSource)>>,
    /// Where a job goes when its printer names an agent (ADR-0112), or `None` in a composition that
    /// has no queue — a route test, the fakes-backed example. A printer that names an agent then
    /// reports [`PrintOutcome::AgentUnavailable`], which is the truth for that composition rather
    /// than a silent no-op.
    agents: Option<Arc<dyn AgentDispatch>>,
    /// The agents whose last claim said they carry a kick
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6), at most
    /// [`KICKING_AGENTS`]. In-process, like a claim's parking: every claim says it again, so a
    /// restart that forgets it is told again within one claim.
    kicking: Mutex<HashSet<DeviceId>>,
    /// The kicks a till is waiting on an agent to acknowledge, at most [`KICKS_AWAITED`].
    awaited: Mutex<HashMap<EventId, oneshot::Sender<()>>>,
}

/// How many agents [`Printers`] remembers as carrying a kick: far more terminals than a store has.
/// A full record forgets one, which says so again at its next claim.
const KICKING_AGENTS: usize = 64;

/// How many kicks may wait on a print agent's acknowledgement at once. A till waits on one at a
/// time, so this is a store with dozens of tills kicking together; past it a kick is refused.
const KICKS_AWAITED: usize = 32;

/// One printer held open for the life of the process.
type HeldPrinter = Arc<EscPosPrinter<Box<dyn Transport>>>;

/// A held printer and what it was opened as.
///
/// Kept beside it because the console can change a device while the store trades: a drawer marked
/// after the printer was first opened, an address corrected. A printer held under the old facts is
/// opened again under the new ones on its next use, rather than addressed as it used to be until the
/// box restarts. The price is a fresh idempotency set for that printer: a job retried across a change
/// of its device prints again.
struct Held {
    printer: HeldPrinter,
    address: String,
    capabilities: PrinterCapabilities,
}

impl fmt::Debug for Printers {
    /// How many printers are held and where the channels come from — never a document and never a
    /// printer's contents, because a print document may carry a buyer's name and tax code
    /// (`pos_ports::printer`). Hand-written because `EscPosPrinter<Box<dyn Transport>>` is not
    /// `Debug`, its transport being a trait object.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let held = self.open.lock().map_or(0, |open| open.len());
        f.debug_struct("Printers")
            .field("transports", &self.transports)
            .field("open", &held)
            // Whether fonts are loaded, not which — the list is long and says nothing an operator
            // reading a log needs. `load_fonts` logs the faces and the script coverage at boot.
            .field("can_rasterise", &self.renderer.is_some())
            .field("local_font_size", &self.local_font_size)
            .field("font_size_said", &self.font_size_said)
            // Whether an agent lane is composed, not what is on it: a queue holds rendered
            // documents.
            .field("has_agent_lane", &self.agents.is_some())
            .field(
                "kicking",
                &self.kicking.lock().map_or(0, |kicking| kicking.len()),
            )
            .field(
                "awaited",
                &self.awaited.lock().map_or(0, |awaited| awaited.len()),
            )
            .finish()
    }
}

impl Printers {
    /// The dispatcher a store runs: network printers over raw TCP.
    #[must_use]
    pub fn tcp() -> Self {
        Self::over(Arc::new(TcpTransports))
    }

    /// A dispatcher over a substituted transport factory, for a test.
    #[must_use]
    pub fn over(transports: Arc<dyn TransportFactory>) -> Self {
        Self {
            transports,
            open: Mutex::new(HashMap::new()),
            renderer: None,
            local_font_size: None,
            font_size_said: Mutex::new(None),
            agents: None,
            kicking: Mutex::new(HashSet::new()),
            awaited: Mutex::new(HashMap::new()),
        }
    }

    /// Gives this dispatcher the queue a printer's named agent claims from (ADR-0112).
    ///
    /// Without it a printer that names an agent reports [`PrintOutcome::AgentUnavailable`]: there is
    /// nowhere for the job to go, and saying so beats opening the address the agent was supposed to
    /// own — which in a hosted edge placement is a device path that is not on this machine.
    #[must_use]
    pub fn with_agents(mut self, agents: Arc<dyn AgentDispatch>) -> Self {
        self.agents = Some(agents);
        self
    }

    /// Gives this dispatcher the fonts to rasterise with.
    ///
    /// Without it, a line outside the printer's code page is [`PrintOutcome::Unprintable`] — which
    /// for a Vietnamese, Japanese or Indic menu means every ticket. With it, such a line is drawn
    /// and sent as a raster instead (ADR-0102).
    #[must_use]
    pub fn with_fonts(mut self, renderer: TextRenderer) -> Self {
        self.renderer = Some(renderer);
        self
    }

    /// Gives this dispatcher this box's own font size: `config.toml`'s deprecated `font_size_dots`,
    /// which applies while the store's `printing.font_size_dots` sets none (ADR-0160 decision 6).
    /// `None` or `0` leaves the renderer's own size, 24.
    #[must_use]
    pub fn with_local_font_size(mut self, dots: Option<u16>) -> Self {
        self.local_font_size = dots.and_then(NonZeroU16::new);
        self
    }

    /// Whether this dispatcher can turn text into a raster.
    #[must_use]
    pub const fn can_rasterise(&self) -> bool {
        self.renderer.is_some()
    }

    /// The size a rasterised line is drawn at now, and where it comes from: the store's
    /// `printing.font_size_dots` when its configuration sets one within the bounds, else this box's
    /// own, else the size the fonts were loaded at, 24 (ADR-0160 decision 6).
    fn font_size(
        &self,
        session: &EdgeSession,
        renderer: &TextRenderer,
    ) -> (NonZeroU16, ValueSource) {
        if let Some(dots) = session.printing.font_size_dots().and_then(NonZeroU16::new) {
            (dots, ValueSource::Published)
        } else if let Some(dots) = self.local_font_size {
            (dots, ValueSource::LocalFile)
        } else {
            (renderer.size(), ValueSource::Default)
        }
    }

    /// The size rasterised lines are drawn at now ([`Self::font_size`]), which the log says the
    /// first time and again whenever it changes.
    fn font_size_now(&self, session: &EdgeSession, renderer: &TextRenderer) -> NonZeroU16 {
        let (size, source) = self.font_size(session, renderer);
        let changed = {
            let mut said = self
                .font_size_said
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = *said != Some((size, source));
            *said = Some((size, source));
            changed
        };
        if changed {
            log_in_force(
                "printing.font_size_dots",
                "font_size_dots",
                u64::from(size.get()),
                source,
                self.local_font_size.is_some(),
            );
        }
        size
    }

    /// Logs the size rasterised lines are drawn at and where it comes from, at start-up. Nothing on
    /// a box with no fonts, which rasterises nothing.
    pub fn say_font_size(&self, session: &EdgeSession) {
        if let Some(renderer) = self.renderer.as_ref() {
            let _said = self.font_size_now(session, renderer);
        }
    }

    /// The words a shift report is printed in: the store's display language when this box can draw
    /// it, English when it cannot, so a label is never the line that stops a document printing.
    fn labels_for(&self, session: &EdgeSession) -> &'static PaperLabels {
        PaperLabels::for_store(session.display_language.as_deref(), self.can_rasterise())
    }

    /// The words the paper a guest is handed — a receipt, its copy, a pre-bill — is printed in for
    /// `till`: the receipt's language ([`receipt_language`]), with the same fallback to English.
    fn receipt_labels_for(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
    ) -> &'static PaperLabels {
        PaperLabels::for_store(
            receipt_language(session, till).as_deref(),
            self.can_rasterise(),
        )
    }

    /// The second language `till`'s guest's paper prints in on `device`
    /// ([`SecondLanguage::for_receipt`]), as wide as this build takes the printer to be.
    fn second_language_for(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        device: &PublishedDevice,
        captured: &[ReceiptLine],
        printed: &[ReceiptLine],
        totals: &BillTotals,
    ) -> Option<SecondLanguage> {
        SecondLanguage::for_receipt(
            session,
            till,
            self.receipt_labels_for(session, till),
            self.can_rasterise(),
            assumed_capabilities(device, self.can_rasterise())
                .columns
                .get(),
            captured,
            printed,
            totals,
        )
    }

    /// The paper choices of the till `device` is
    /// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 4): what the `TERMINAL` entry it is bound to names, through the print-agent binding
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
    ///
    /// [`TillPrinting::STORE`] when no terminal in the store's `devices` node names anything of its
    /// own, without reading the binding at all, so a store that names nothing prints as before
    /// whatever the binding record holds. The same when `device` is bound to no terminal, when its
    /// terminal is not in the node, and in a composition with no binding record. A binding that
    /// cannot be read while a terminal does name its own is a till nobody can tell apart, whose
    /// paper is refused: printed at the store's receipt printer, the bar's bill would come out at
    /// the counter.
    pub async fn till_for(&self, session: &EdgeSession, device: DeviceId) -> TillPrinting {
        if !a_till_prints_its_own(&session.devices) {
            return TillPrinting::STORE;
        }
        let Some(lane) = self.agents.as_ref() else {
            return TillPrinting::STORE;
        };
        match lane.terminal_of(device).await {
            Ok(terminal) => terminal
                .and_then(|terminal| {
                    published_terminals(&session.devices).find(|entry| entry.device_id == terminal)
                })
                .map_or(TillPrinting::STORE, TillPrinting::of),
            Err(error) => {
                tracing::warn!(%device, %error, "the terminal a device is bound to could not be read");
                TillPrinting::UNKNOWN
            }
        }
    }

    /// Opens the store's cash drawer through the printer [`drawer_printer`] picks
    /// ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 4).
    ///
    /// The caller asks only after the act the drawer is for has committed: a cash payment, a paid in
    /// or out, a no-sale opening. Never an error, for the reason a receipt is never one: the money is
    /// already recorded, and a drawer that did not spring is opened with its key. The outcome says
    /// which, and the till tells the cashier.
    ///
    /// Behind a print agent that carries the kick, the kick is a job on its queue under `kick_id`,
    /// and the till waits up to [`KICK_WAIT`] for the agent to say it wrote it
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6). A fresh
    /// id, as a test page's is: a cash payment's receipt already prints under the payment's.
    pub async fn open_drawer(
        &self,
        session: &EdgeSession,
        store_id: StoreId,
        kick_id: EventId,
    ) -> DrawerOutcome {
        let Some(device) = drawer_printer(&session.devices, |agent| self.carries_kick(agent))
        else {
            if session
                .devices
                .devices()
                .iter()
                .any(|device| device.drawer_attached)
            {
                tracing::warn!(
                    "a printer is marked as having a cash drawer, but not one this edge may open: it \
                     must serve the bill, be on USB, and print without an agent or through one that \
                     carries the kick"
                );
            }
            return DrawerOutcome::NoDrawer;
        };
        if let Some(agent) = device.agent_device_id {
            return self
                .kick_through(agent, device.device_id, store_id, kick_id)
                .await;
        }
        let capabilities = assumed_capabilities(device, self.can_rasterise());
        let printer = match self.printer_for(device, capabilities) {
            Ok(printer) => printer,
            Err(error) => {
                tracing::warn!(device = %device.device_id, %error, "no channel to the drawer's printer");
                return DrawerOutcome::Unavailable;
            }
        };
        // On a blocking thread for the reason a receipt is: an unplugged printer blocks on its write.
        match tokio::task::spawn_blocking(move || printer.open_drawer_blocking()).await {
            Ok(Ok(())) => {
                tracing::info!(device = %device.device_id, "cash drawer kicked");
                DrawerOutcome::Opened
            }
            Ok(Err(error)) => {
                tracing::warn!(device = %device.device_id, %error, "the drawer's printer refused the kick");
                DrawerOutcome::Unavailable
            }
            Err(_) => {
                tracing::warn!(device = %device.device_id, "the drawer thread did not finish");
                DrawerOutcome::Unavailable
            }
        }
    }

    /// Opens the drawer through the print agent that owns its printer: a kick on the agent's
    /// queue, answered `Opened` once the agent acknowledges it within [`KICK_WAIT`].
    async fn kick_through(
        &self,
        agent: DeviceId,
        printer: DeviceId,
        store_id: StoreId,
        job_id: EventId,
    ) -> DrawerOutcome {
        let Some(lane) = self.agents.as_ref() else {
            tracing::warn!(%printer, %agent, "the drawer's printer names an agent but no print queue is composed");
            return DrawerOutcome::Unavailable;
        };
        // Waited on before it is offered, so an acknowledgement that beats the offer is heard.
        let Some(acknowledged) = self.await_kick(job_id) else {
            tracing::warn!(%printer, %agent, "too many kicks are waiting on print agents; this one is not sent");
            return DrawerOutcome::Unavailable;
        };
        let kick = PrintJob {
            job_id,
            store_id,
            station_id: None,
            document: PrintDocument::kick(),
        };
        if lane.kick(agent, printer, kick).await != PrintOutcome::QueuedToAgent {
            return DrawerOutcome::Unavailable;
        }
        if acknowledged.within(KICK_WAIT).await {
            tracing::info!(%job_id, %printer, %agent, "cash drawer kicked through its print agent");
            DrawerOutcome::Opened
        } else {
            tracing::warn!(%job_id, %printer, %agent, "the print agent did not acknowledge the kick in time");
            DrawerOutcome::Unavailable
        }
    }

    /// Records what a print agent's claim said about it: whether it carries a kick. The claim
    /// route calls this once the binding has said which terminal the caller is.
    pub(crate) fn heard_from_agent(&self, agent: DeviceId, carries_kick: bool) {
        let mut kicking = self
            .kicking
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !carries_kick {
            kicking.remove(&agent);
            return;
        }
        if !kicking.contains(&agent)
            && kicking.len() >= KICKING_AGENTS
            && let Some(forgotten) = kicking.iter().next().copied()
        {
            kicking.remove(&forgotten);
        }
        kicking.insert(agent);
    }

    /// Whether `agent`'s last claim said it carries a kick.
    fn carries_kick(&self, agent: DeviceId) -> bool {
        self.kicking
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&agent)
    }

    /// An agent acknowledged `job`: a till waiting on it as a kick hears so. The acknowledgement
    /// route calls this for a job it deleted from the queue.
    pub(crate) fn acknowledged(&self, job: EventId) {
        let waiting = self
            .awaited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&job);
        if let Some(waiting) = waiting {
            // The till may have stopped waiting a moment ago, and nothing else is listening.
            let _ = waiting.send(());
        }
    }

    /// Starts waiting on `job`'s acknowledgement, or `None` when [`KICKS_AWAITED`] are already.
    fn await_kick(&self, job: EventId) -> Option<AwaitedKick<'_>> {
        let mut awaited = self
            .awaited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if awaited.len() >= KICKS_AWAITED {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        awaited.insert(job, sender);
        Some(AwaitedKick {
            awaited: &self.awaited,
            job,
            receiver,
        })
    }

    /// Prints the guest's receipt for a settled bill, on `till`'s receipt printer: the store's,
    /// unless the till's terminal names its own that the node lists as a printer serving no
    /// station.
    ///
    /// `job_id` is the idempotency key: pass the settle's event id and a retried settle reprints
    /// nothing. Never returns an error — a printer that is down must not roll back a bill the guest
    /// has already paid, so the outcome is reported and the caller decides what to tell the cashier.
    #[expect(
        clippy::too_many_arguments,
        reason = "the receipt's own arguments plus the till it is printed for; a struct for them \
                  would be built at one call site and taken apart here"
    )]
    pub async fn print_receipt(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        store_id: StoreId,
        job_id: EventId,
        receipt_number: u64,
        lines: &[ReceiptLine],
        totals: &BillTotals,
        buyer: Option<&BuyerDetails>,
    ) -> PrintOutcome {
        let device = match guest_printer(&session.devices, till) {
            Ok(device) => device,
            Err(outcome) => return outcome,
        };
        // The store's own identity, from the `store_profile` node the config pull applies
        // (ADR-0106). Empty until somebody fills it in, and the document is then what it was before.
        let printed = receipt_lines_in(session, till, lines);
        let totals = receipt_totals_in(session, till, totals);
        let second = self.second_language_for(session, till, device, lines, &printed, &totals);
        let document = receipt_document(
            &session.profile,
            &MoneyStyle::of(session),
            self.receipt_labels_for(session, till),
            receipt_number,
            &printed,
            &totals,
            buyer,
            second.as_ref(),
        );
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: None,
                document,
            },
        )
        .await
    }

    /// Prints a copy of a settled bill's receipt on `till`'s receipt printer, as the receipt is
    /// ([ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)),
    /// marked as a copy with the store's local time it was printed at.
    ///
    /// The job is keyed on the copy's own event ([`ReceiptCopy::job_id`]), so a job retried after an
    /// ambiguous failure prints once and the next press, which is a new copy, prints again.
    pub async fn print_receipt_copy(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        store_id: StoreId,
        copy: &ReceiptCopy,
        buyer: Option<&BuyerDetails>,
    ) -> PrintOutcome {
        let device = match guest_printer(&session.devices, till) {
            Ok(device) => device,
            Err(outcome) => return outcome,
        };
        // An instant always reads as a local time; the empty fallback is for a timestamp outside
        // what the calendar can represent, which a copy printed now cannot have.
        let reprinted = local_time(copy.reprinted_time, &session.timezone)
            .map(|time| time.to_string())
            .unwrap_or_default();
        let printed = receipt_lines_in(session, till, &copy.lines);
        let totals = receipt_totals_in(session, till, &copy.totals);
        let second =
            self.second_language_for(session, till, device, &copy.lines, &printed, &totals);
        let document = receipt_copy_document(
            &session.profile,
            &MoneyStyle::of(session),
            self.receipt_labels_for(session, till),
            copy.receipt_number,
            copy.copy_number,
            &reprinted,
            &printed,
            &totals,
            buyer,
            second.as_ref(),
        );
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id: copy.job_id,
                store_id,
                station_id: None,
                document,
            },
        )
        .await
    }

    /// Prints a pre-bill on `till`'s receipt printer — the same device its receipt goes to, since
    /// both are handed to the guest.
    ///
    /// `job_id` should be fresh for each press: a table that asks twice gets two pieces of paper, and
    /// unlike a settle there is nothing to deduplicate against.
    pub async fn print_pre_bill(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        store_id: StoreId,
        job_id: EventId,
        reference: &str,
        pre_bill: &PreBill,
    ) -> PrintOutcome {
        let device = match guest_printer(&session.devices, till) {
            Ok(device) => device,
            Err(outcome) => return outcome,
        };
        let printed = receipt_lines_in(session, till, &pre_bill.lines);
        let totals = receipt_totals_in(session, till, &pre_bill.totals);
        let second =
            self.second_language_for(session, till, device, &pre_bill.lines, &printed, &totals);
        let document = pre_bill_document(
            &session.profile,
            &MoneyStyle::of(session),
            self.receipt_labels_for(session, till),
            reference,
            &printed,
            &totals,
            second.as_ref(),
        );
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: None,
                document,
            },
        )
        .await
    }

    /// Prints a closed shift's report on the store's receipt printer.
    ///
    /// Never returns an error, for the reason [`Self::print_receipt`] gives: the shift is closed
    /// whether the paper comes out or not, and the caller tells the cashier which.
    pub async fn print_shift_report(
        &self,
        session: &EdgeSession,
        store_id: StoreId,
        job_id: EventId,
        report: &ShiftReport,
    ) -> PrintOutcome {
        let Some(device) = receipt_printer(&session.devices) else {
            tracing::info!("no receipt printer is published for this store; nothing to print");
            return PrintOutcome::NoPrinter;
        };
        let document = shift_report_document(
            &session.profile,
            &MoneyStyle::of(session),
            self.labels_for(session),
            report,
        );
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: None,
                document,
            },
        )
        .await
    }

    /// Prints the kitchen ticket for `fired` at its station, or at the station plan's declared
    /// backup.
    ///
    /// The failover is the plan's (ADR-0072, ADR-0100): one hop, to the station an operator named.
    /// The ticket is in the language of the station whose printer prints it ([`ticket_at`]), the
    /// backup's on a failover, because its cooks read it.
    ///
    /// `note` is the guest note the edge holds for the line, if it still holds one (ADR-0157).
    pub async fn print_ticket(
        &self,
        session: &EdgeSession,
        store_id: StoreId,
        job_id: EventId,
        reference: &str,
        fired: &FiredLine,
        note: Option<&NoteText>,
    ) -> PrintOutcome {
        let Some((printing, device)) =
            station_printer(&session.devices, &session.stations, fired.station_id)
        else {
            tracing::info!(
                "no printer is published for that station or its backup; the kitchen display still has the order"
            );
            return PrintOutcome::NoPrinter;
        };
        let document = ticket_at(
            session,
            self.can_rasterise(),
            printing,
            reference,
            fired,
            note,
        );
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: Some(fired.station_id),
                document,
            },
        )
        .await
    }

    /// Prints [`test_page_document`] on the printer `device_id`, through the same dispatch a receipt
    /// takes — direct or through its agent (ADR-0112) — so a page that prints proves the path a
    /// receipt will take.
    ///
    /// [`PrintOutcome::NoPrinter`] for an id this store has not published as a printer.
    pub async fn print_test_page(
        &self,
        session: &EdgeSession,
        store_id: StoreId,
        job_id: EventId,
        device_id: DeviceId,
        printed_at: &str,
    ) -> PrintOutcome {
        let Some(device) = published_printers(&session.devices)
            .into_iter()
            .find(|device| device.device_id == device_id)
        else {
            return PrintOutcome::NoPrinter;
        };
        let document =
            test_page_document(&session.profile, device, printed_at, self.can_rasterise());
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: device.station_id,
                document,
            },
        )
        .await
    }

    /// Turns every line the printer's own character set cannot carry into a raster, as wide as its
    /// paper, and leaves out the cut where the printer has no cutter.
    ///
    /// The framework's half of the port's contract (ADR-0026 §5): the adapter sends `Text` as text,
    /// so deciding a line is sendable is the caller's job, and getting it wrong prints a row of
    /// question marks in front of a customer. Before ADR-0102 the only answer available here was to
    /// refuse the whole document; now the line is drawn, at the size in force for `session`
    /// ([`Self::font_size`]), so a change to it applies from the next print.
    ///
    /// # Errors
    ///
    /// The count of characters that could not be rendered at all — no code page, and no font loaded
    /// to draw them with. The caller reports [`PrintOutcome::Unprintable`], which is still better
    /// than sending bytes the printer will mangle.
    fn prepare(
        &self,
        session: &EdgeSession,
        capabilities: &PrinterCapabilities,
        document: PrintDocument,
    ) -> Result<PrintDocument, usize> {
        let size = self
            .renderer
            .as_ref()
            .map(|renderer| self.font_size_now(session, renderer));
        let mut blocks = Vec::with_capacity(document.blocks.len());
        let mut refused = 0_usize;
        for block in document.blocks {
            // A printer with no cutter is sent no cut (ADR-0160): its operator tears the paper.
            if matches!(block, PrintBlock::Cut) && !capabilities.cuts_paper {
                continue;
            }
            let PrintBlock::Text { line, style } = &block else {
                blocks.push(block);
                continue;
            };
            // The printer's own font covers it: text is a fraction of the bytes and comes out of the
            // head faster, so an ASCII receipt still goes as text after this change.
            if capabilities.needs_bitmap(line) == Ok(false) {
                blocks.push(block);
                continue;
            }
            let (Some(renderer), Some(size)) = (self.renderer.as_ref(), size) else {
                refused = refused.saturating_add(line.chars().count());
                continue;
            };
            match renderer.render_at(line, *style, capabilities.dots_per_line, size) {
                Ok(drawn) => {
                    if !drawn.substituted.is_empty() {
                        // The characters, not the line: which glyphs are missing is what an operator
                        // needs to install a font for.
                        //
                        // Truncated at `MISSING_GLYPHS_REPORTED`, because the *whole* set is not the
                        // harmless thing the sentence above assumed. A receipt block carries a
                        // buyer's name (`pos_ports::printer`); on a box with no CJK or Vietnamese
                        // face, every character of that name substitutes, and a set of them in a
                        // durable log reconstructs most of it (ADR-0117 decision 7). A handful names
                        // the script and the font to install, which is the whole operational use.
                        let missing = &drawn.substituted
                            [..drawn.substituted.len().min(MISSING_GLYPHS_REPORTED)];
                        tracing::warn!(
                            missing = ?missing,
                            missing_count = drawn.substituted.len(),
                            "no installed font covers these characters; they printed as boxes"
                        );
                    }
                    blocks.push(drawn.bitmap.into_block());
                }
                Err(error) => {
                    tracing::warn!(%error, "a line could not be rendered");
                    refused = refused.saturating_add(line.chars().count());
                }
            }
        }
        if refused > 0 {
            return Err(refused);
        }
        Ok(PrintDocument { blocks })
    }

    /// Sends one job to one device — down the wire itself, or onto the queue of the agent that
    /// owns that device's transport (ADR-0112).
    async fn dispatch(
        &self,
        session: &EdgeSession,
        device: &PublishedDevice,
        mut job: PrintJob,
    ) -> PrintOutcome {
        let capabilities = assumed_capabilities(device, self.can_rasterise());
        // Rendered **before** the branch, deliberately. The agent receives a finished document and
        // decides nothing about it, so the rasterising, the code-page decision, the width and the
        // font size all happen here whichever way the bytes leave. Moving any of it across would
        // mean a store with three terminals printing three different tickets from one order.
        match self.prepare(session, &capabilities, job.document) {
            Ok(document) => job.document = document,
            Err(line) => {
                // Never the line's text: a document may carry a buyer's name and tax code
                // (`pos_ports::printer`).
                tracing::warn!(
                    device = %device.device_id,
                    characters = line,
                    "the document needs characters this printer cannot render as text and no font \
                     is loaded to rasterise them"
                );
                return PrintOutcome::Unprintable;
            }
        }
        // A printer that names an agent is reached through the queue and never through a socket
        // this process opens. Absent — every printer in every store that has not configured one —
        // falls through to exactly the path ADR-0103 shipped.
        if let Some(agent) = device.agent_device_id {
            let Some(lane) = self.agents.as_ref() else {
                tracing::warn!(
                    device = %device.device_id,
                    %agent,
                    "this printer names an agent but no print queue is composed"
                );
                return PrintOutcome::AgentUnavailable;
            };
            return lane.enqueue(agent, device.device_id, job).await;
        }
        let printer = match self.printer_for(device, capabilities) {
            Ok(printer) => printer,
            Err(error) => {
                tracing::warn!(device = %device.device_id, %error, "no channel to that printer");
                return PrintOutcome::Unavailable;
            }
        };
        let job_id = job.job_id;
        // A printer that has been unplugged blocks on a socket timeout, so the write goes on a
        // blocking thread rather than an async worker: the till must stay responsive while the
        // receipt fails.
        let outcome = tokio::task::spawn_blocking(move || printer.print_blocking(&job)).await;
        match outcome {
            Ok(Ok(())) => {
                // The job's identifier and its outcome, never its content (`pos_ports::printer`).
                tracing::info!(%job_id, device = %device.device_id, "printed");
                PrintOutcome::Printed
            }
            Ok(Err(error)) => {
                tracing::warn!(%job_id, device = %device.device_id, %error, "the printer refused the job");
                PrintOutcome::Unavailable
            }
            Err(_) => {
                tracing::warn!(%job_id, device = %device.device_id, "the print thread did not finish");
                PrintOutcome::Unavailable
            }
        }
    }

    /// The held printer for `device`, opening a channel the first time and again whenever the
    /// device's address or its assumed capabilities have changed since ([`Held`]).
    fn printer_for(
        &self,
        device: &PublishedDevice,
        capabilities: PrinterCapabilities,
    ) -> Result<HeldPrinter, PortError> {
        let mut open = self.open.lock().map_err(|_| {
            PortError::internal(
                PortName::PrinterDriver,
                "the printer registry lock was poisoned",
            )
        })?;
        if let Some(held) = open.get(&device.device_id)
            && held.address == device.address
            && held.capabilities == capabilities
        {
            return Ok(Arc::clone(&held.printer));
        }
        let transport = self.transports.open(device)?;
        let printer = Arc::new(EscPosPrinter::new(capabilities.clone(), transport));
        open.insert(
            device.device_id,
            Held {
                printer: Arc::clone(&printer),
                address: device.address.clone(),
                capabilities,
            },
        );
        Ok(printer)
    }
}

#[cfg(test)]
mod tests {
    use super::TcpTransports;
    use super::{
        AgentLane, DrawerOutcome, KICKING_AGENTS, KICKS_AWAITED, MoneyStyle, PrintOutcome,
        Printers, SecondLanguage, SecondNames, TicketLine, TicketNote, TillPrinting,
        TransportFactory, assumed_capabilities, connection_of, drawer_printer, pre_bill_document,
        receipt_copy_document, receipt_document, receipt_language, receipt_lines_in,
        receipt_printer, receipt_totals_in, shift_report_document, short_reference,
        station_printer, ticket_at, ticket_document, ticket_language, ticket_line,
    };
    use crate::app::{BuyerDetails, EdgeSession, FiredLine, ReceiptLine, ShiftReport};
    use crate::config::ValueSource;
    use crate::paper_labels::{ENGLISH, PaperLabels, VIETNAMESE};
    use pos_core::billing::{BillTotals, FeeLine};
    use pos_ports::printer::{PrintBlock, PrintJob, PrinterConnection};
    use pos_ports::{PortError, PortName};
    use pos_proto::ClockSource as _;
    use pos_proto::devices::{
        DeviceConnection, DeviceKind, PaperWidth, PublishedDevice, PublishedDevices,
    };
    use pos_proto::fees::{FeeCode, PublishedFees};
    use pos_proto::floor::{KitchenStation, StationPlan};
    use pos_proto::ids::{DeviceId, FeeId, MenuItemId, StationId};
    use pos_proto::locale::NumberFormat;
    use pos_proto::menu::{MenuCatalog, MenuEntry};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::printing::{PublishedPrinting, ReceiptLanguage, ReceiptSecondLanguage};
    use pos_proto::quantity::Quantity;
    use pos_proto::store_profile::StoreProfile;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;
    use printer_escpos::{Transport, TransportStatus, Unreachable};
    use std::num::NonZeroU16;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    /// The store's own choices, which a till whose terminal names nothing prints with.
    const STORE: TillPrinting = TillPrinting::STORE;

    /// A settled bill's totals, for the receipt tests: one 10 % tax line and nothing else.
    /// The money style a test store writes with: `exponent` decimals and the common marks.
    ///
    /// The marks are `NumberFormat::default()` — `1,234.50` — for every fixture but the Vietnamese
    /// one below, which states its own. That keeps each existing expectation meaning what it meant
    /// before ADR-0136, so a test that changed is a test whose *country* changed.
    fn style(exponent: u8) -> MoneyStyle {
        MoneyStyle {
            exponent,
            format: NumberFormat::default(),
        }
    }

    fn totals_of(total: Money) -> BillTotals {
        let zero = Money::zero(total.currency_code);
        BillTotals {
            subtotal: total,
            discount_total: zero,
            comp_total: zero,
            service_charge: zero,
            fee_lines: Vec::new(),
            tax_lines: Vec::new(),
            tax_total: zero,
            rounding_adjustment: zero,
            total_due: total,
        }
    }

    fn lines_of(document: &pos_ports::printer::PrintDocument) -> Vec<&str> {
        document
            .blocks
            .iter()
            .filter_map(|block| match block {
                PrintBlock::Text { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect()
    }

    fn station(seed: u128) -> StationId {
        StationId::new(Ulid::from_u128(seed))
    }

    fn device(seed: u128, kind: DeviceKind, at: Option<StationId>) -> PublishedDevice {
        PublishedDevice {
            device_id: DeviceId::new(Ulid::from_u128(seed)),
            kind: kind.into(),
            connection: DeviceConnection::Network.into(),
            address: format!("192.0.2.{seed}:9100"),
            name: DisplayName::new("Printer"),
            station_id: at,
            // Absent is the in-store case: the edge opens the address itself, which is what every
            // store that configures no agent still does (ADR-0112).
            agent_device_id: None,
            drawer_attached: false,
            paper_width: Open::default(),
            cuts_paper: None,
            receipt_printer_id: None,
            receipt_language: Open::default(),
            receipt_second_language: Open::default(),
            opening_float_minor: None,
        }
    }

    /// The counter's printer on a USB cable, marked as having a cash drawer (ADR-0165).
    fn drawer_device(seed: u128) -> PublishedDevice {
        PublishedDevice {
            connection: DeviceConnection::Usb.into(),
            address: "/dev/usb/lp0".to_owned(),
            drawer_attached: true,
            ..device(seed, DeviceKind::Printer, None)
        }
    }

    /// The same printer, but with its transport owned by `agent` (ADR-0112).
    fn device_via_agent(seed: u128, at: Option<StationId>, agent: DeviceId) -> PublishedDevice {
        PublishedDevice {
            agent_device_id: Some(agent),
            ..device(seed, DeviceKind::Printer, at)
        }
    }

    /// A transport that records what was written, standing in for a printer on the LAN.
    #[derive(Debug, Default)]
    struct Recorder {
        written: Mutex<Vec<Vec<u8>>>,
        /// Whether the printer answers at all. `false` is an unplugged printer.
        reachable: bool,
    }

    #[derive(Debug)]
    struct Recorders {
        recorder: Arc<Recorder>,
    }

    /// A handle to the shared recorder. A newtype because `Transport` and `Arc` are both foreign.
    #[derive(Debug)]
    struct SharedRecorder(Arc<Recorder>);

    impl Transport for SharedRecorder {
        fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
            if !self.0.reachable {
                return Err(Unreachable);
            }
            self.0
                .written
                .lock()
                .map_err(|_| Unreachable)?
                .push(bytes.to_vec());
            Ok(())
        }

        fn probe(&self) -> Result<TransportStatus, Unreachable> {
            if self.0.reachable {
                Ok(TransportStatus::default())
            } else {
                Err(Unreachable)
            }
        }
    }

    impl TransportFactory for Recorders {
        fn open(&self, _device: &PublishedDevice) -> Result<Box<dyn Transport>, PortError> {
            Ok(Box::new(SharedRecorder(Arc::clone(&self.recorder))))
        }
    }

    fn recorder(reachable: bool) -> (Printers, Arc<Recorder>) {
        let recorder = Arc::new(Recorder {
            written: Mutex::new(Vec::new()),
            reachable,
        });
        let printers = Printers::over(Arc::new(Recorders {
            recorder: Arc::clone(&recorder),
        }));
        (printers, recorder)
    }

    fn session_with(devices: PublishedDevices) -> EdgeSession {
        EdgeSession {
            devices,
            ..EdgeSession::bootstrap()
        }
    }

    fn event_id(seed: u128) -> pos_proto::ids::EventId {
        pos_proto::ids::EventId::new(Ulid::from_u128(seed))
    }

    fn store_id() -> pos_proto::ids::StoreId {
        pos_proto::ids::StoreId::new(Ulid::from_u128(0x0051_5111))
    }

    #[tokio::test]
    async fn a_settled_bill_reaches_the_receipt_printer() {
        let (printers, recorder) = recorder(true);
        let session = session_with(PublishedDevices::new(vec![device(
            2,
            DeviceKind::Printer,
            None,
        )]));

        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(1),
                42,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 99_000)),
                None,
            )
            .await;

        assert_eq!(outcome, PrintOutcome::Printed);
        let written = recorder.written.lock().expect("the recorder");
        assert_eq!(written.len(), 1, "one receipt, one write");
        let bytes = written.first().expect("the receipt");
        assert!(
            bytes.starts_with(&printer_escpos::escpos::INIT),
            "a document begins with the initialise command"
        );
        assert!(
            bytes.windows(3).any(|window| window == b"#42"),
            "the receipt number is on the paper"
        );
    }

    #[tokio::test]
    async fn a_store_with_no_printer_says_so_rather_than_claiming_it_printed() {
        // The bug this slice exists to close: the till has rendered "Printing receipt…" over exactly
        // this state since P5.
        let (printers, _recorder) = recorder(true);
        let session = session_with(PublishedDevices::new(Vec::new()));

        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(1),
                42,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 99_000)),
                None,
            )
            .await;

        assert_eq!(outcome, PrintOutcome::NoPrinter);
        assert!(!outcome.printed());
        assert_eq!(outcome.as_wire(), "NO_PRINTER");
    }

    #[tokio::test]
    async fn a_printer_that_does_not_answer_is_reported_and_does_not_fail_the_settle() {
        // The guest has already paid. A printer that is down is news for the cashier, not a reason to
        // unwind a settled bill.
        let (printers, _recorder) = recorder(false);
        let session = session_with(PublishedDevices::new(vec![device(
            2,
            DeviceKind::Printer,
            None,
        )]));

        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(1),
                42,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 99_000)),
                None,
            )
            .await;

        assert_eq!(outcome, PrintOutcome::Unavailable);
        assert_eq!(outcome.as_wire(), "PRINTER_UNAVAILABLE");
    }

    #[tokio::test]
    async fn one_settle_retried_prints_one_receipt() {
        // The adapter is idempotent by job id and the dispatcher holds the printer, which is what
        // makes that idempotency reach across two calls.
        let (printers, recorder) = recorder(true);
        let session = session_with(PublishedDevices::new(vec![device(
            2,
            DeviceKind::Printer,
            None,
        )]));
        let total = Money::new(CurrencyCode::VND, 99_000);

        for _ in 0..2 {
            let outcome = printers
                .print_receipt(
                    &session,
                    &STORE,
                    store_id(),
                    event_id(1),
                    42,
                    &[],
                    &totals_of(total),
                    None,
                )
                .await;
            assert_eq!(outcome, PrintOutcome::Printed);
        }

        assert_eq!(
            recorder.written.lock().expect("the recorder").len(),
            1,
            "a retried settle must not hand the guest a second receipt"
        );
    }

    #[tokio::test]
    async fn a_kitchen_ticket_a_printer_cannot_spell_is_refused_when_no_font_is_installed() {
        // The remaining half of the ADR-0102 story, and the one an operator has to be told about: a
        // box with no font package installed can draw nothing, so a Vietnamese item name is still
        // refused. Refusing beats sending the bytes — question marks on a kitchen ticket are how the
        // wrong dish gets made — and the KDS still shows the order. The companion test below is the
        // same ticket on a box that *has* fonts.
        let (printers, recorder) = recorder(true);
        // A station that sets no language: the dish prints as the menu names it.
        let session = a_kitchen(None, ReceiptLanguage::Display, ReceiptLanguage::Display);
        let outcome = printers
            .print_ticket(
                &session,
                store_id(),
                event_id(1),
                "A1",
                &fired_at(oven(), vec![]),
                None,
            )
            .await;

        assert_eq!(outcome, PrintOutcome::Unprintable);
        assert_eq!(outcome.as_wire(), "UNPRINTABLE_TEXT");
        assert!(
            recorder.written.lock().expect("the recorder").is_empty(),
            "nothing goes to the printer when it cannot spell the dish"
        );
    }

    #[test]
    fn a_ticket_leads_with_the_reference_then_the_item_then_its_modifiers() {
        let document = ticket_document(
            "A1",
            &TicketLine {
                item: "Margherita".to_owned(),
                quantity: "2".to_owned(),
                modifiers: vec!["Extra cheese".to_owned()],
                note: TicketNote::None,
            },
            &ENGLISH,
        );
        assert_eq!(
            lines_of(&document),
            vec!["A1", "2 x Margherita", "  + Extra cheese"]
        );
        assert_eq!(document.blocks.last(), Some(&PrintBlock::Cut));
    }

    #[test]
    fn a_ticket_prints_the_guests_note_last_and_emphasised() {
        let document = ticket_document(
            "A1",
            &TicketLine {
                item: "Margherita".to_owned(),
                quantity: "1".to_owned(),
                modifiers: vec!["Extra cheese".to_owned()],
                note: TicketNote::Text("no peanuts".to_owned()),
            },
            &ENGLISH,
        );
        assert_eq!(
            lines_of(&document),
            vec!["A1", "1 x Margherita", "  + Extra cheese", "  ! no peanuts"]
        );
        let note = document
            .blocks
            .iter()
            .find_map(|block| match block {
                PrintBlock::Text { line, style } if line.contains("no peanuts") => Some(style),
                _ => None,
            })
            .expect("the note is printed");
        assert!(
            note.emphasised,
            "a note about somebody's health is not small print"
        );
    }

    #[test]
    fn a_note_the_edge_lost_is_printed_as_lost_in_the_stores_language() {
        let fired = FiredLine {
            station_id: station(1),
            menu_item_id: MenuItemId::new(Ulid::from_u128(11)),
            quantity: Quantity::ONE,
            modifier_menu_item_ids: Vec::new(),
            note_present: true,
        };
        let line = ticket_line(&EdgeSession::bootstrap(), &fired, None, None);
        assert_eq!(line.note, TicketNote::Lost);
        let english = ticket_document("A1", &line, &ENGLISH);
        assert_eq!(
            lines_of(&english).last().copied(),
            Some("  ! A note was written - ask the server")
        );
        let vietnamese = ticket_document("A1", &line, &VIETNAMESE);
        assert_eq!(
            lines_of(&vietnamese).last().copied(),
            Some("  ! Có ghi chú - hỏi nhân viên phục vụ")
        );

        let unnoted = FiredLine {
            note_present: false,
            ..fired
        };
        assert_eq!(
            ticket_line(&EdgeSession::bootstrap(), &unnoted, None, None).note,
            TicketNote::None
        );
    }

    #[test]
    fn a_ticket_takes_its_names_from_the_published_menu_and_falls_back_to_the_id() {
        // A blank line on a kitchen ticket is how the wrong dish gets made; the identifier at least
        // matches what the KDS is showing.
        let margherita = MenuItemId::new(Ulid::from_u128(11));
        let unknown = MenuItemId::new(Ulid::from_u128(12));
        let session = EdgeSession {
            menu: MenuCatalog::default().with(MenuEntry::new(
                margherita,
                DisplayName::new("Margherita"),
                Money::new(CurrencyCode::VND, 150_000),
                EdgeSession::standard_tax_class(),
            )),
            ..EdgeSession::bootstrap()
        };

        let line = ticket_line(
            &session,
            &FiredLine {
                station_id: station(1),
                menu_item_id: margherita,
                quantity: Quantity::from_milli(2_000),
                modifier_menu_item_ids: vec![unknown],
                note_present: false,
            },
            None,
            None,
        );

        assert_eq!(line.item, "Margherita");
        assert_eq!(line.quantity, "2");
        assert_eq!(line.modifiers, vec![unknown.to_string()]);
    }

    #[test]
    fn half_of_a_split_item_reads_as_a_half_and_not_as_five_hundred() {
        // `Quantity` counts thousandths. A ticket saying "500 x Margherita" is not a rounding bug an
        // operator would ever guess at.
        let session = EdgeSession::bootstrap();
        let half = ticket_line(
            &session,
            &FiredLine {
                station_id: station(1),
                menu_item_id: MenuItemId::new(Ulid::from_u128(11)),
                quantity: Quantity::HALF,
                modifier_menu_item_ids: Vec::new(),
                note_present: false,
            },
            None,
            None,
        );
        assert_eq!(half.quantity, "0.5");
    }

    /// The oven, which has a printer of its own.
    fn oven() -> StationId {
        station(1)
    }

    /// The grill, which has no printer and fails over to the oven.
    fn grill() -> StationId {
        station(2)
    }

    /// The menu's grilled pork, its herbs and its cheese.
    fn pork() -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(11))
    }
    fn herbs() -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(12))
    }
    fn cheese() -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(13))
    }

    /// The kitchen the ticket tests print in (ADR-0160 decision 2): the oven, with its printer,
    /// printing in `oven_language`, and the grill, printing in `grill_language`, whose tickets fail
    /// over to the oven's printer. The menu names its dishes in Vietnamese, the store's display
    /// language where `display` says so, and translates the pork and the herbs into English but not
    /// the cheese.
    fn a_kitchen(
        display: Option<&str>,
        oven_language: ReceiptLanguage,
        grill_language: ReceiptLanguage,
    ) -> EdgeSession {
        let dish = |id: MenuItemId, name: &str, english: Option<&str>| {
            MenuEntry::new(
                id,
                DisplayName::new(name),
                Money::new(CurrencyCode::VND, 50_000),
                EdgeSession::standard_tax_class(),
            )
            .with_name_translations(
                english
                    .map(|english| ("en".to_owned(), DisplayName::new(english)))
                    .into_iter()
                    .collect(),
            )
        };
        let kitchen = |id: StationId, name: &str, backup, language| KitchenStation {
            station_id: id,
            name: DisplayName::new(name),
            backup_station_id: backup,
            late_after_seconds: None,
            ticket_language: Open::from_known(language),
        };
        EdgeSession {
            display_language: display.map(str::to_owned),
            menu: MenuCatalog::new()
                .with(dish(pork(), "Bún chả", Some("Grilled pork with noodles")))
                .with(dish(herbs(), "Thêm rau", Some("Extra herbs")))
                .with(dish(cheese(), "Phô mai", None)),
            devices: PublishedDevices::new(vec![device(3, DeviceKind::Printer, Some(oven()))]),
            stations: StationPlan::from_parts(
                vec![
                    kitchen(oven(), "Oven", None, oven_language),
                    kitchen(grill(), "Grill", Some(oven()), grill_language),
                ],
                Vec::new(),
                None,
            ),
            ..EdgeSession::bootstrap()
        }
    }

    /// Two of the grilled pork fired at `at`, with `modifiers`.
    fn fired_at(at: StationId, modifiers: Vec<MenuItemId>) -> FiredLine {
        FiredLine {
            station_id: at,
            menu_item_id: pork(),
            quantity: Quantity::from_milli(2_000),
            modifier_menu_item_ids: modifiers,
            note_present: false,
        }
    }

    #[test]
    fn a_station_in_english_prints_its_ticket_in_english_while_the_display_language_is_vietnamese()
    {
        let session = a_kitchen(
            Some("vi"),
            ReceiptLanguage::English,
            ReceiptLanguage::Display,
        );
        let fired = FiredLine {
            note_present: true,
            ..fired_at(oven(), vec![herbs(), cheese()])
        };

        let english = ticket_at(&session, true, oven(), "A1", &fired, None);
        assert_eq!(
            lines_of(&english),
            vec![
                "A1",
                "2 x Grilled pork with noodles",
                "  + Extra herbs",
                // The menu gives the cheese no English name, so it keeps the one it has.
                "  + Phô mai",
                "  ! A note was written - ask the server",
            ]
        );

        // The same line where a station sets no language is the ticket every station printed.
        let display = ticket_at(&session, true, grill(), "A1", &fired, None);
        assert_eq!(
            lines_of(&display),
            vec![
                "A1",
                "2 x Bún chả",
                "  + Thêm rau",
                "  + Phô mai",
                "  ! Có ghi chú - hỏi nhân viên phục vụ",
            ]
        );
    }

    #[tokio::test]
    async fn a_ticket_that_fails_over_prints_in_the_backup_stations_language() {
        // The grill reads Vietnamese and has no printer, so its ticket goes to the oven's, where the
        // cooks read English. On a box with no fonts a Vietnamese name cannot print at all, so the
        // ticket coming out at all says whose language it was printed in.
        let (printers, recorder) = recorder(true);
        let session = a_kitchen(
            Some("vi"),
            ReceiptLanguage::English,
            ReceiptLanguage::Vietnamese,
        );
        assert_eq!(ticket_language(&session, grill()).as_deref(), Some("vi"));
        assert_eq!(ticket_language(&session, oven()).as_deref(), Some("en"));

        let outcome = printers
            .print_ticket(
                &session,
                store_id(),
                event_id(1),
                "A1",
                &fired_at(grill(), vec![herbs()]),
                None,
            )
            .await;

        assert_eq!(outcome, PrintOutcome::Printed);
        let written = recorder.written.lock().expect("the recorder");
        let bytes = written.first().expect("a ticket at the oven");
        let printed = |text: &[u8]| bytes.windows(text.len()).any(|window| window == text);
        assert!(printed(b"2 x Grilled pork with noodles"));
        assert!(printed(b"  + Extra herbs"));
    }

    #[test]
    fn a_stations_ticket_language_is_resolved_as_a_receipts_is() {
        let mut session = a_kitchen(
            Some("en"),
            ReceiptLanguage::Country,
            ReceiptLanguage::Display,
        );
        session.country_language = Some("vi".to_owned());
        assert_eq!(ticket_language(&session, oven()).as_deref(), Some("vi"));
        assert_eq!(ticket_language(&session, grill()).as_deref(), Some("en"));
        // A station the plan does not name prints in the display language, as every ticket did.
        assert_eq!(ticket_language(&session, station(9)).as_deref(), Some("en"));
        // A country whose language the edge has no labels in prints in the display language, as a
        // receipt does, rather than in labels and names from two languages.
        session.country_language = Some("ja".to_owned());
        assert_eq!(ticket_language(&session, oven()).as_deref(), Some("en"));
        // A language from a newer release reads as the display language.
        session.stations = StationPlan::from_parts(
            vec![KitchenStation {
                ticket_language: Open::parse("RECEIPT_LANGUAGE_JA"),
                ..session.stations.stations()[0].clone()
            }],
            Vec::new(),
            None,
        );
        assert_eq!(ticket_language(&session, oven()).as_deref(), Some("en"));
    }

    #[test]
    fn a_station_that_sets_no_language_prints_the_ticket_it_always_did() {
        // What `print_ticket` printed before a station could choose: the menu's names, and the
        // labels of the store's display language, or English on a box that cannot draw them.
        for display in [Some("vi"), Some("en"), None] {
            for can_rasterise in [true, false] {
                let session =
                    a_kitchen(display, ReceiptLanguage::Display, ReceiptLanguage::Display);
                let fired = FiredLine {
                    note_present: true,
                    ..fired_at(oven(), vec![herbs(), cheese()])
                };
                let before = ticket_document(
                    "A1",
                    &TicketLine {
                        item: "Bún chả".to_owned(),
                        quantity: "2".to_owned(),
                        modifiers: vec!["Thêm rau".to_owned(), "Phô mai".to_owned()],
                        note: TicketNote::Lost,
                    },
                    PaperLabels::for_store(display, can_rasterise),
                );
                for printing in [oven(), grill()] {
                    assert_eq!(
                        ticket_at(&session, can_rasterise, printing, "A1", &fired, None),
                        before,
                        "{display:?}, fonts {can_rasterise}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_ticket_reference_is_short_enough_to_read_across_a_kitchen() {
        assert_eq!(short_reference("01JQZ8N3K7RT9V0XW2YB4C6DEF"), "4C6DEF");
        // Shorter than six is returned whole rather than padded — a test id is not a crash.
        assert_eq!(short_reference("A1"), "A1");
    }

    #[test]
    fn the_receipt_printer_is_the_one_serving_no_station() {
        let devices = PublishedDevices::new(vec![
            device(1, DeviceKind::Printer, Some(station(9))),
            device(2, DeviceKind::Printer, None),
        ]);
        let chosen = receipt_printer(&devices).expect("a receipt printer");
        assert_eq!(chosen.address, "192.0.2.2:9100");
    }

    #[test]
    fn a_store_with_only_kitchen_printers_has_no_receipt_printer_and_says_so() {
        // `None`, not "fall back to the oven". A guest's bill printing in the kitchen is worse than
        // not printing: the guest waits for paper that went somewhere they cannot see.
        let devices = PublishedDevices::new(vec![device(1, DeviceKind::Printer, Some(station(9)))]);
        assert!(receipt_printer(&devices).is_none());
    }

    #[test]
    fn a_kds_is_not_something_to_send_escpos_to() {
        let devices = PublishedDevices::new(vec![device(1, DeviceKind::Kds, None)]);
        assert!(
            receipt_printer(&devices).is_none(),
            "a kitchen display is a screen, not a printer"
        );
    }

    #[test]
    fn a_station_with_no_printer_falls_back_to_the_plans_declared_backup() {
        let oven = station(1);
        let grill = station(2);
        let plan = StationPlan::from_parts(
            vec![
                KitchenStation {
                    station_id: oven,
                    name: DisplayName::new("Oven"),
                    backup_station_id: Some(grill),
                    late_after_seconds: None,
                    ticket_language: Open::default(),
                },
                KitchenStation {
                    station_id: grill,
                    name: DisplayName::new("Grill"),
                    backup_station_id: None,
                    late_after_seconds: None,
                    ticket_language: Open::default(),
                },
            ],
            Vec::new(),
            None,
        );
        // Only the grill has a printer.
        let devices = PublishedDevices::new(vec![device(2, DeviceKind::Printer, Some(grill))]);
        let (printing, chosen) =
            station_printer(&devices, &plan, oven).expect("the backup station's printer");
        assert_eq!(chosen.address, "192.0.2.2:9100");
        assert_eq!(
            printing, grill,
            "the grill's printer, so the grill's cooks read it"
        );
        let (printing, _) = station_printer(&devices, &plan, grill).expect("its own printer");
        assert_eq!(printing, grill);
    }

    #[test]
    fn the_backup_is_followed_once_and_not_chased_around_a_loop() {
        // Two stations naming each other. `pos_core::floor` rejects a plan like this, but a plan
        // reaching here unvalidated must not hang the printer thread — one hop, then give up.
        let left = station(1);
        let right = station(2);
        let plan = StationPlan::from_parts(
            vec![
                KitchenStation {
                    station_id: left,
                    name: DisplayName::new("Left"),
                    backup_station_id: Some(right),
                    late_after_seconds: None,
                    ticket_language: Open::default(),
                },
                KitchenStation {
                    station_id: right,
                    name: DisplayName::new("Right"),
                    backup_station_id: Some(left),
                    late_after_seconds: None,
                    ticket_language: Open::default(),
                },
            ],
            Vec::new(),
            None,
        );
        let devices = PublishedDevices::new(Vec::new());
        assert!(station_printer(&devices, &plan, left).is_none());
    }

    #[test]
    fn a_connection_this_build_does_not_know_degrades_to_the_posture_that_authorises_least() {
        let mut unknown = device(1, DeviceKind::Printer, None);
        unknown.connection = Open::parse("DEVICE_CONNECTION_INFRARED");
        assert_eq!(connection_of(&unknown), PrinterConnection::Network);
        assert!(
            !connection_of(&unknown).may_open_a_drawer(),
            "an unrecognised connection never authorises a cash drawer"
        );
    }

    fn sold(name: &str, milli: i64, unit: Money, total: Money) -> ReceiptLine {
        ReceiptLine {
            menu_item_id: MenuItemId::new(Ulid::from_u128(0x17E5)),
            display_name: DisplayName::new(name),
            quantity: Quantity::from_milli(milli),
            unit_price: unit,
            line_total: total,
            modifier_display_names: Vec::new(),
            modifier_menu_item_ids: Vec::new(),
        }
    }

    fn printing_in(language: ReceiptLanguage) -> PublishedPrinting {
        PublishedPrinting {
            receipt_language: Open::from_known(language),
            ..PublishedPrinting::default()
        }
    }

    #[test]
    fn a_receipt_follows_the_display_language_until_the_store_chooses_its_own() {
        let mut session = EdgeSession {
            display_language: Some("vi".to_owned()),
            ..EdgeSession::bootstrap()
        };
        assert_eq!(receipt_language(&session, &STORE).as_deref(), Some("vi"));
        session.printing = printing_in(ReceiptLanguage::English);
        assert_eq!(receipt_language(&session, &STORE).as_deref(), Some("en"));
        session.printing = printing_in(ReceiptLanguage::Country);
        assert_eq!(
            receipt_language(&session, &STORE).as_deref(),
            Some("vi"),
            "a store whose locale names no country language prints in its display language"
        );
        session.country_language = Some("ja".to_owned());
        assert_eq!(
            receipt_language(&session, &STORE).as_deref(),
            Some("vi"),
            "and so does one whose country's language the edge has no labels in"
        );
        session.display_language = Some("en".to_owned());
        session.country_language = Some("vi".to_owned());
        let language = receipt_language(&session, &STORE);
        assert_eq!(language.as_deref(), Some("vi"), "whatever the tills show");
        assert_eq!(
            PaperLabels::for_store(language.as_deref(), true),
            &VIETNAMESE
        );
        // No fonts on this box: a label is never the line that stops a receipt printing.
        assert_eq!(PaperLabels::for_store(language.as_deref(), false), &ENGLISH);
    }

    #[test]
    fn a_receipt_in_another_language_names_what_the_menu_translates_and_keeps_the_rest() {
        let item = |seed: u128| MenuItemId::new(Ulid::from_u128(seed));
        let entry = |seed: u128, name: &str, english: Option<&str>| {
            let entry = MenuEntry::new(
                item(seed),
                DisplayName::new(name),
                Money::new(CurrencyCode::VND, 65_000),
                EdgeSession::standard_tax_class(),
            );
            match english {
                Some(english) => entry
                    .with_name_translations([("en".to_owned(), DisplayName::new(english))].into()),
                None => entry,
            }
        };
        // The pho was renamed after it was rung up; the rice has no English name.
        let session = EdgeSession {
            display_language: Some("vi".to_owned()),
            menu: MenuCatalog::new()
                .with(entry(1, "Phở bò đặc biệt", Some("Beef pho")))
                .with(entry(2, "Cơm tấm", None))
                .with(entry(3, "Trứng", Some("Egg"))),
            printing: printing_in(ReceiptLanguage::English),
            ..EdgeSession::bootstrap()
        };
        let price = Money::new(CurrencyCode::VND, 65_000);
        let lines = [
            ReceiptLine {
                menu_item_id: item(1),
                modifier_display_names: vec![DisplayName::new("Trứng")],
                modifier_menu_item_ids: vec![item(3)],
                ..sold("Phở bò", 1_000, price, price)
            },
            ReceiptLine {
                menu_item_id: item(2),
                ..sold("Cơm tấm", 1_000, price, price)
            },
        ];

        let english = receipt_lines_in(&session, &STORE, &lines);
        assert_eq!(english[0].display_name.as_str(), "Beef pho");
        assert_eq!(english[0].modifier_display_names[0].as_str(), "Egg");
        assert_eq!(
            english[1].display_name.as_str(),
            "Cơm tấm",
            "the name it was rung up as"
        );

        // In the display language a receipt prints the names its lines captured, as it always did.
        let as_before = EdgeSession {
            printing: PublishedPrinting::default(),
            ..session
        };
        assert!(matches!(
            receipt_lines_in(&as_before, &STORE, &lines),
            std::borrow::Cow::Borrowed(same) if same == lines
        ));
    }

    #[test]
    fn a_receipt_lists_what_was_sold() {
        // ADR-0129 decision 1, and the shape Decree 123/2020 Art. 10 asks for: the name, then how
        // many at what each, with the extended amount. The rows come before the totals and sum to
        // the subtotal printed under them.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let lines = [
            sold("Phở bò đặc biệt", 2_000, vnd(99_000), vnd(198_000)),
            sold("Margherita", 1_000, vnd(149_000), vnd(149_000)),
        ];

        let document = receipt_document(
            &StoreProfile::default(),
            // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
            &style(0),
            &ENGLISH,
            7,
            &lines,
            &totals_of(vnd(347_000)),
            None,
            None,
        );

        assert_eq!(
            lines_of(&document),
            vec![
                "#7",
                "Phở bò đặc biệt",
                "  2 x VND 99,000  VND 198,000",
                "Margherita",
                "  1 x VND 149,000  VND 149,000",
                "Subtotal  VND 347,000",
                "VND 347,000",
            ]
        );
    }

    /// A fee as `assemble` reports one, under the name its bill froze.
    fn a_fee(seed: u128, code: &str, name: &str, minor: i64) -> FeeLine {
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        FeeLine {
            fee_id: FeeId::new(Ulid::from_u128(seed)),
            code: FeeCode::new(code),
            display_name: DisplayName::new(name),
            amount: vnd(minor),
            class_shares: Vec::new(),
            tax: vnd(0),
            waivable: false,
        }
    }

    #[test]
    fn a_receipt_prints_each_fee_under_its_own_name_and_no_service_charge_line() {
        // ADR-0159 decision 4. The fees take the place of the one service-charge line their sum
        // used to print under, in the order of their rules; a fee that charged nothing is not
        // printed, as a discount of nothing is not.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let totals = BillTotals {
            subtotal: vnd(100_000),
            service_charge: vnd(15_000),
            fee_lines: vec![
                a_fee(1, "SERVICE", "Service charge", 5_000),
                a_fee(2, "COVER", "Cover charge", 10_000),
                a_fee(3, "BAG", "Bag", 0),
            ],
            ..totals_of(vnd(115_000))
        };
        let document = receipt_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            7,
            &[],
            &totals,
            None,
            None,
        );
        assert_eq!(
            lines_of(&document),
            vec![
                "#7",
                "Subtotal  VND 100,000",
                "Service charge  VND 5,000",
                "Cover charge  VND 10,000",
                "VND 115,000",
            ]
        );

        // A bill with no fee line prints its service charge as every receipt did.
        let lump = BillTotals {
            fee_lines: Vec::new(),
            ..totals
        };
        let document = receipt_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            7,
            &[],
            &lump,
            None,
            None,
        );
        assert!(lines_of(&document).contains(&"Service charge  VND 15,000"));
    }

    #[test]
    fn a_fee_prints_in_the_receipts_language_where_its_rule_translates_it() {
        let rules: PublishedFees = serde_json::from_str(
            r#"{"fees": [
                {"fee_id": "00000000000000000000000001", "code": "SERVICE",
                 "display_name": "Service fee", "display_name_translations": {"vi": "Phí phục vụ"}},
                {"fee_id": "00000000000000000000000002", "code": "COVER",
                 "display_name": "Cover"}
            ]}"#,
        )
        .expect("a fees node");
        let mut session = EdgeSession {
            display_language: Some("vi".to_owned()),
            fees: rules,
            ..EdgeSession::bootstrap()
        };
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        // The bill froze the rule's own names; the service fee was renamed since, the cover is not
        // translated, and a third rule is no longer published.
        let totals = BillTotals {
            fee_lines: vec![
                a_fee(1, "SERVICE", "Service charge", 5_000),
                a_fee(2, "COVER", "Cover charge", 10_000),
                a_fee(3, "BAG", "Bag", 2_000),
            ],
            ..totals_of(vnd(117_000))
        };
        let names = |totals: &BillTotals| -> Vec<String> {
            totals
                .fee_lines
                .iter()
                .map(|fee| fee.display_name.as_str().to_owned())
                .collect()
        };
        assert_eq!(
            names(&receipt_totals_in(&session, &STORE, &totals)),
            ["Phí phục vụ", "Cover charge", "Bag"]
        );
        // In English nothing is translated, so the receipt prints the names the bill froze.
        session.printing = printing_in(ReceiptLanguage::English);
        assert!(matches!(
            receipt_totals_in(&session, &STORE, &totals),
            std::borrow::Cow::Borrowed(same) if same == &totals
        ));
    }

    #[test]
    fn a_pre_bill_is_the_receipts_rows_under_a_reference_and_no_number() {
        // Roadmap-v3 B2.1. The same rows and figures the receipt will print, so the guest checks
        // what they will pay; headed as a pre-bill under the order's reference, with no receipt
        // number (nothing is settled, and a number here would leave a hole in the gapless series);
        // and signed off "Not a receipt", because on the table it looks like one.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let lines = [
            sold("Phở bò đặc biệt", 2_000, vnd(99_000), vnd(198_000)),
            sold("Margherita", 1_000, vnd(149_000), vnd(149_000)),
        ];

        let document = pre_bill_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            "3K9Q2Z",
            &lines,
            &totals_of(vnd(347_000)),
            None,
        );

        assert_eq!(
            lines_of(&document),
            vec![
                "PRE-BILL",
                "3K9Q2Z",
                "Phở bò đặc biệt",
                "  2 x VND 99,000  VND 198,000",
                "Margherita",
                "  1 x VND 149,000  VND 149,000",
                "Subtotal  VND 347,000",
                "VND 347,000",
                "Not a receipt",
            ]
        );
        assert!(
            !lines_of(&document).iter().any(|line| line.starts_with('#')),
            "a pre-bill carries no receipt number"
        );
        assert_eq!(document.blocks.last(), Some(&PrintBlock::Cut));
    }

    #[test]
    fn a_copy_is_the_receipt_under_its_own_number_marked_as_a_copy() {
        // ADR-0164 decision 5. The same rows and figures under the same number, so the copy matches
        // the paper the guest already holds; under the number, COPY in bold and which copy it is,
        // so a copy taken away is never read as a second sale.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let lines = [sold("Margherita", 1_000, vnd(149_000), vnd(149_000))];
        let totals = totals_of(vnd(149_000));
        let copy = |labels| {
            receipt_copy_document(
                &StoreProfile::default(),
                &style(0),
                labels,
                42,
                2,
                "2026-09-30 19:05",
                &lines,
                &totals,
                None,
                None,
            )
        };

        let english = copy(&ENGLISH);
        assert_eq!(
            lines_of(&english),
            vec![
                "#42",
                "COPY",
                "Reprint 2 - 2026-09-30 19:05",
                "Margherita",
                "  1 x VND 149,000  VND 149,000",
                "Subtotal  VND 149,000",
                "VND 149,000",
            ]
        );
        assert!(
            english.blocks.iter().any(|block| matches!(
                block,
                PrintBlock::Text { line, style } if line == "COPY" && style.emphasised
            )),
            "COPY is printed in bold"
        );
        let original = receipt_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            42,
            &lines,
            &totals,
            None,
            None,
        );
        let mut unmarked = lines_of(&english);
        unmarked.retain(|line| *line != "COPY" && !line.starts_with("Reprint "));
        assert_eq!(
            unmarked,
            lines_of(&original),
            "apart from the marking, the copy is the original"
        );
        assert_eq!(english.blocks.last(), Some(&PrintBlock::Cut));

        let vietnamese = copy(&VIETNAMESE);
        assert_eq!(
            lines_of(&vietnamese)[..3],
            ["#42", "BẢN SAO", "In lại lần 2 - 2026-09-30 19:05"]
        );
    }

    #[test]
    fn a_copy_without_its_per_rate_lines_prints_the_recorded_tax_as_one_figure() {
        // ADR-0164 decision 5: when the rates in force no longer give the tax the settle recorded,
        // the copy prints that tax as one figure rather than a line at a rate that did not apply,
        // and the paper still adds up to what the guest paid.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let lines = [sold("Margherita", 1_000, vnd(150_000), vnd(150_000))];
        let totals = BillTotals {
            tax_total: vnd(15_000),
            total_due: vnd(165_000),
            ..totals_of(vnd(150_000))
        };

        let document = receipt_copy_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            7,
            1,
            "2026-09-30 19:05",
            &lines,
            &totals,
            None,
            None,
        );
        assert_eq!(
            lines_of(&document)[5..],
            ["Subtotal  VND 150,000", "Tax  VND 15,000", "VND 165,000"]
        );
    }

    #[test]
    fn a_shift_report_adds_up_on_paper_and_says_over_or_short_in_words() {
        // The float plus the cash taken is the expectation, and the count less the expectation is
        // the variance — in that order, so a supervisor checks it top to bottom. The word beside
        // the variance is what they read: a minus sign on thermal paper is one dot wide.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let report = |counted: i64| ShiftReport {
            shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
            opening_float: vnd(500_000),
            cash_collected: vnd(1_250_000),
            paid_in: vnd(0),
            paid_out: vnd(0),
            expected_amount: vnd(1_750_000),
            counted_amount: vnd(counted),
            variance: vnd(counted - 1_750_000),
        };

        let short = shift_report_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            &report(1_730_000),
        );
        let lines = lines_of(&short);
        assert_eq!(lines[0], "SHIFT REPORT");
        assert_eq!(
            lines[2..],
            [
                "Opening float  VND 500,000",
                "Cash taken  VND 1,250,000",
                "Expected in drawer  VND 1,750,000",
                "Counted  VND 1,730,000",
                "Variance  VND -20,000",
                "Short",
            ]
        );
        assert_eq!(short.blocks.last(), Some(&PrintBlock::Cut));

        let balanced = shift_report_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            &report(1_750_000),
        );
        assert_eq!(lines_of(&balanced).last(), Some(&"Balanced"));
        let over = shift_report_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            &report(1_760_000),
        );
        assert_eq!(lines_of(&over).last(), Some(&"Over"));
    }

    #[test]
    fn a_shift_report_prints_what_was_paid_in_and_out_so_the_expectation_adds_up() {
        // ADR-0165 decision 2: 500k float + 1,250k taken + 200k paid in - 150k paid out is 1,800k,
        // and the paper shows every term, in the order the sum runs.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let report = ShiftReport {
            shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
            opening_float: vnd(500_000),
            cash_collected: vnd(1_250_000),
            paid_in: vnd(200_000),
            paid_out: vnd(150_000),
            expected_amount: vnd(1_800_000),
            counted_amount: vnd(1_800_000),
            variance: vnd(0),
        };
        let english = shift_report_document(&StoreProfile::default(), &style(0), &ENGLISH, &report);
        assert_eq!(
            lines_of(&english)[2..],
            [
                "Opening float  VND 500,000",
                "Cash taken  VND 1,250,000",
                "Paid in  VND 200,000",
                "Paid out  VND 150,000",
                "Expected in drawer  VND 1,800,000",
                "Counted  VND 1,800,000",
                "Variance  VND 0",
                "Balanced",
            ]
        );

        let paid_out_only = ShiftReport {
            paid_in: vnd(0),
            expected_amount: vnd(1_600_000),
            counted_amount: vnd(1_600_000),
            ..report
        };
        let vietnamese = shift_report_document(
            &StoreProfile::default(),
            &style(0),
            &VIETNAMESE,
            &paid_out_only,
        );
        let drawn = lines_of(&vietnamese);
        assert!(
            drawn.contains(&"Chi ngoài bán hàng  VND 150,000"),
            "{drawn:?}"
        );
        assert!(
            !drawn
                .iter()
                .any(|line| line.starts_with("Thu ngoài bán hàng")),
            "nothing paid in prints no paid-in line: {drawn:?}"
        );
    }

    #[test]
    fn a_vietnamese_store_prints_its_bill_and_its_drawer_in_vietnamese() {
        // The same documents, in the words a Vietnamese guest and cashier read: the heading, every
        // total's label and the sign-off change; the rows, the amounts and their order do not.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let lines = [sold("Phở bò đặc biệt", 1_000, vnd(99_000), vnd(99_000))];
        let pre_bill = pre_bill_document(
            &StoreProfile::default(),
            &style(0),
            &VIETNAMESE,
            "3K9Q2Z",
            &lines,
            &totals_of(vnd(99_000)),
            None,
        );
        assert_eq!(
            lines_of(&pre_bill),
            vec![
                "PHIẾU TẠM TÍNH",
                "3K9Q2Z",
                "Phở bò đặc biệt",
                "  1 x VND 99,000  VND 99,000",
                "Tạm tính  VND 99,000",
                "VND 99,000",
                "Không phải hóa đơn thanh toán",
            ]
        );

        let report = ShiftReport {
            shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
            opening_float: vnd(500_000),
            cash_collected: vnd(1_250_000),
            paid_in: vnd(0),
            paid_out: vnd(0),
            expected_amount: vnd(1_750_000),
            counted_amount: vnd(1_730_000),
            variance: vnd(-20_000),
        };
        let drawer =
            shift_report_document(&StoreProfile::default(), &style(0), &VIETNAMESE, &report);
        let drawn = lines_of(&drawer);
        assert_eq!(drawn.first(), Some(&"BÁO CÁO CA"));
        assert_eq!(drawn.last(), Some(&"Thiếu"));
        assert!(drawn.contains(&"Chênh lệch  VND -20,000"));
    }

    /// The bill the bilingual cases print: a pho with an egg, which the menu names in English, an
    /// iced tea it does not, a service fee whose rule names it in English, and 8 % tax.
    fn a_bilingual_bill() -> ([ReceiptLine; 2], BillTotals) {
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let lines = [
            ReceiptLine {
                menu_item_id: MenuItemId::new(Ulid::from_u128(1)),
                modifier_display_names: vec![DisplayName::new("Thêm trứng")],
                modifier_menu_item_ids: vec![MenuItemId::new(Ulid::from_u128(3))],
                ..sold("Phở bò", 2_000, vnd(50_000), vnd(100_000))
            },
            ReceiptLine {
                menu_item_id: MenuItemId::new(Ulid::from_u128(2)),
                ..sold("Trà đá", 1_000, vnd(10_000), vnd(10_000))
            },
        ];
        let totals = BillTotals {
            subtotal: vnd(110_000),
            service_charge: vnd(5_500),
            fee_lines: vec![a_fee(1, "SERVICE", "Phí dịch vụ", 5_500)],
            tax_lines: vec![pos_core::billing::TaxLine {
                tax_class_id: EdgeSession::standard_tax_class(),
                taxable_base: vnd(115_500),
                rate_basis_points: 800,
                tax: vnd(9_240),
                components: Vec::new(),
            }],
            tax_total: vnd(9_240),
            ..totals_of(vnd(124_740))
        };
        (lines, totals)
    }

    /// English as [`a_bilingual_bill`]'s second language, on paper `columns` characters wide.
    fn in_english(columns: usize) -> SecondLanguage {
        SecondLanguage {
            labels: &ENGLISH,
            columns,
            rows: vec![
                SecondNames {
                    item: Some(DisplayName::new("Beef pho")),
                    modifiers: vec![Some(DisplayName::new("Extra egg"))],
                },
                // The iced tea has no English name, so nothing prints under it.
                SecondNames::default(),
            ],
            fees: vec![Some(DisplayName::new("Service charge"))],
        }
    }

    #[test]
    fn a_bilingual_receipt_pairs_a_label_where_it_fits_and_puts_it_under_where_not() {
        // ADR-0160 decision 2, on the three widths a receipt printer is: 58 mm paper's 32
        // characters, and 80 mm paper's 42 and 48.
        let (lines, totals) = a_bilingual_bill();
        let receipt = |columns| -> Vec<String> {
            let second = in_english(columns);
            let document = receipt_document(
                &StoreProfile::default(),
                &style(0),
                &VIETNAMESE,
                7,
                &lines,
                &totals,
                None,
                Some(&second),
            );
            lines_of(&document).into_iter().map(str::to_owned).collect()
        };
        let rows = [
            "#7",
            "Phở bò",
            "  Beef pho",
            "  + Thêm trứng",
            "    Extra egg",
            "  2 x VND 50,000  VND 100,000",
            "Trà đá",
            "  1 x VND 10,000  VND 10,000",
            // Thirty-two characters, so it fits even the narrowest paper, exactly.
            "Tạm tính / Subtotal  VND 110,000",
        ];
        let wide: Vec<&str> = rows
            .into_iter()
            .chain([
                "Phí dịch vụ / Service charge  VND 5,500",
                "Thuế / Tax 8.00%  VND 9,240",
                "VND 124,740",
            ])
            .collect();
        assert_eq!(receipt(48), wide);
        assert_eq!(receipt(42), wide);
        // On 58 mm paper the fee and its English name do not fit on one line, so the English goes
        // under it, and the line above is the one a Vietnamese receipt prints.
        let narrow: Vec<&str> = rows
            .into_iter()
            .chain([
                "Phí dịch vụ  VND 5,500",
                "  Service charge",
                "Thuế / Tax 8.00%  VND 9,240",
                "VND 124,740",
            ])
            .collect();
        assert_eq!(receipt(32), narrow);
    }

    #[test]
    fn a_bilingual_pre_bill_and_copy_head_themselves_in_both_languages() {
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let headings_only = |columns| SecondLanguage {
            rows: Vec::new(),
            fees: Vec::new(),
            ..in_english(columns)
        };
        let pre_bill = |columns| -> Vec<String> {
            let second = headings_only(columns);
            let document = pre_bill_document(
                &StoreProfile::default(),
                &style(0),
                &VIETNAMESE,
                "3K9Q2Z",
                &[],
                &totals_of(vnd(99_000)),
                Some(&second),
            );
            lines_of(&document).into_iter().map(str::to_owned).collect()
        };
        assert_eq!(
            pre_bill(48),
            [
                "PHIẾU TẠM TÍNH / PRE-BILL",
                "3K9Q2Z",
                "Tạm tính / Subtotal  VND 99,000",
                "VND 99,000",
                "Không phải hóa đơn thanh toán / Not a receipt",
            ]
        );
        // Forty-five characters: on 80 mm paper's 42 the English is centred under the Vietnamese.
        assert_eq!(
            pre_bill(42)[4..],
            ["Không phải hóa đơn thanh toán", "Not a receipt"]
        );

        let second = headings_only(32);
        let copy = receipt_copy_document(
            &StoreProfile::default(),
            &style(0),
            &VIETNAMESE,
            7,
            2,
            "09:15",
            &[],
            &totals_of(vnd(99_000)),
            None,
            Some(&second),
        );
        assert_eq!(
            lines_of(&copy)[..3],
            ["#7", "BẢN SAO / COPY", "In lại lần / Reprint 2 - 09:15"]
        );
    }

    #[test]
    fn a_second_language_names_what_the_menu_and_the_fees_rule_translate_and_nothing_else() {
        let entry = |seed: u128, name: &str, english: Option<&str>| {
            let entry = MenuEntry::new(
                MenuItemId::new(Ulid::from_u128(seed)),
                DisplayName::new(name),
                Money::new(CurrencyCode::VND, 50_000),
                EdgeSession::standard_tax_class(),
            );
            match english {
                Some(english) => entry
                    .with_name_translations([("en".to_owned(), DisplayName::new(english))].into()),
                None => entry,
            }
        };
        let rules: PublishedFees = serde_json::from_str(
            r#"{"fees": [{"fee_id": "00000000000000000000000001", "code": "SERVICE",
                 "display_name": "Phí dịch vụ", "display_name_translations": {"en": "Service charge"}}]}"#,
        )
        .expect("a fees node");
        let second_in = |language| PublishedPrinting {
            receipt_second_language: Open::from_known(language),
            ..PublishedPrinting::default()
        };
        let session = EdgeSession {
            display_language: Some("vi".to_owned()),
            menu: MenuCatalog::new()
                .with(entry(1, "Phở bò", Some("Beef pho")))
                .with(entry(2, "Trà đá", None))
                .with(entry(3, "Thêm trứng", Some("Egg"))),
            fees: rules,
            printing: second_in(ReceiptSecondLanguage::English),
            ..EdgeSession::bootstrap()
        };
        let (lines, totals) = a_bilingual_bill();
        // At the store's own choices, on paper 42 characters wide.
        let second = |session: &EdgeSession, first, rasterise, printed: &[ReceiptLine]| {
            SecondLanguage::for_receipt(
                session, &STORE, first, rasterise, 42, &lines, printed, &totals,
            )
        };
        let english = second(&session, &VIETNAMESE, true, &lines).expect("a bilingual receipt");
        assert_eq!(english.labels, &ENGLISH);
        assert_eq!(
            english.rows,
            [
                SecondNames {
                    item: Some(DisplayName::new("Beef pho")),
                    modifiers: vec![Some(DisplayName::new("Egg"))],
                },
                SecondNames::default(),
            ]
        );
        assert_eq!(english.fees, [Some(DisplayName::new("Service charge"))]);

        // A receipt in English with Vietnamese under it: the rows were rung up in Vietnamese, so
        // those are its Vietnamese names, printed where the English differs from them.
        let session = EdgeSession {
            printing: PublishedPrinting {
                receipt_language: Open::from_known(ReceiptLanguage::English),
                ..second_in(ReceiptSecondLanguage::Vietnamese)
            },
            ..session
        };
        let printed = receipt_lines_in(&session, &STORE, &lines);
        let vietnamese = second(&session, &ENGLISH, true, &printed).expect("a bilingual receipt");
        assert_eq!(
            vietnamese.rows,
            [
                SecondNames {
                    item: Some(DisplayName::new("Phở bò")),
                    modifiers: vec![Some(DisplayName::new("Thêm trứng"))],
                },
                // Rung up as "Trà đá" and printed as that, so it prints once.
                SecondNames {
                    item: None,
                    modifiers: Vec::new(),
                },
            ]
        );

        // One language: on a box with no fonts, which cannot draw Vietnamese labels; where the
        // second language is the receipt's own; and where none is set.
        assert_eq!(second(&session, &ENGLISH, false, &printed), None);
        let same = EdgeSession {
            printing: second_in(ReceiptSecondLanguage::Vietnamese),
            ..session.clone()
        };
        assert_eq!(second(&same, &VIETNAMESE, true, &lines), None);
        let none = EdgeSession {
            printing: PublishedPrinting::default(),
            ..session
        };
        assert_eq!(second(&none, &VIETNAMESE, true, &lines), None);
    }

    #[test]
    fn a_receipt_prints_what_each_line_was_made_with() {
        // ADR-0144: the names the line recorded, under the item and above its amount, whose unit
        // price already includes them. A line recorded before the event carried names has none, and
        // prints exactly what it printed before.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let mut pizza = sold("Margherita", 1_000, vnd(189_000), vnd(189_000));
        pizza.modifier_display_names = vec![DisplayName::new("30 cm"), DisplayName::new("Burrata")];
        let lines = [pizza, sold("Phở bò", 1_000, vnd(99_000), vnd(99_000))];

        let document = receipt_document(
            &StoreProfile::default(),
            // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
            &style(0),
            &ENGLISH,
            9,
            &lines,
            &totals_of(vnd(288_000)),
            None,
            None,
        );

        assert_eq!(
            lines_of(&document),
            vec![
                "#9",
                "Margherita",
                "  + 30 cm",
                "  + Burrata",
                "  1 x VND 189,000  VND 189,000",
                "Phở bò",
                "  1 x VND 99,000  VND 99,000",
                "Subtotal  VND 288,000",
                "VND 288,000",
            ]
        );
    }

    #[test]
    fn a_half_prints_as_a_half_and_not_as_five_hundred() {
        // `Quantity` counts thousandths, so the raw figure for one half is 500. A receipt reading
        // "500 x" is a rounding bug nobody would guess at from the paper, which is why the rows
        // share `format_quantity` with the kitchen ticket rather than printing the integer.
        let jpy = |minor| Money::new(CurrencyCode::JPY, minor);
        let lines = [sold("Hokkaido melon", 500, jpy(4_000), jpy(2_000))];

        let document = receipt_document(
            &StoreProfile::default(),
            // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
            &style(0),
            &ENGLISH,
            8,
            &lines,
            &totals_of(jpy(2_000)),
            None,
            None,
        );

        assert!(
            lines_of(&document).contains(&"  0.5 x JPY 4,000  JPY 2,000"),
            "one half of an item is half of its unit price"
        );
    }

    #[test]
    fn a_store_that_has_filled_nothing_in_prints_what_it_always_printed() {
        // The never-blank rule applied to a legal document: an empty label reads as a value somebody
        // forgot to type, so an unfilled profile shortens the receipt rather than padding it
        // (ADR-0106).
        let document = receipt_document(
            &StoreProfile::default(),
            // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
            &style(0),
            &ENGLISH,
            7,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 50_000)),
            None,
            None,
        );
        assert_eq!(
            lines_of(&document),
            vec!["#7", "Subtotal  VND 50,000", "VND 50,000"]
        );
    }

    #[test]
    fn a_receipt_names_the_store_and_ends_in_a_cut() {
        let profile = StoreProfile {
            legal_name: "Pizza 4P's Vietnam Co., Ltd".to_owned(),
            trading_name: Some("Bến Thành".to_owned()),
            address_lines: vec!["8 Thủ Khoa Huân, Quận 1".to_owned()],
            tax_registration_number: Some("0101243150".to_owned()),
            tax_registration_label: Some("MST".to_owned()),
            contact_lines: vec!["028 3822 9838".to_owned()],
            footer_lines: vec!["Cảm ơn quý khách".to_owned()],
        };
        let document = receipt_document(
            &profile,
            // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
            &style(0),
            &ENGLISH,
            42,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 99_000)),
            None,
            None,
        );
        assert_eq!(
            lines_of(&document),
            vec![
                "Bến Thành",
                "8 Thủ Khoa Huân, Quận 1",
                "MST: 0101243150",
                "#42",
                "Subtotal  VND 99,000",
                "VND 99,000",
                "028 3822 9838",
                "Cảm ơn quý khách",
            ]
        );
        assert_eq!(
            document.blocks.last(),
            Some(&PrintBlock::Cut),
            "the paper is cut, or the next receipt starts on this one"
        );
    }

    #[test]
    fn a_b2b_invoice_names_its_buyer_between_the_seller_and_the_figures() {
        // Without the buyer's name and registration number a Japanese qualified invoice does not
        // let the buyer claim input tax, which is the whole reason a business asks for one
        // (ADR-0107). The block sits after the seller's and before the money, which is the order
        // both Japan's qualified invoice and India's Rule 46 read in.
        let profile = StoreProfile {
            legal_name: "Pizza 4P's Japan".to_owned(),
            tax_registration_number: Some("T9876543210987".to_owned()),
            tax_registration_label: Some("登録番号".to_owned()),
            ..StoreProfile::default()
        };
        let buyer = BuyerDetails {
            name: "Kabushiki Kaisha Reiwa".to_owned(),
            tax_code: Some("T1234567890123".to_owned()),
            address: Some("1-1 Marunouchi, Chiyoda".to_owned()),
            email: Some("accounts@example.co.jp".to_owned()),
        };
        let document = receipt_document(
            &profile,
            // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
            &style(0),
            &ENGLISH,
            11,
            &[],
            &totals_of(Money::new(CurrencyCode::JPY, 1_100)),
            Some(&buyer),
            None,
        );
        let lines = lines_of(&document);
        assert!(
            lines.contains(&"Bill to: Kabushiki Kaisha Reiwa"),
            "the buyer is named: {lines:?}"
        );
        assert!(lines.contains(&"T1234567890123"));
        assert!(lines.contains(&"1-1 Marunouchi, Chiyoda"));
        assert!(
            !lines.iter().any(|line| line.contains("example.co.jp")),
            "an email is where to send the copy, not part of the document"
        );
    }

    #[test]
    fn a_retail_receipt_has_no_buyer_block_at_all() {
        // The overwhelming majority of bills. A blank "Bill to:" on a receipt reads as a value
        // somebody forgot to type, so the block is absent rather than empty.
        let document = receipt_document(
            &StoreProfile::default(),
            // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
            &style(0),
            &ENGLISH,
            12,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 50_000)),
            None,
            None,
        );
        assert!(
            !lines_of(&document)
                .iter()
                .any(|line| line.starts_with("Bill to")),
            "an ordinary sale prints the receipt it always did"
        );
    }

    #[test]
    fn an_indian_receipt_prints_cgst_and_sgst_under_the_rate_they_explain() {
        // The document ADR-0104 exists for: the halves go to different governments, so an invoice
        // printing their sum is not a lesser rendering of the same fact — it is not a valid invoice.
        use pos_core::billing::{TaxComponentLine, TaxLine};
        use pos_proto::ids::TaxClassId;

        let inr = CurrencyCode::parse("INR").expect("INR is three upper-case letters");
        let money = |amount| Money::new(inr, amount);
        let totals = BillTotals {
            subtotal: money(100_000),
            discount_total: money(0),
            comp_total: money(0),
            service_charge: money(0),
            fee_lines: Vec::new(),
            tax_lines: vec![TaxLine {
                tax_class_id: TaxClassId::new(Ulid::from_u128(1)),
                taxable_base: money(100_000),
                rate_basis_points: 500,
                tax: money(5_000),
                components: vec![
                    TaxComponentLine {
                        name: "CGST".to_owned(),
                        rate_basis_points: 250,
                        tax: money(2_500),
                    },
                    TaxComponentLine {
                        name: "SGST".to_owned(),
                        rate_basis_points: 250,
                        tax: money(2_500),
                    },
                ],
            }],
            tax_total: money(5_000),
            rounding_adjustment: money(-100),
            total_due: money(104_900),
        };
        let profile = StoreProfile {
            legal_name: "Pizza 4P's India".to_owned(),
            tax_registration_number: Some("29ABCDE1234F1Z5".to_owned()),
            tax_registration_label: Some("GSTIN".to_owned()),
            ..StoreProfile::default()
        };
        // Two, because this fixture is denominated in **paise** and the rupee has a hundred of them
        // (ADR-0134). This is the receipt that record is about: every figure below used to print as
        // its own minor units, so a guest who paid ₹1,049.00 was handed a document reading
        // `INR 104900` — on the one piece of paper India's Rule 46 makes a tax invoice.
        //
        // The rounding line is the other half of why the exponent is not the same fact as the
        // increment: the invoice still rounds to the whole rupee, which is what `-1.00` is, while
        // the figures it rounds are quoted in paise.
        let document = receipt_document(&profile, &style(2), &ENGLISH, 9, &[], &totals, None, None);
        assert_eq!(
            lines_of(&document),
            vec![
                "Pizza 4P's India",
                "GSTIN: 29ABCDE1234F1Z5",
                "#9",
                "Subtotal  INR 1,000.00",
                "Tax 5.00%  INR 50.00",
                "  CGST 2.50%  INR 25.00",
                "  SGST 2.50%  INR 25.00",
                "Rounding  INR -1.00",
                "INR 1,049.00",
            ]
        );
    }

    #[test]
    fn a_vietnamese_receipt_is_grouped_the_way_vietnam_writes_a_number() {
        // ADR-0136. The figures were already right after ADR-0134 — the đồng has no subunit, so the
        // arithmetic was never the problem here — but they were *spelled* `VND 1,234,567`, which is
        // not how anyone in the country writes a number. The store's own pack says `.` groups and
        // `,` points, and now the paper says it too.
        let profile = StoreProfile {
            legal_name: "Pizza 4P's Bến Thành".to_owned(),
            ..StoreProfile::default()
        };
        let vietnam = MoneyStyle {
            // The đồng has no subunit, so nothing here exercises the decimal mark. The mark is still
            // published and still differs from the default, which is why the two-decimal case below
            // exists: a store on a fractional currency in a comma-decimal country is where getting
            // only the group separator right would still print a figure nobody writes.
            exponent: 0,
            format: NumberFormat {
                decimal_separator: ',',
                group_separator: '.',
                digits_per_group: 3,
            },
        };
        let document = receipt_document(
            &profile,
            &vietnam,
            &ENGLISH,
            5,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 1_234_567)),
            None,
            None,
        );
        assert!(
            lines_of(&document).contains(&"VND 1.234.567"),
            "the total is grouped the way the store's country writes numbers: {:?}",
            lines_of(&document)
        );
    }

    #[test]
    fn both_marks_are_the_store_s_own_where_the_currency_has_a_fraction() {
        // The case a group-separator-only fix would still get wrong: two decimals *and* a country
        // that points with a comma. `1.234,56` is one figure; `1.234.56` and `1,234,56` are both
        // nonsense, and each is what you get from honouring one mark and not the other.
        let style = MoneyStyle {
            exponent: 2,
            format: NumberFormat {
                decimal_separator: ',',
                group_separator: '.',
                digits_per_group: 3,
            },
        };
        let document = receipt_document(
            &StoreProfile::default(),
            &style,
            &ENGLISH,
            6,
            &[],
            &totals_of(Money::new(CurrencyCode::USD, 123_456)),
            None,
            None,
        );
        assert!(
            lines_of(&document).contains(&"USD 1.234,56"),
            "both marks come from the store: {:?}",
            lines_of(&document)
        );
    }

    #[test]
    fn a_registration_label_with_no_number_prints_no_line() {
        // A label alone reads as a number somebody forgot to type, which on a tax invoice is worse
        // than a shorter receipt.
        let profile = StoreProfile {
            legal_name: "Pizza 4P's Ginza".to_owned(),
            tax_registration_label: Some("登録番号".to_owned()),
            ..StoreProfile::default()
        };
        let document = receipt_document(
            &profile,
            // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
            &style(0),
            &ENGLISH,
            3,
            &[],
            &totals_of(Money::new(CurrencyCode::JPY, 1_100)),
            None,
            None,
        );
        assert!(
            !lines_of(&document)
                .iter()
                .any(|line| line.contains("登録番号")),
            "a label with nothing after it is not printed"
        );
    }

    /// The fonts a test box renders with, or `None` where the machine running the suite has no
    /// font packages — the same condition a store hits, and the reason this is a skip rather than a
    /// failure. `pos-render`'s own suite covers the rendering itself.
    fn fonts() -> Option<pos_render::TextRenderer> {
        fonts_at(24)
    }

    /// [`fonts`] loaded at `size` dots per em: the renderer a box whose file set that size built
    /// before ADR-0160 decision 6 made the size a setting.
    fn fonts_at(size: u16) -> Option<pos_render::TextRenderer> {
        let mut library = pos_render::FontLibrary::new();
        for directory in [
            "/usr/share/fonts/truetype",
            "/usr/share/fonts/opentype",
            "C:\\Windows\\Fonts",
        ] {
            let _ = library.add_directory(std::path::Path::new(directory));
        }
        if library.is_empty() || !library.covers('ở') {
            return None;
        }
        Some(pos_render::TextRenderer::new(
            library,
            NonZeroU16::new(size).expect("positive"),
        ))
    }

    fn recorder_with_fonts(reachable: bool) -> Option<(Printers, Arc<Recorder>)> {
        let renderer = fonts()?;
        let recorder = Arc::new(Recorder {
            written: Mutex::new(Vec::new()),
            reachable,
        });
        let printers = Printers::over(Arc::new(Recorders {
            recorder: Arc::clone(&recorder),
        }))
        .with_fonts(renderer);
        Some((printers, recorder))
    }

    #[tokio::test]
    async fn a_vietnamese_kitchen_ticket_prints_when_a_font_is_installed() {
        // The whole point of ADR-0102, end to end: the same ticket that is refused above comes out
        // of the printer here, because the line the firmware cannot spell is drawn and sent as a
        // raster instead.
        let Some((printers, recorder)) = recorder_with_fonts(true) else {
            return;
        };
        // A station that sets no language: the dish prints as the menu names it.
        let session = a_kitchen(None, ReceiptLanguage::Display, ReceiptLanguage::Display);
        let outcome = printers
            .print_ticket(
                &session,
                store_id(),
                event_id(1),
                "A1",
                &fired_at(oven(), vec![]),
                None,
            )
            .await;

        assert_eq!(outcome, PrintOutcome::Printed);
        let written = recorder.written.lock().expect("the recorder");
        assert_eq!(written.len(), 1, "one ticket");
        let bytes = written.first().expect("a ticket");
        // `GS v 0` — the raster command. Its presence is what says the diacritics went out as an
        // image rather than as bytes the printer would have mangled.
        assert!(
            bytes
                .windows(4)
                .any(|window| window == [0x1D, 0x76, 0x30, 0x00]),
            "the ticket should carry a raster image"
        );
    }

    #[tokio::test]
    async fn an_ascii_line_still_goes_out_as_text_when_fonts_are_loaded() {
        // Rasterising everything would work and would be wrong: text is a fraction of the bytes and
        // comes out of the head faster, so the code page is still used where it is sufficient.
        let Some((printers, recorder)) = recorder_with_fonts(true) else {
            return;
        };
        let session = session_with(PublishedDevices::new(vec![device(
            2,
            DeviceKind::Printer,
            None,
        )]));
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(1),
                42,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 99_000)),
                None,
            )
            .await;

        assert_eq!(outcome, PrintOutcome::Printed);
        let written = recorder.written.lock().expect("the recorder");
        let bytes = written.first().expect("a receipt");
        assert!(
            !bytes
                .windows(4)
                .any(|window| window == [0x1D, 0x76, 0x30, 0x00]),
            "an ASCII receipt needs no raster"
        );
    }

    /// The bytes a Vietnamese kitchen ticket goes out as, from fonts loaded at `loaded`, with `local`
    /// this box's own size and `published` the store's `printing.font_size_dots`. `None` where the
    /// machine running the suite has no fonts.
    async fn ticket_drawn(
        loaded: u16,
        local: Option<u16>,
        published: Option<i64>,
    ) -> Option<Vec<u8>> {
        let recorder = Arc::new(Recorder {
            written: Mutex::new(Vec::new()),
            reachable: true,
        });
        let printers = Printers::over(Arc::new(Recorders {
            recorder: Arc::clone(&recorder),
        }))
        .with_fonts(fonts_at(loaded)?)
        .with_local_font_size(local);
        let session = EdgeSession {
            printing: PublishedPrinting {
                font_size_dots: published,
                ..PublishedPrinting::default()
            },
            ..a_kitchen(None, ReceiptLanguage::Display, ReceiptLanguage::Display)
        };
        let outcome = printers
            .print_ticket(
                &session,
                store_id(),
                event_id(1),
                "A1",
                &fired_at(oven(), vec![]),
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::Printed);
        let written = recorder.written.lock().expect("the recorder");
        written.first().cloned()
    }

    /// A store that publishes a font size has its rasterised lines drawn at it, exactly as a box
    /// whose file set that size drew them before the setting; a store that publishes none has them
    /// drawn at the box's own size, and at 24 where the box sets none; and a published size out of
    /// bounds is not read (ADR-0160 decision 6).
    #[tokio::test]
    async fn a_rasterised_line_is_drawn_at_the_published_size_then_the_files_then_twenty_four() {
        // What a box printed before the setting: its fonts loaded at its file's size.
        let Some(file_was_32) = ticket_drawn(32, None, None).await else {
            return;
        };
        let file_was_30 = ticket_drawn(30, None, None).await.expect("fonts");
        let neither = ticket_drawn(24, None, None).await.expect("fonts");
        assert_ne!(neither, file_was_32, "the size is on the paper");

        assert_eq!(
            ticket_drawn(24, None, Some(32)).await.expect("fonts"),
            file_was_32,
            "a published 32 draws at 32"
        );
        assert_eq!(
            ticket_drawn(24, Some(30), Some(32)).await.expect("fonts"),
            file_was_32,
            "and wins over the box's own"
        );
        assert_eq!(
            ticket_drawn(24, Some(30), None).await.expect("fonts"),
            file_was_30,
            "nothing published: the box's own size"
        );
        assert_eq!(
            ticket_drawn(24, Some(30), Some(64)).await.expect("fonts"),
            file_was_30,
            "a published size out of bounds is not read"
        );
        assert_eq!(
            ticket_drawn(24, None, Some(12)).await.expect("fonts"),
            neither,
            "and with neither, 24"
        );
    }

    #[test]
    fn the_font_size_in_force_is_the_stores_then_the_boxs_then_the_default() {
        let renderer = pos_render::TextRenderer::new(
            pos_render::FontLibrary::new(),
            NonZeroU16::new(24).expect("positive"),
        );
        let session = |published: Option<i64>| EdgeSession {
            printing: PublishedPrinting {
                font_size_dots: published,
                ..PublishedPrinting::default()
            },
            ..EdgeSession::bootstrap()
        };
        let size = |local: Option<u16>, published: Option<i64>| {
            let (dots, source) = Printers::tcp()
                .with_local_font_size(local)
                .font_size(&session(published), &renderer);
            (dots.get(), source)
        };
        assert_eq!(size(None, Some(32)), (32, ValueSource::Published));
        assert_eq!(size(Some(30), Some(32)), (32, ValueSource::Published));
        assert_eq!(size(Some(30), None), (30, ValueSource::LocalFile));
        assert_eq!(size(Some(30), Some(64)), (30, ValueSource::LocalFile));
        assert_eq!(size(None, None), (24, ValueSource::Default));
        assert_eq!(
            size(Some(0), Some(12)),
            (24, ValueSource::Default),
            "a zero in the file draws nothing, so it is not read"
        );
    }

    /// A receipt printer and a store printing English under its Vietnamese (ADR-0160 decision 2),
    /// whose menu names the pho in English.
    fn a_bilingual_store(second: ReceiptSecondLanguage, display: &str) -> EdgeSession {
        let pho = MenuEntry::new(
            MenuItemId::new(Ulid::from_u128(1)),
            DisplayName::new("Phở bò"),
            Money::new(CurrencyCode::VND, 50_000),
            EdgeSession::standard_tax_class(),
        )
        .with_name_translations([("en".to_owned(), DisplayName::new("Beef pho"))].into());
        EdgeSession {
            display_language: Some(display.to_owned()),
            menu: MenuCatalog::new().with(pho),
            printing: PublishedPrinting {
                receipt_second_language: Open::from_known(second),
                ..PublishedPrinting::default()
            },
            ..session_with(PublishedDevices::new(vec![device(
                2,
                DeviceKind::Printer,
                None,
            )]))
        }
    }

    #[tokio::test]
    async fn a_bilingual_receipt_draws_what_its_printer_cannot_spell_and_sends_the_rest_as_text() {
        let Some((printers, recorder)) = recorder_with_fonts(true) else {
            return;
        };
        let session = a_bilingual_store(ReceiptSecondLanguage::English, "vi");
        let (lines, totals) = a_bilingual_bill();
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(1),
                7,
                &lines,
                &totals,
                None,
            )
            .await;

        assert_eq!(outcome, PrintOutcome::Printed);
        let written = recorder.written.lock().expect("the recorder");
        let bytes = written.first().expect("a receipt");
        // The Vietnamese lines, each label's English beside it included, go out as rasters...
        assert!(
            bytes
                .windows(4)
                .any(|window| window == [0x1D, 0x76, 0x30, 0x00]),
            "a bilingual receipt carries a raster"
        );
        // ...and the English the menu gave the pho, which the printer can spell, as text.
        assert!(bytes.windows(8).any(|window| window == b"Beef pho"));
    }

    #[tokio::test]
    async fn a_box_with_no_fonts_prints_a_bilingual_store_s_receipt_in_one_language_as_before() {
        // Vietnamese labels need fonts this box does not have, so the receipt it prints is the one
        // it printed before the store chose a second language, byte for byte.
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let lines = [sold("Beef pho", 1_000, vnd(50_000), vnd(50_000))];
        let totals = totals_of(vnd(50_000));
        let mut written = Vec::new();
        for second in [
            ReceiptSecondLanguage::Vietnamese,
            ReceiptSecondLanguage::None,
        ] {
            let (printers, recorder) = recorder(true);
            let session = a_bilingual_store(second, "en");
            let outcome = printers
                .print_receipt(
                    &session,
                    &STORE,
                    store_id(),
                    event_id(1),
                    7,
                    &lines,
                    &totals,
                    None,
                )
                .await;
            assert_eq!(outcome, PrintOutcome::Printed);
            written.push(recorder.written.lock().expect("the recorder").concat());
        }
        assert_eq!(written[0], written[1]);
    }

    #[test]
    fn a_printer_only_claims_it_prints_bitmaps_when_this_box_can_make_one() {
        // `prints_bitmaps` describes the framework's ability, not the hardware's: `GS v 0` is
        // universal on ESC/POS, so the printer is never the constraint. Claiming it while no font is
        // loaded would mean asking for a document nothing can render.
        let printer = device(1, DeviceKind::Printer, None);
        assert!(!assumed_capabilities(&printer, false).prints_bitmaps);
        assert!(assumed_capabilities(&printer, true).prints_bitmaps);
        // And a printer the console names no paper for is the 80 mm printer every printer was taken
        // to be: 42 characters a line and 576 dots at 203 dpi, and a cutter.
        let assumed = assumed_capabilities(&printer, true);
        assert_eq!(
            (
                assumed.columns.get(),
                assumed.dots_per_line.get(),
                assumed.cuts_paper
            ),
            (42, 576, true)
        );
    }

    /// The receipt printer, on the paper the console says it takes (ADR-0160 decision 2).
    fn on_paper(paper: PaperWidth, cuts_paper: Option<bool>) -> PublishedDevice {
        PublishedDevice {
            paper_width: Open::from_known(paper),
            cuts_paper,
            ..device(2, DeviceKind::Printer, None)
        }
    }

    #[test]
    fn a_printer_is_assumed_to_be_as_wide_and_to_cut_as_the_console_says() {
        let narrow = assumed_capabilities(&on_paper(PaperWidth::Millimetres58, Some(false)), true);
        assert_eq!(
            (
                narrow.columns.get(),
                narrow.dots_per_line.get(),
                narrow.cuts_paper
            ),
            (32, 384, false)
        );
        let laid_out_at_48 =
            assumed_capabilities(&on_paper(PaperWidth::Millimetres80Columns48, None), true);
        assert_eq!(
            (
                laid_out_at_48.columns.get(),
                laid_out_at_48.dots_per_line.get(),
                laid_out_at_48.cuts_paper
            ),
            (48, 576, true)
        );
    }

    #[tokio::test]
    async fn a_58_mm_printer_is_sent_a_384_dot_raster_and_a_bilingual_label_wrapped_at_32() {
        let Some((printers, recorder)) = recorder_with_fonts(true) else {
            return;
        };
        let rules: PublishedFees = serde_json::from_str(
            r#"{"fees": [{"fee_id": "00000000000000000000000001", "code": "SERVICE",
                 "display_name": "Phí dịch vụ", "display_name_translations": {"en": "Service charge"}}]}"#,
        )
        .expect("a fees node");
        let (lines, totals) = a_bilingual_bill();
        let mut printed = Vec::new();
        for (seed, paper) in [
            (1, PaperWidth::Millimetres58),
            (2, PaperWidth::Millimetres80),
        ] {
            let session = EdgeSession {
                devices: PublishedDevices::new(vec![on_paper(paper, None)]),
                fees: rules.clone(),
                ..a_bilingual_store(ReceiptSecondLanguage::English, "vi")
            };
            let outcome = printers
                .print_receipt(
                    &session,
                    &STORE,
                    store_id(),
                    event_id(seed),
                    7,
                    &lines,
                    &totals,
                    None,
                )
                .await;
            assert_eq!(outcome, PrintOutcome::Printed);
            let written = recorder.written.lock().expect("the recorder");
            printed.push(written.last().cloned().expect("a receipt"));
        }
        // `GS v 0` names a raster's width in bytes: 384 dots is 48 a row, and 576 is 72.
        let drawn_at = |bytes: &[u8], width: u8| {
            bytes
                .windows(6)
                .any(|window| window == [0x1D, 0x76, 0x30, 0x00, width, 0x00])
        };
        assert!(drawn_at(&printed[0], 48) && !drawn_at(&printed[0], 72));
        assert!(drawn_at(&printed[1], 72) && !drawn_at(&printed[1], 48));
        // The fee and its English share 80 mm paper's line. On 58 mm the English goes under it, and
        // being ASCII it goes as text.
        let says =
            |bytes: &[u8], text: &[u8]| bytes.windows(text.len()).any(|window| window == text);
        assert!(says(&printed[0], b"  Service charge"));
        assert!(!says(&printed[1], b"  Service charge"));
    }

    #[tokio::test]
    async fn a_printer_with_no_cutter_is_sent_no_cut_and_one_set_as_it_was_prints_as_it_did() {
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let lines = [sold("Beef pho", 1_000, vnd(50_000), vnd(50_000))];
        let totals = totals_of(vnd(50_000));
        let mut printed = Vec::new();
        for printer in [
            device(2, DeviceKind::Printer, None),
            on_paper(PaperWidth::Millimetres80, Some(true)),
            on_paper(PaperWidth::Millimetres80, Some(false)),
        ] {
            let (printers, recorder) = recorder(true);
            let session = session_with(PublishedDevices::new(vec![printer]));
            let outcome = printers
                .print_receipt(
                    &session,
                    &STORE,
                    store_id(),
                    event_id(1),
                    7,
                    &lines,
                    &totals,
                    None,
                )
                .await;
            assert_eq!(outcome, PrintOutcome::Printed);
            printed.push(recorder.written.lock().expect("the recorder").concat());
        }
        // `GS V 0`, the full cut.
        let cuts = |bytes: &[u8]| bytes.windows(3).any(|window| window == [0x1D, 0x56, 0x00]);
        assert!(
            cuts(&printed[0]),
            "a printer nobody set cuts, as every printer did"
        );
        assert_eq!(
            printed[0], printed[1],
            "and one set to what it was taken to be prints byte for byte as it did"
        );
        assert!(
            !cuts(&printed[2]),
            "a printer with no cutter is sent no cut"
        );
    }

    #[test]
    fn a_directly_attached_printer_gets_a_channel_rather_than_a_refusal() {
        // Before ADR-0103 this arm returned "this build talks to network printers only". A store
        // with a USB printer had no way to print at all, and a cash drawer — which may only ever
        // open over USB (architecture.md §5) — had no transport to open over.
        let usb = PublishedDevice {
            connection: DeviceConnection::Usb.into(),
            address: "/dev/usb/lp0".to_owned(),
            ..device(1, DeviceKind::Printer, None)
        };
        assert!(
            TcpTransports.open(&usb).is_ok(),
            "a USB printer should get a device channel"
        );

        let serial = PublishedDevice {
            connection: DeviceConnection::Serial.into(),
            address: "/dev/ttyUSB0".to_owned(),
            ..device(2, DeviceKind::Printer, None)
        };
        assert!(TcpTransports.open(&serial).is_ok(), "and so should serial");

        // Opening the channel is not reaching the printer: nothing is touched until the first write,
        // so a device path that does not exist fails where every other unreachable printer does.
        let network = device(3, DeviceKind::Printer, None);
        assert!(
            TcpTransports.open(&network).is_ok(),
            "and the LAN still works"
        );
    }

    // ---------------------------------------------------------------------
    // ADR-0165: a cash drawer opens through the printer the console marked.

    #[test]
    fn a_drawer_outcome_names_itself_with_a_stable_token() {
        assert_eq!(DrawerOutcome::Opened.as_wire(), "OPENED");
        assert_eq!(DrawerOutcome::NoDrawer.as_wire(), "NO_DRAWER");
        assert_eq!(DrawerOutcome::Unavailable.as_wire(), "DRAWER_UNAVAILABLE");
    }

    #[test]
    fn only_a_marked_usb_counter_printer_written_here_or_by_a_kicking_agent_is_a_drawer_printer() {
        let marked = drawer_device(1);
        let devices = PublishedDevices::new(vec![marked.clone()]);
        assert_eq!(drawer_printer(&devices, |_| false), Some(&marked));
        assert!(
            assumed_capabilities(&marked, false).may_open_a_drawer(),
            "the mark is what the adapter's own check reads"
        );

        let station = StationId::new(Ulid::from_u128(9));
        for (why, device) in [
            (
                "not marked",
                PublishedDevice {
                    drawer_attached: false,
                    ..drawer_device(2)
                },
            ),
            (
                "on the network, where port 9100 has no authentication",
                PublishedDevice {
                    connection: DeviceConnection::Network.into(),
                    ..drawer_device(3)
                },
            ),
            (
                "a kitchen station's printer",
                PublishedDevice {
                    station_id: Some(station),
                    ..drawer_device(4)
                },
            ),
            (
                "written through a print agent that does not carry the kick",
                PublishedDevice {
                    agent_device_id: Some(DeviceId::new(Ulid::from_u128(5))),
                    ..drawer_device(6)
                },
            ),
        ] {
            assert_eq!(
                drawer_printer(&PublishedDevices::new(vec![device]), |_| false),
                None,
                "{why}"
            );
        }

        // Behind an agent that carries the kick, the same printer is the drawer's (ADR-0167).
        let agent = DeviceId::new(Ulid::from_u128(5));
        let behind = PublishedDevice {
            agent_device_id: Some(agent),
            ..drawer_device(7)
        };
        let devices = PublishedDevices::new(vec![behind.clone()]);
        assert_eq!(
            drawer_printer(&devices, |kicking| kicking == agent),
            Some(&behind)
        );
    }

    #[tokio::test]
    async fn a_drawer_is_kicked_through_its_printer_and_nothing_else_is_written() {
        let (printers, written) = recorder(true);
        let outcome = printers
            .open_drawer(
                &session_with(PublishedDevices::new(vec![drawer_device(2)])),
                store_id(),
                event_id(0xD0),
            )
            .await;
        assert_eq!(outcome, DrawerOutcome::Opened);
        assert_eq!(
            *written.written.lock().expect("the recorder"),
            vec![printer_escpos::escpos::DRAWER_KICK.to_vec()],
            "one pulse on the kick pin, and no document"
        );

        let (printers, written) = recorder(true);
        let unmarked = PublishedDevice {
            drawer_attached: false,
            ..drawer_device(2)
        };
        let outcome = printers
            .open_drawer(
                &session_with(PublishedDevices::new(vec![unmarked])),
                store_id(),
                event_id(0xD0),
            )
            .await;
        assert_eq!(outcome, DrawerOutcome::NoDrawer);
        assert!(
            written.written.lock().expect("the recorder").is_empty(),
            "a store with no drawer sends nothing to its printer"
        );
    }

    #[tokio::test]
    async fn a_drawer_printer_that_does_not_answer_is_unavailable() {
        let (printers, _written) = recorder(false);
        let outcome = printers
            .open_drawer(
                &session_with(PublishedDevices::new(vec![drawer_device(2)])),
                store_id(),
                event_id(0xD0),
            )
            .await;
        assert_eq!(outcome, DrawerOutcome::Unavailable);
    }

    #[tokio::test]
    async fn a_drawer_marked_after_its_printer_was_opened_opens_without_a_restart() {
        // The printer is opened for a receipt before anybody ticks the box, so it is held with no
        // drawer. The next config pull marks it, and the mark must not wait for a restart.
        let (printers, written) = recorder(true);
        let unmarked = PublishedDevice {
            drawer_attached: false,
            ..drawer_device(2)
        };
        let receipt = printers
            .print_receipt(
                &session_with(PublishedDevices::new(vec![unmarked])),
                &STORE,
                store_id(),
                event_id(1),
                1,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 10_000)),
                None,
            )
            .await;
        assert_eq!(receipt, PrintOutcome::Printed);

        let outcome = printers
            .open_drawer(
                &session_with(PublishedDevices::new(vec![drawer_device(2)])),
                store_id(),
                event_id(0xD0),
            )
            .await;
        assert_eq!(outcome, DrawerOutcome::Opened);
        assert_eq!(
            written.written.lock().expect("the recorder").last(),
            Some(&printer_escpos::escpos::DRAWER_KICK.to_vec()),
            "the kick follows the receipt on the same printer"
        );
    }

    // ---------------------------------------------------------------------
    // ADR-0112: a printer whose transport belongs to another device.
    // ---------------------------------------------------------------------

    /// The three seams the agent lane needs, in memory, plus the agent's own id.
    struct Lane {
        queue: Arc<crate::print_queue::InMemoryPrintQueue>,
        agent: DeviceId,
    }

    /// A dispatcher whose printers are reached through an agent bound `heard_from_ms` ago, or that
    /// nobody holds at all when `heard_from_ms` is `None`.
    async fn with_agent_lane(heard_from_ms: Option<i64>) -> (Printers, Arc<Recorder>, Lane) {
        use crate::print_agent::PrintAgents as _;

        let (printers, recorder) = recorder(true);
        let agents = Arc::new(crate::print_agent::InMemoryPrintAgents::new());
        let queue = Arc::new(crate::print_queue::InMemoryPrintQueue::new());
        let agent = DeviceId::new(Ulid::from_u128(0xA6E7));
        if let Some(ago) = heard_from_ms {
            let now = crate::clock::SystemClock
                .now()
                .as_milliseconds_since_epoch();
            agents
                .claim(
                    agent,
                    DeviceId::new(Ulid::from_u128(0x00DE_71CE)),
                    now - ago,
                )
                .await
                .expect("the binding is recorded");
        }
        let printers = printers.with_agents(Arc::new(AgentLane::new(
            Arc::clone(&agents),
            Arc::clone(&queue),
            Arc::new(crate::print_wake::SharedPrintWake::new()),
        )));
        (printers, recorder, Lane { queue, agent })
    }

    /// What the agent would be handed if it asked right now.
    async fn claimable(lane: &Lane) -> Vec<crate::print_queue::ClaimedJob> {
        use crate::print_queue::PrintQueue as _;
        let now = crate::clock::SystemClock
            .now()
            .as_milliseconds_since_epoch();
        lane.queue
            .claim(lane.agent, now, now + 30_000)
            .await
            .expect("the queue answers")
    }

    /// A drawer printer behind `agent`: marked, on USB, serving the bill.
    fn drawer_via_agent(agent: DeviceId) -> PublishedDevice {
        PublishedDevice {
            agent_device_id: Some(agent),
            ..drawer_device(2)
        }
    }

    /// An agent that takes the first kick it finds on its queue within [`KICK_WAIT`], writes it
    /// and acknowledges it, as `pos_print_agent` does through the routes, and hands it back.
    async fn an_agent_kicks(
        printers: &Printers,
        lane: &Lane,
    ) -> Option<crate::print_queue::ClaimedJob> {
        use crate::print_queue::PrintQueue as _;
        let found = tokio::time::timeout(super::KICK_WAIT, async {
            loop {
                if let Some(kick) = claimable(lane)
                    .await
                    .into_iter()
                    .find(|claimed| claimed.job.document.is_kick())
                {
                    return kick;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .ok()?;
        let job = found.job.job_id;
        assert!(lane.queue.acknowledge(job, lane.agent).await.expect("ack"));
        printers.acknowledged(job);
        Some(found)
    }

    #[tokio::test]
    async fn a_drawer_behind_an_agent_that_carries_the_kick_opens_once_it_acknowledges() {
        // ADR-0167 decision 6: the kick travels inside a job, to the agent that owns the printer,
        // and the till hears `OPENED` when the agent says it wrote it.
        let (printers, recorder, lane) = with_agent_lane(Some(0)).await;
        printers.heard_from_agent(lane.agent, true);
        let session = session_with(PublishedDevices::new(vec![drawer_via_agent(lane.agent)]));

        let (outcome, kick) = tokio::join!(
            printers.open_drawer(&session, store_id(), event_id(0xD0)),
            an_agent_kicks(&printers, &lane)
        );
        assert_eq!(outcome, DrawerOutcome::Opened);
        let kick = kick.expect("the agent was sent a kick");
        assert_eq!(kick.printer, drawer_via_agent(lane.agent).device_id);
        assert_eq!(kick.job.job_id, event_id(0xD0), "under the id it was given");
        assert!(kick.job.document.is_kick());
        assert!(
            recorder.written.lock().expect("the recorder").is_empty(),
            "the edge never dials a printer an agent owns"
        );
        assert!(
            claimable(&lane).await.is_empty(),
            "one kick, and it is acknowledged"
        );
    }

    #[tokio::test]
    async fn an_agent_that_does_not_carry_the_kick_is_sent_none_and_the_till_uses_the_key() {
        // An agent built before kicks claims without saying it carries one: ADR-0165's answer for a
        // printer the edge may not open a drawer through, and nothing on its queue.
        let (printers, _recorder, lane) = with_agent_lane(Some(0)).await;
        printers.heard_from_agent(lane.agent, false);
        let session = session_with(PublishedDevices::new(vec![drawer_via_agent(lane.agent)]));
        assert_eq!(
            printers
                .open_drawer(&session, store_id(), event_id(0xD0))
                .await,
            DrawerOutcome::NoDrawer
        );
        assert!(claimable(&lane).await.is_empty());

        // One that said so and then said otherwise, an agent rolled back, is the same.
        printers.heard_from_agent(lane.agent, true);
        printers.heard_from_agent(lane.agent, false);
        assert_eq!(
            printers
                .open_drawer(&session, store_id(), event_id(0xD0))
                .await,
            DrawerOutcome::NoDrawer
        );
        assert!(claimable(&lane).await.is_empty());
    }

    #[tokio::test]
    async fn a_kicking_agent_that_is_silent_or_unbound_leaves_the_drawer_unavailable() {
        // The drawer printer was chosen and its agent cannot take the kick: queue nothing, and say
        // so at once rather than after a wait.
        let (printers, _recorder, lane) = with_agent_lane(None).await;
        printers.heard_from_agent(lane.agent, true);
        let session = session_with(PublishedDevices::new(vec![drawer_via_agent(lane.agent)]));
        assert_eq!(
            printers
                .open_drawer(&session, store_id(), event_id(0xD0))
                .await,
            DrawerOutcome::Unavailable
        );
        assert!(
            claimable(&lane).await.is_empty(),
            "nothing builds behind a box that is not there"
        );
    }

    #[tokio::test]
    async fn a_kick_is_heard_only_while_the_till_waits_and_the_waiters_are_bounded() {
        let (printers, _recorder, _lane) = with_agent_lane(Some(0)).await;
        let first = event_id(0x0001);
        let waiting = printers.await_kick(first).expect("room to wait");
        printers.acknowledged(first);
        assert!(waiting.within(std::time::Duration::from_secs(1)).await);

        let unheard = printers.await_kick(event_id(0x0002)).expect("room to wait");
        assert!(!unheard.within(std::time::Duration::from_millis(10)).await);
        assert!(
            printers.awaited.lock().expect("lock").is_empty(),
            "a till that stopped waiting leaves nothing behind"
        );

        let held: Vec<_> = (0..KICKS_AWAITED)
            .map(|n| printers.await_kick(event_id(0x1000 + n as u128)))
            .collect();
        assert!(held.iter().all(Option::is_some));
        assert!(
            printers.await_kick(event_id(0x2000)).is_none(),
            "past the bound a kick is refused"
        );
    }

    #[test]
    fn the_agents_said_to_carry_a_kick_are_bounded() {
        let (printers, _recorder) = recorder(true);
        for n in 0..=KICKING_AGENTS {
            printers.heard_from_agent(DeviceId::new(Ulid::from_u128(0x5000 + n as u128)), true);
        }
        let last = DeviceId::new(Ulid::from_u128(0x5000 + KICKING_AGENTS as u128));
        assert_eq!(printers.kicking.lock().expect("lock").len(), KICKING_AGENTS);
        assert!(
            printers.carries_kick(last),
            "the newest is remembered, and one older forgotten"
        );
    }

    #[tokio::test]
    async fn a_printer_that_names_a_live_agent_is_queued_rather_than_dialled() {
        // The last hop moves and nothing above it does: the document is rendered here, and what
        // reaches the queue is the finished job.
        let (printers, recorder, lane) = with_agent_lane(Some(0)).await;
        let session = session_with(PublishedDevices::new(vec![device_via_agent(
            2, None, lane.agent,
        )]));
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(7),
                41,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 90_000)),
                None,
            )
            .await;

        assert_eq!(outcome, PrintOutcome::QueuedToAgent);
        assert_eq!(outcome.as_wire(), "QUEUED_TO_AGENT");
        assert!(
            !outcome.printed(),
            "no paper has come out; the till must not claim it has"
        );
        assert!(
            recorder.written.lock().expect("lock").is_empty(),
            "the edge must not also dial a printer it handed to an agent"
        );

        let queued = claimable(&lane).await;
        assert_eq!(queued.len(), 1, "one job, for one printer");
        assert_eq!(queued[0].job.job_id, event_id(7));
        assert!(
            lines_of(&queued[0].job.document)
                .iter()
                .any(|line| line.contains("41")),
            "the agent receives the rendered receipt, not the ingredients for one"
        );
    }

    #[tokio::test]
    async fn a_printer_with_no_agent_still_opens_its_own_transport() {
        // The absent field is the whole compatibility story: a lane being composed changes nothing
        // for a store that configured no agent.
        let (printers, recorder, _lane) = with_agent_lane(Some(0)).await;
        let session = session_with(PublishedDevices::new(vec![device(
            2,
            DeviceKind::Printer,
            None,
        )]));
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(8),
                42,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 10_000)),
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::Printed);
        assert!(!recorder.written.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn an_unbound_or_silent_agent_is_refused_before_the_queue_is_touched() {
        // ADR-0112's ordering: a queue must not start building behind a box that is not there.
        for heard_from in [
            None,
            Some(i64::try_from(crate::print_agent::AGENT_SILENCE.as_millis()).expect("fits") + 1),
        ] {
            let (printers, recorder, lane) = with_agent_lane(heard_from).await;
            let session = session_with(PublishedDevices::new(vec![device_via_agent(
                2, None, lane.agent,
            )]));
            let outcome = printers
                .print_receipt(
                    &session,
                    &STORE,
                    store_id(),
                    event_id(9),
                    43,
                    &[],
                    &totals_of(Money::new(CurrencyCode::VND, 10_000)),
                    None,
                )
                .await;
            assert_eq!(outcome, PrintOutcome::AgentUnavailable);
            assert_eq!(outcome.as_wire(), "PRINT_AGENT_UNAVAILABLE");
            assert!(
                claimable(&lane).await.is_empty(),
                "nothing was written: the refusal comes before the table"
            );
            assert!(
                recorder.written.lock().expect("lock").is_empty(),
                "and the edge does not fall back to dialling the printer itself"
            );
        }
    }

    #[tokio::test]
    async fn a_printer_whose_agent_is_alive_but_not_consuming_fills_and_then_refuses() {
        // The state the cap exists for: a *live* agent whose printer is not consuming. Adding one
        // more job is promising paper that is not coming, so the enqueue refuses and the operator
        // learns it at the till rather than from the absence of a ticket.
        use crate::print_queue::PrintQueue as _;

        let (printers, _recorder, lane) = with_agent_lane(Some(0)).await;
        let printer = device_via_agent(2, None, lane.agent);
        let now = crate::clock::SystemClock
            .now()
            .as_milliseconds_since_epoch();
        for filler in 0..crate::print_agent::MAX_QUEUED_PER_PRINTER {
            lane.queue
                .enqueue(
                    lane.agent,
                    printer.device_id,
                    PrintJob {
                        job_id: event_id(u128::from(filler) + 1_000),
                        store_id: store_id(),
                        station_id: None,
                        document: pos_ports::printer::PrintDocument { blocks: Vec::new() },
                    },
                    now,
                    now + 600_000,
                    crate::print_agent::MAX_QUEUED_PER_PRINTER,
                )
                .await
                .expect("the queue fills");
        }

        let session = session_with(PublishedDevices::new(vec![printer]));
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(10),
                44,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 10_000)),
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::QueueFull);
        assert_eq!(outcome.as_wire(), "PRINT_QUEUE_FULL");
    }

    #[tokio::test]
    async fn a_named_agent_with_no_queue_composed_refuses_rather_than_dialling() {
        // A route test or the fakes-backed example. Opening the address anyway is the wrong
        // direction: in a hosted edge placement it is a device path that is not on this machine.
        let (printers, recorder) = recorder(true);
        let session = session_with(PublishedDevices::new(vec![device_via_agent(
            2,
            None,
            DeviceId::new(Ulid::from_u128(0xA6E7)),
        )]));
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(11),
                45,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 10_000)),
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::AgentUnavailable);
        assert!(recorder.written.lock().expect("lock").is_empty());
    }

    // --- A till's own receipt printer and languages (ADR-0160 decision 4) ----------------------

    /// What each printer was sent, by its address, so a test can say at which counter a document
    /// came out. The printer at `down`, if any, does not answer.
    #[derive(Debug, Default)]
    struct Counters {
        sent: Mutex<Vec<(String, Vec<u8>)>>,
        down: Option<&'static str>,
    }

    impl Counters {
        /// The addresses written to, in order.
        fn at(&self) -> Vec<String> {
            let sent = self.sent.lock().expect("the counters");
            sent.iter().map(|(address, _)| address.clone()).collect()
        }

        /// Everything the printer at `address` was sent, in one.
        fn bytes_at(&self, address: &str) -> Vec<u8> {
            let sent = self.sent.lock().expect("the counters");
            sent.iter()
                .filter(|(at, _)| at == address)
                .flat_map(|(_, bytes)| bytes.clone())
                .collect()
        }
    }

    #[derive(Debug)]
    struct CountersFactory(Arc<Counters>);

    #[derive(Debug)]
    struct CounterTransport {
        counters: Arc<Counters>,
        address: String,
    }

    impl Transport for CounterTransport {
        fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
            if self.counters.down == Some(self.address.as_str()) {
                return Err(Unreachable);
            }
            self.counters
                .sent
                .lock()
                .map_err(|_| Unreachable)?
                .push((self.address.clone(), bytes.to_vec()));
            Ok(())
        }

        fn probe(&self) -> Result<TransportStatus, Unreachable> {
            if self.counters.down == Some(self.address.as_str()) {
                Err(Unreachable)
            } else {
                Ok(TransportStatus::default())
            }
        }
    }

    impl TransportFactory for CountersFactory {
        fn open(&self, device: &PublishedDevice) -> Result<Box<dyn Transport>, PortError> {
            Ok(Box::new(CounterTransport {
                counters: Arc::clone(&self.0),
                address: device.address.clone(),
            }))
        }
    }

    const COUNTER: &str = "192.0.2.2:9100";
    const BAR: &str = "192.0.2.3:9100";

    fn paired(seed: u128) -> DeviceId {
        DeviceId::new(Ulid::from_u128(seed))
    }

    /// A terminal `seed` naming `printer` as its receipt printer and `language` as its language.
    fn till(
        seed: u128,
        printer: Option<u128>,
        language: Option<ReceiptLanguage>,
    ) -> PublishedDevice {
        PublishedDevice {
            receipt_printer_id: printer.map(paired),
            receipt_language: language.map_or_else(Open::default, Open::from_known),
            ..device(seed, DeviceKind::Terminal, None)
        }
    }

    /// The store's devices: its receipt printer at the counter, a printer at the bar, a kitchen's,
    /// and four tills. The bar till (7) names the bar's printer; the door till (8) names nothing;
    /// the terrace till (9) names the kitchen's, and the patio till (10) a printer the node lacks.
    fn the_tills() -> PublishedDevices {
        PublishedDevices::new(vec![
            device(2, DeviceKind::Printer, None),
            device(3, DeviceKind::Printer, None),
            device(4, DeviceKind::Printer, Some(station(9))),
            till(7, Some(3), None),
            till(8, None, None),
            till(9, Some(4), None),
            till(10, Some(5), None),
        ])
    }

    /// A dispatcher over `counters` whose binding record holds each till for one paired device:
    /// the bar till for `0xBA2`, the door till for `0xD002`, the terrace till for `0x7E2` and the
    /// patio till for `0xFA7`.
    async fn with_tills(counters: &Arc<Counters>) -> Printers {
        use crate::print_agent::PrintAgents as _;
        let agents = Arc::new(crate::print_agent::InMemoryPrintAgents::new());
        let now = crate::clock::SystemClock
            .now()
            .as_milliseconds_since_epoch();
        for (terminal, device) in [(7, 0xBA2), (8, 0xD002), (9, 0x7E2), (10, 0xFA7)] {
            agents
                .claim(paired(terminal), paired(device), now)
                .await
                .expect("the binding is recorded");
        }
        Printers::over(Arc::new(CountersFactory(Arc::clone(counters)))).with_agents(Arc::new(
            AgentLane::new(
                agents,
                Arc::new(crate::print_queue::InMemoryPrintQueue::new()),
                Arc::new(crate::print_wake::SharedPrintWake::new()),
            ),
        ))
    }

    /// Prints a receipt for the till `device` is, and answers what came of it.
    async fn receipt_from(
        printers: &Printers,
        session: &EdgeSession,
        device: u128,
    ) -> PrintOutcome {
        let till = printers.till_for(session, paired(device)).await;
        printers
            .print_receipt(
                session,
                &till,
                store_id(),
                event_id(device),
                42,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 99_000)),
                None,
            )
            .await
    }

    #[tokio::test]
    async fn a_till_bound_to_a_terminal_with_its_own_printer_prints_there_and_the_rest_at_the_stores()
     {
        let counters = Arc::new(Counters::default());
        let printers = with_tills(&counters).await;
        let session = session_with(the_tills());
        for (device, expected, why) in [
            (0xBA2, BAR, "the bar till prints at the bar"),
            (
                0xD002,
                COUNTER,
                "a till whose terminal names nothing prints at the store's",
            ),
            (
                0x0FF,
                COUNTER,
                "a device bound to no terminal prints at the store's",
            ),
            (
                0x7E2,
                COUNTER,
                "a kitchen's printer is not a receipt printer",
            ),
            (
                0xFA7,
                COUNTER,
                "a printer the node does not list is the store's case",
            ),
        ] {
            assert_eq!(
                receipt_from(&printers, &session, device).await,
                PrintOutcome::Printed,
                "{why}"
            );
            assert_eq!(
                counters.at().last().map(String::as_str),
                Some(expected),
                "{why}"
            );
        }
        assert_eq!(
            counters.at().len(),
            5,
            "one receipt each, and no other paper"
        );
    }

    #[tokio::test]
    async fn a_tills_printer_that_does_not_answer_is_reported_and_nothing_prints_at_the_counter() {
        // No fallback: the bar's bill at the counter is worse than a failure the cashier reads.
        let counters = Arc::new(Counters {
            down: Some(BAR),
            ..Counters::default()
        });
        let printers = with_tills(&counters).await;
        let session = session_with(the_tills());
        assert_eq!(
            receipt_from(&printers, &session, 0xBA2).await,
            PrintOutcome::Unavailable
        );
        assert!(
            counters.at().is_empty(),
            "nothing came out anywhere: {:?}",
            counters.at()
        );
    }

    /// A binding record that cannot be read.
    struct Unreadable;

    impl super::AgentDispatch for Unreadable {
        fn enqueue<'a>(
            &'a self,
            _agent: DeviceId,
            _printer: DeviceId,
            _job: PrintJob,
        ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
            Box::pin(async { PrintOutcome::AgentUnavailable })
        }

        fn terminal_of<'a>(
            &'a self,
            _device: DeviceId,
        ) -> Pin<Box<dyn Future<Output = Result<Option<DeviceId>, PortError>> + Send + 'a>>
        {
            Box::pin(async {
                Err(PortError::unavailable(
                    PortName::EventStore,
                    "the binding cannot be read",
                ))
            })
        }
    }

    #[tokio::test]
    async fn a_till_that_cannot_be_told_is_refused_only_where_a_till_prints_its_own() {
        let counters = Arc::new(Counters::default());
        let printers = Printers::over(Arc::new(CountersFactory(Arc::clone(&counters))))
            .with_agents(Arc::new(Unreadable));
        // A terminal names its own printer, so which till this is decides the counter.
        let session = session_with(the_tills());
        assert_eq!(
            receipt_from(&printers, &session, 0xBA2).await,
            PrintOutcome::Unavailable
        );
        assert!(counters.at().is_empty(), "not guessed at the counter");
        // No terminal names anything: the binding is not read, and the store prints as before.
        let session = session_with(PublishedDevices::new(vec![
            device(2, DeviceKind::Printer, None),
            till(8, None, None),
        ]));
        assert_eq!(
            receipt_from(&printers, &session, 0xBA2).await,
            PrintOutcome::Printed
        );
        assert_eq!(counters.at(), [COUNTER]);
    }

    #[tokio::test]
    async fn a_tills_language_prints_its_receipt_in_that_language_and_the_others_in_the_stores() {
        // An English store whose menu names the pho in Vietnamese, ASCII here so that a box with no
        // fonts prints it, and a bar till that prints its receipts in Vietnamese.
        let item = MenuItemId::new(Ulid::from_u128(1));
        let session = EdgeSession {
            display_language: Some("en".to_owned()),
            menu: MenuCatalog::new().with(
                MenuEntry::new(
                    item,
                    DisplayName::new("Beef pho"),
                    Money::new(CurrencyCode::VND, 65_000),
                    EdgeSession::standard_tax_class(),
                )
                .with_name_translations([("vi".to_owned(), DisplayName::new("Pho bo"))].into()),
            ),
            ..session_with(PublishedDevices::new(vec![
                device(2, DeviceKind::Printer, None),
                till(7, None, Some(ReceiptLanguage::Vietnamese)),
            ]))
        };
        let price = Money::new(CurrencyCode::VND, 65_000);
        let lines = [ReceiptLine {
            menu_item_id: item,
            ..sold("Beef pho", 1_000, price, price)
        }];
        let counters = Arc::new(Counters::default());
        let printers = with_tills(&counters).await;
        for (device, name) in [(0xBA2, "Pho bo"), (0xD002, "Beef pho")] {
            let till = printers.till_for(&session, paired(device)).await;
            let outcome = printers
                .print_receipt(
                    &session,
                    &till,
                    store_id(),
                    event_id(device),
                    42,
                    &lines,
                    &totals_of(price),
                    None,
                )
                .await;
            assert_eq!(outcome, PrintOutcome::Printed);
            let bytes = counters.bytes_at(COUNTER);
            assert!(
                bytes
                    .windows(name.len())
                    .any(|window| window == name.as_bytes()),
                "{name}: the till's language decides the names, at the store's printer"
            );
            counters.sent.lock().expect("the counters").clear();
        }
        assert_eq!(
            receipt_language(
                &session,
                &TillPrinting::of(&till(7, None, Some(ReceiptLanguage::Country)))
            )
            .as_deref(),
            Some("en"),
            "resolved as the store's own choice is: a country with no language on the node"
        );
    }

    #[test]
    fn a_tills_second_language_replaces_the_stores_and_none_is_a_choice() {
        let (lines, totals) = a_bilingual_bill();
        let session = EdgeSession {
            display_language: Some("vi".to_owned()),
            printing: PublishedPrinting {
                receipt_second_language: Open::from_known(ReceiptSecondLanguage::English),
                ..PublishedPrinting::default()
            },
            ..EdgeSession::bootstrap()
        };
        let second = |till: &PublishedDevice| {
            SecondLanguage::for_receipt(
                &session,
                &TillPrinting::of(till),
                &VIETNAMESE,
                true,
                42,
                &lines,
                &lines,
                &totals,
            )
            .map(|second| second.labels)
        };
        let one_language = PublishedDevice {
            receipt_second_language: Open::from_known(ReceiptSecondLanguage::None),
            ..till(7, None, None)
        };
        assert_eq!(second(&till(8, None, None)), Some(&ENGLISH), "the store's");
        assert_eq!(second(&one_language), None, "this till prints one language");
    }
}
