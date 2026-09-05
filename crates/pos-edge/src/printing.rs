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
//! # A drawer is not a decision this module gets to make
//!
//! [`PrinterConnection::may_open_a_drawer`](pos_ports::printer::PrinterConnection::may_open_a_drawer)
//! is false for anything but USB, because port 9100 has no authentication and the drawer-kick rides
//! the same channel as everything else (`docs/architecture.md` §5). A published node naming a network
//! printer with a drawer is accepted here and the drawer command is still not sent.

use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};

use printer_escpos::tcp::TcpTransport;
use printer_escpos::{EscPosPrinter, Transport};

use pos_ports::printer::{
    CodePage, PrintBlock, PrintDocument, PrintJob, PrinterCapabilities, PrinterConnection,
    TextStyle,
};
use pos_ports::{PortError, PortName};
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::floor::StationPlan;
use pos_proto::ids::{DeviceId, EventId, MenuItemId, StationId, StoreId};
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;

use crate::app::{EdgeSession, FiredLine};

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

/// The printer serving `station`, falling back to the station plan's declared backup.
///
/// The failover target is the *plan's*, not this module's guess: `KitchenStation::backup_station_id`
/// is what an operator authored and `pos_core::floor` already validates (it must name a different
/// station in the same plan). One hop only — a backup chain that looped would print a ticket
/// somewhere nobody expected, and a ticket printed in the wrong kitchen is worse than one not
/// printed at all, because nobody goes looking for it.
#[must_use]
pub fn station_printer<'a>(
    devices: &'a PublishedDevices,
    plan: &StationPlan,
    station: StationId,
) -> Option<&'a PublishedDevice> {
    if let Some(direct) = printer_for_station(devices, station) {
        return Some(direct);
    }
    let backup = plan
        .stations()
        .iter()
        .find(|entry| entry.station_id == station)
        .and_then(|entry| entry.backup_station_id)?;
    printer_for_station(devices, backup)
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

/// The customer's receipt for a settled bill.
///
/// Deliberately plain: an optional header, the receipt number, the total, and a cut.
/// `docs/pos-spec.md` §12 and the country's invoice rules decide what a *legal* invoice carries, and
/// that is [`pos_ports::Fiscalization`]'s question, not this one — a receipt is the paper the guest
/// walks out with, and the gapless number on it is explicitly not an invoice number (ADR-0025).
///
/// `header` is the store's name. It is optional because **nothing on the edge knows it yet**: no
/// config node carries a store's display name, so the dispatcher passes `None` and the receipt starts
/// at its number. Printing a blank line, or inventing a name, would both be worse than omitting one.
#[must_use]
pub fn receipt_document(header: Option<&str>, receipt_number: u64, total: Money) -> PrintDocument {
    let mut blocks = Vec::new();
    if let Some(header) = header {
        blocks.push(PrintBlock::Text {
            line: header.to_owned(),
            style: TextStyle {
                emphasised: true,
                double_size: false,
                centred: true,
            },
        });
    }
    blocks.extend([
        PrintBlock::Text {
            line: format!("#{receipt_number}"),
            style: TextStyle {
                emphasised: false,
                double_size: false,
                centred: true,
            },
        },
        PrintBlock::Text {
            line: format_money(total),
            style: TextStyle {
                emphasised: true,
                double_size: true,
                centred: true,
            },
        },
        PrintBlock::Cut,
    ]);
    PrintDocument { blocks }
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
}

