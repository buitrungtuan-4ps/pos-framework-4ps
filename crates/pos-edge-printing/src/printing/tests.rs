// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The printing module's unit tests, by area, and the fixtures they share: a store's devices, a
//! recorder that stands in for a printer, a session, and the money style.

use super::{MoneyStyle, Printers, TillPrinting, TransportFactory};
use crate::app::EdgeSession;
use pos_core::billing::BillTotals;
use pos_ports::PortError;
use pos_ports::printer::PrintBlock;
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::ids::{DeviceId, StationId};
use pos_proto::locale::NumberFormat;
use pos_proto::money::Money;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use printer_escpos::{Transport, TransportStatus, Unreachable};
use std::sync::{Arc, Mutex};

// Two of the tests below name these as `super::`, as they did when every test sat in one module.
use super::{AgentDispatch, KICK_WAIT};

mod agents;
mod devices;
mod drawer;
mod fonts;
mod jobs;
mod language;
mod layout;
mod receipts;
mod reports;
mod tickets;
mod tills;

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
