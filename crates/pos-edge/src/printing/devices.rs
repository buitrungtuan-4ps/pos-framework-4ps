// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which published device prints what: the receipt printer and a till's own, a station's printer,
//! the printer a drawer hangs from, how each is reached, and the printers and terminals published.

use pos_ports::printer::PrinterConnection;
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::floor::StationPlan;
use pos_proto::ids::{DeviceId, StationId};
use pos_proto::printing::{ReceiptLanguage, ReceiptSecondLanguage};

use crate::app::EdgeSession;
use crate::printing::PrintOutcome;

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
/// station's printer. A shift report goes to the store's receipt printer, or where the store keeps
/// a drawer per till to the receipt printer of the till it is closed at, in the store's language.
/// The cash drawer is [`drawer_printer`]'s choice: the store's, or where the store keeps a drawer
/// per till the receipt printer the till's entry names, whatever its paper prints in.
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
    pub(super) const UNKNOWN: Self = Self {
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
    pub(super) fn receipt_language(&self, session: &EdgeSession) -> ReceiptLanguage {
        self.receipt_language
            .unwrap_or_else(|| session.printing.receipt_language())
    }

    /// The second language this till's paper prints in: its own, or the store's `printing` node's.
    pub(super) fn receipt_second_language(&self, session: &EdgeSession) -> ReceiptSecondLanguage {
        self.receipt_second_language
            .unwrap_or_else(|| session.printing.receipt_second_language())
    }
}

/// Whether a terminal in `devices` names anything of its own to print with.
pub(super) fn a_till_prints_its_own(devices: &PublishedDevices) -> bool {
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
pub(super) fn guest_printer<'a>(
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
/// The store's one drawer, `till` `None`, opens through the first printer that serves the bill
/// rather than a station, that the console marked `drawer_attached`, whose connection may open a
/// drawer, which is USB and nothing else, and whose bytes this edge writes itself or a print agent
/// writes that `carries_kick` says carries the kick. Each condition is a way the kick would go
/// wrong: a station printer's drawer is not the till's, port 9100 has no authentication
/// (`docs/architecture.md` §5), and an agent built before kicks cannot read one.
///
/// A till's drawer, where the store keeps one per till, opens only through the receipt printer its
/// `TERMINAL` entry names, on the same conditions. A till that names none, or one that cannot open
/// a drawer, has no drawer the edge opens: the store's drawer never springs for a till's cash. The
/// one place the choice is made.
#[must_use]
pub fn drawer_printer(
    devices: &PublishedDevices,
    till: Option<DeviceId>,
    carries_kick: impl Fn(DeviceId) -> bool,
) -> Option<&PublishedDevice> {
    let springs = |device: &&PublishedDevice| {
        device.station_id.is_none()
            && is_printer(device)
            && device.drawer_attached
            && device.agent_device_id.is_none_or(&carries_kick)
            && connection_of(device).may_open_a_drawer()
    };
    let Some(till) = till else {
        return devices.devices().iter().find(springs);
    };
    let own = published_terminals(devices)
        .find(|terminal| terminal.device_id == till)?
        .receipt_printer_id?;
    devices
        .devices()
        .iter()
        .find(|device| device.device_id == own)
        .filter(springs)
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