/// The ticket a station's printer produces when a line fires.
///
/// `reference` is what the kitchen calls back across the pass — a table's name, or the tail of the
/// order's id when there is no table. Double-size, because it is read from a metre away over a hot
/// counter; the item is emphasised; modifiers follow indented so a cook scanning the ticket sees
/// "what" before "how".
#[must_use]
pub fn ticket_document(reference: &str, line: &TicketLine) -> PrintDocument {
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
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

/// The ticket a fired line prints, with every id resolved against the store's published menu.
///
/// An item the menu does not name falls back to its identifier rather than to a blank: a cook can
/// still match a ticket to a screen, and a silently empty line is how the wrong dish gets made. The
/// same rule applies to each modifier.
#[must_use]
pub fn ticket_line(session: &EdgeSession, fired: &FiredLine) -> TicketLine {
    TicketLine {
        item: item_name(session, fired.menu_item_id),
        quantity: format_quantity(fired.quantity),
        modifiers: fired
            .modifier_menu_item_ids
            .iter()
            .map(|modifier| item_name(session, *modifier))
            .collect(),
    }
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

fn item_name(session: &EdgeSession, item: MenuItemId) -> String {
    session.menu.get(item).map_or_else(
        || item.to_string(),
        |entry| entry.display_name.as_str().to_owned(),
    )
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

/// Renders money as minor units beside its currency — `VND 99000`.
///
/// Minor units, not a decimal: the decimal place is a locale question (§14) and a receipt that
/// invented one would be wrong in half the countries this framework targets. The currency code makes
/// the unit unambiguous, which a bare number would not.
fn format_money(amount: Money) -> String {
    format!("{} {}", amount.currency_code, amount.amount_minor)
}

/// What came of a print attempt, as the till reports it.
///
/// Four outcomes, and three of them are not errors. The till has spent every release since P5 saying
/// "Printing receipt…" over a store with no printer wired at all; naming what actually happened is
/// most of what this slice is for (ADR-0100).
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
        }
    }

    /// Whether paper came out.
    #[must_use]
    pub const fn printed(self) -> bool {
        matches!(self, Self::Printed)
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
            // Named, not silently treated as a network printer: dialling port 9100 at a USB
            // printer's "address" would fail with a message about the network, which is the wrong
            // thing to hand an operator holding a USB cable.
            PrinterConnection::Usb | PrinterConnection::Serial => {
                Err(PortError::failed_precondition(
                    PortName::PrinterDriver,
                    "this build talks to network printers only; USB and serial need hardware bring-up",
                ))
            }
            // `PrinterConnection` is `#[non_exhaustive]`.
            _ => Err(PortError::failed_precondition(
                PortName::PrinterDriver,
                "this build has no transport for that connection",
            )),
        }
    }
}

/// Characters per line this build assumes of a printer it has never talked to.
///
/// 42 is the 80mm standard. Nothing reads it yet — the documents here are single lines that a printer
/// wraps on its own — so a 58mm printer is not mis-served by it today; a column-aware receipt layout
/// is what would need the real figure, and that needs the number to come from the console.
const ASSUMED_COLUMNS: u16 = 42;

/// What this build assumes about a published printer.
///
/// A device proposal carries an address, a kind and — since ADR-0100's approval change — a
/// connection. It does not carry a code page, a paper width or whether a drawer is wired to it, and
/// no discovery protocol reports them. So every unknown is set to the answer that cannot produce a
/// wrong receipt:
///
/// - `code_page: Ascii` — the repertoire every ESC/POS printer has. Claiming more would print
///   question marks; claiming [`CodePage::Unsupported`] would refuse plain ASCII too.
/// - `prints_bitmaps: false` — this build cannot *produce* a bitmap, so claiming the printer could
///   accept one would only mean asking for a document nothing can render.
/// - `kicks_drawer: false` — no drawer is opened from here at all. ADR-0100 is explicit that the
///   drawer is not this module's decision.
fn assumed_capabilities(device: &PublishedDevice) -> PrinterCapabilities {
    PrinterCapabilities {
        connection: connection_of(device),
        code_page: CodePage::Ascii,
        columns: NonZeroU16::new(ASSUMED_COLUMNS).unwrap_or(NonZeroU16::MIN),
        prints_bitmaps: false,
        cuts_paper: true,
        kicks_drawer: false,
    }
}

/// The store's printers, and the dispatch that puts a document on one.
///
/// Holds one [`EscPosPrinter`] per device for the life of the process, which is what keeps a socket
/// open between receipts and what makes the adapter's idempotency set mean anything: a retry of the
/// same `job_id` prints once.
pub struct Printers {
    transports: Arc<dyn TransportFactory>,
    open: Mutex<HashMap<DeviceId, HeldPrinter>>,
}

/// One printer held open for the life of the process.
type HeldPrinter = Arc<EscPosPrinter<Box<dyn Transport>>>;

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
        }
    }

    /// Prints the guest's receipt for a settled bill on the store's receipt printer.
    ///
    /// `job_id` is the idempotency key: pass the settle's event id and a retried settle reprints
    /// nothing. Never returns an error — a printer that is down must not roll back a bill the guest
    /// has already paid, so the outcome is reported and the caller decides what to tell the cashier.
    pub async fn print_receipt(
        &self,
        session: &EdgeSession,
        store_id: StoreId,
        job_id: EventId,
        receipt_number: u64,
        total: Money,
    ) -> PrintOutcome {
        let Some(device) = receipt_printer(&session.devices) else {
            tracing::info!("no receipt printer is published for this store; nothing to print");
            return PrintOutcome::NoPrinter;
        };
        // `None`: no config node carries the store's display name yet — see `receipt_document`.
        let document = receipt_document(None, receipt_number, total);
        self.dispatch(
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

    /// Prints a kitchen ticket at `station`, or at the station plan's declared backup.
    ///
    /// The failover is the plan's (ADR-0072, ADR-0100): one hop, to the station an operator named.
    pub async fn print_ticket(
        &self,
        session: &EdgeSession,
        store_id: StoreId,
        job_id: EventId,
        station: StationId,
        reference: &str,
        line: &TicketLine,
    ) -> PrintOutcome {
        let Some(device) = station_printer(&session.devices, &session.stations, station) else {
            tracing::info!(
                "no printer is published for that station or its backup; the kitchen display still has the order"
            );
            return PrintOutcome::NoPrinter;
        };
        let document = ticket_document(reference, line);
        self.dispatch(
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: Some(station),
                document,
            },
        )
        .await
    }

    /// Sends one job to one device.
    async fn dispatch(&self, device: &PublishedDevice, job: PrintJob) -> PrintOutcome {
        let capabilities = assumed_capabilities(device);
        if !renderable(&capabilities, &job.document) {
            tracing::warn!(
                device = %device.device_id,
                "the document needs a character this printer cannot render as text and this build cannot rasterise"
            );
            return PrintOutcome::Unprintable;
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

    /// The held printer for `device`, opening a channel the first time.
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
        if let Some(held) = open.get(&device.device_id) {
            return Ok(Arc::clone(held));
        }
        let transport = self.transports.open(device)?;
        let printer = Arc::new(EscPosPrinter::new(capabilities, transport));
        open.insert(device.device_id, Arc::clone(&printer));
        Ok(printer)
    }
}

/// Whether every text line in `document` can be sent to a printer with these capabilities.
///
/// The framework's half of the port's contract (ADR-0026 §5): the adapter sends `Text` as text, so
/// deciding a line is sendable is the caller's job and getting it wrong prints garbage.
fn renderable(capabilities: &PrinterCapabilities, document: &PrintDocument) -> bool {
    document.blocks.iter().all(|block| match block {
        PrintBlock::Text { line, .. } => capabilities.needs_bitmap(line) == Ok(false),
        _ => true,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        PrintOutcome, Printers, TicketLine, TransportFactory, connection_of, receipt_document,
        receipt_printer, short_reference, station_printer, ticket_document, ticket_line,
    };
    use crate::app::{EdgeSession, FiredLine};
    use pos_ports::PortError;
    use pos_ports::printer::{PrintBlock, PrinterConnection};
    use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
    use pos_proto::floor::{KitchenStation, StationPlan};
    use pos_proto::ids::{DeviceId, MenuItemId, StationId};
    use pos_proto::menu::{MenuCatalog, MenuEntry};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::quantity::Quantity;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use printer_escpos::{Transport, TransportStatus, Unreachable};
    use std::sync::{Arc, Mutex};

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
                store_id(),
                event_id(1),
                42,
                Money::new(CurrencyCode::VND, 99_000),
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
                store_id(),
                event_id(1),
                42,
                Money::new(CurrencyCode::VND, 99_000),
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
                store_id(),
                event_id(1),
                42,
                Money::new(CurrencyCode::VND, 99_000),
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
                .print_receipt(&session, store_id(), event_id(1), 42, total)
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
    async fn a_kitchen_ticket_a_printer_cannot_spell_is_refused_rather_than_printed_as_noise() {
        // The gap ADR-0100 leaves open: a Vietnamese item name needs CP1258 or a rasteriser and this
        // build has neither. Refusing is the only correct answer — question marks on a kitchen ticket
        // are how the wrong dish gets made. The KDS still shows the order.
        let (printers, recorder) = recorder(true);
        let oven = station(1);
        let session = EdgeSession {
            devices: PublishedDevices::new(vec![device(3, DeviceKind::Printer, Some(oven))]),
            stations: StationPlan::from_parts(
                vec![KitchenStation {
                    station_id: oven,
                    name: DisplayName::new("Oven"),
                    backup_station_id: None,
                }],
                Vec::new(),
                None,
            ),
            ..EdgeSession::bootstrap()
        };

        let outcome = printers
            .print_ticket(
                &session,
                store_id(),
                event_id(1),
                oven,
                "A1",
                &TicketLine {
                    item: "Bún chả".to_owned(),
                    quantity: "2".to_owned(),
                    modifiers: Vec::new(),
                },
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
            },
        );
        assert_eq!(
            lines_of(&document),
            vec!["A1", "2 x Margherita", "  + Extra cheese"]
        );
        assert_eq!(document.blocks.last(), Some(&PrintBlock::Cut));
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
            },
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
            },
        );
        assert_eq!(half.quantity, "0.5");
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
                },
                KitchenStation {
                    station_id: grill,
                    name: DisplayName::new("Grill"),
                    backup_station_id: None,
                },
            ],
            Vec::new(),
            None,
        );
        // Only the grill has a printer.
        let devices = PublishedDevices::new(vec![device(2, DeviceKind::Printer, Some(grill))]);
        let chosen = station_printer(&devices, &plan, oven).expect("the backup station's printer");
        assert_eq!(chosen.address, "192.0.2.2:9100");
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
                },
                KitchenStation {
                    station_id: right,
                    name: DisplayName::new("Right"),
                    backup_station_id: Some(left),
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
        unknown.connection = pos_proto::wire_enum::Open::parse("DEVICE_CONNECTION_INFRARED");
        assert_eq!(connection_of(&unknown), PrinterConnection::Network);
        assert!(
            !connection_of(&unknown).may_open_a_drawer(),
            "an unrecognised connection never authorises a cash drawer"
        );
    }

    #[test]
    fn a_receipt_with_no_store_name_starts_at_its_number_rather_than_a_blank_line() {
        // Nothing on the edge knows the store's name yet, and a blank first line on every receipt in
        // the fleet would be a worse answer than no line.
        let document = receipt_document(None, 7, Money::new(CurrencyCode::VND, 50_000));
        assert_eq!(lines_of(&document), vec!["#7", "VND 50000"]);
    }

    #[test]
    fn a_receipt_carries_the_store_the_number_and_the_total_and_ends_in_a_cut() {
        let document =
            receipt_document(Some("Bến Thành"), 42, Money::new(CurrencyCode::VND, 99_000));
        assert_eq!(lines_of(&document), vec!["Bến Thành", "#42", "VND 99000"]);
        assert_eq!(
            document.blocks.last(),
            Some(&PrintBlock::Cut),
            "the paper is cut, or the next receipt starts on this one"
        );
    }
}
