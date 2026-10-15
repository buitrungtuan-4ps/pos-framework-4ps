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
//! ([`drawer_printer`]). Where the store keeps a drawer per till, that is the receipt printer the
//! till's `TERMINAL` entry names, and a till that names none has no drawer the edge opens
//! ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
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

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};

use pos_render::TextRenderer;
use printer_escpos::{EscPosPrinter, Transport};

use pos_core::billing::BillTotals;
use pos_ports::printer::PrinterCapabilities;
use pos_proto::devices::PublishedDevice;
use pos_proto::ids::{DeviceId, EventId};

use tokio::sync::oneshot;

use crate::app::{EdgeSession, ReceiptLine};
use crate::config::{ValueSource, log_in_force};
use crate::paper_labels::PaperLabels;

mod agents;
mod blocks;
mod devices;
mod dispatch;
mod documents;
mod drawer;
mod jobs;
mod language;
mod tickets;
mod transports;

pub use agents::{AgentDispatch, AgentLane};
pub use blocks::{MoneyStyle, short_reference};
pub use devices::{
    TillPrinting, connection_of, drawer_printer, published_printers, published_terminals,
    receipt_printer, station_printer,
};
pub use documents::{
    combined_shift_report_document, pre_bill_document, receipt_copy_document, receipt_document,
    shift_report_document, test_page_document,
};
pub use language::{SecondLanguage, receipt_language, ticket_language};
pub use tickets::{TicketLine, TicketNote, ticket_at, ticket_document, ticket_line};
pub use transports::{TcpTransports, TransportFactory, assumed_capabilities};

// The unit tests below name these as `super::`, as they did when every item sat in this file.
#[cfg(test)]
use crate::print_agent::KICK_WAIT;
#[cfg(test)]
use blocks::printed_totals;
#[cfg(test)]
use language::{SecondNames, receipt_lines_in};

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

/// How many distinct missing characters a font warning names before it stops naming them.
///
/// The warning exists so an operator knows which font to install, and a handful of characters says
/// which script that is. The *whole* set says more than that: a receipt block carries a buyer's name
/// (`pos_ports::printer`), and on a box with no Vietnamese or CJK face every character of that name
/// substitutes — so an unbounded set in a durable log is the name itself, spelled with the
/// duplicates removed ([ADR-0117](../../../docs/adr/0117-a-headless-store-keeps-a-log.md)
/// decision 7). The count is reported alongside, so a truncation is visible as one.
const MISSING_GLYPHS_REPORTED: usize = 6;

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
}

#[cfg(test)]
mod tests {
    use super::TcpTransports;
    use super::{
        AgentLane, DrawerOutcome, KICKING_AGENTS, KICKS_AWAITED, MoneyStyle, PrintOutcome,
        Printers, SecondLanguage, SecondNames, TicketLine, TicketNote, TillPrinting,
        TransportFactory, assumed_capabilities, combined_shift_report_document, connection_of,
        drawer_printer, pre_bill_document, printed_totals, receipt_copy_document, receipt_document,
        receipt_language, receipt_lines_in, receipt_printer, shift_report_document,
        short_reference, station_printer, ticket_at, ticket_document, ticket_language, ticket_line,
    };
    use crate::app::{
        BuyerDetails, CombinedReport, DrawerFigures, EdgeSession, FiredLine, ReceiptLine,
        ShiftReport,
    };
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
            tax_components: Vec::new(),
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
            names(&printed_totals(&session, &STORE, &totals)),
            ["Phí phục vụ", "Cover charge", "Bag"]
        );
        // In English nothing is translated, so the receipt prints the names the bill froze.
        session.printing = printing_in(ReceiptLanguage::English);
        assert!(matches!(
            printed_totals(&session, &STORE, &totals),
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
            tax_components: Vec::new(),
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
    fn a_combined_slip_prints_the_store_totals_then_each_drawer_under_its_tills_name() {
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let drawer = |seed: u128, float: i64, counted: i64| ShiftReport {
            shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(seed)),
            opening_float: vnd(float),
            cash_collected: vnd(100_000),
            paid_in: vnd(0),
            paid_out: vnd(0),
            expected_amount: vnd(float + 100_000),
            counted_amount: vnd(counted),
            variance: vnd(counted - float - 100_000),
            tax_components: Vec::new(),
        };
        let bar = drawer(0xBA, 500_000, 590_000);
        let counter = drawer(0xC0, 300_000, 400_000);
        let combined = CombinedReport {
            totals: DrawerFigures {
                opening_float: vnd(800_000),
                cash_collected: vnd(200_000),
                paid_in: vnd(0),
                paid_out: vnd(0),
                expected_amount: vnd(1_000_000),
                counted_amount: vnd(990_000),
                variance: vnd(-10_000),
            },
            tax_components: Vec::new(),
            drawers: vec![
                (Some(DisplayName::new("Bar")), bar),
                (Some(DisplayName::new("Counter")), counter),
            ],
        };
        let slip = combined_shift_report_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            &combined,
        );
        let figures = |float: &'static str, expected: &'static str, counted: &'static str| {
            [float, "Cash taken  VND 100,000", expected, counted]
        };
        let lines = lines_of(&slip);
        assert_eq!(lines[..2], ["SHIFT REPORT", "STORE TOTALS"]);
        assert_eq!(
            lines[2..9],
            [
                "Opening float  VND 800,000",
                "Cash taken  VND 200,000",
                "Expected in drawer  VND 1,000,000",
                "Counted  VND 990,000",
                "Variance  VND -10,000",
                "Short",
                "Bar",
            ]
        );
        assert_eq!(
            lines[10..14],
            figures(
                "Opening float  VND 500,000",
                "Expected in drawer  VND 600,000",
                "Counted  VND 590,000"
            )
        );
        assert_eq!(lines[16], "Counter");
        assert_eq!(lines.last(), Some(&"Balanced"));
        assert_eq!(slip.blocks.last(), Some(&PrintBlock::Cut));
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
            tax_components: Vec::new(),
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
            ..report.clone()
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
            tax_components: Vec::new(),
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
        use pos_proto::locale::TaxComponentName;

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
                        name: TaxComponentName::new("CGST").expect("a token"),
                        rate_basis_points: 250,
                        tax: money(2_500),
                    },
                    TaxComponentLine {
                        name: TaxComponentName::new("SGST").expect("a token"),
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
        assert_eq!(drawer_printer(&devices, None, |_| false), Some(&marked));
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
                drawer_printer(&PublishedDevices::new(vec![device]), None, |_| false),
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
            drawer_printer(&devices, None, |kicking| kicking == agent),
            Some(&behind)
        );
    }

    #[test]
    fn a_tills_drawer_opens_only_through_the_receipt_printer_its_terminal_names() {
        let store_drawer = drawer_device(2);
        let bar_drawer = PublishedDevice {
            address: "/dev/usb/lp1".to_owned(),
            ..drawer_device(3)
        };
        let devices = PublishedDevices::new(vec![
            store_drawer.clone(),
            bar_drawer.clone(),
            PublishedDevice {
                drawer_attached: false,
                ..drawer_device(4)
            },
            till(7, Some(3), None),
            till(8, Some(4), None),
            till(9, None, None),
        ]);
        assert_eq!(
            drawer_printer(&devices, None, |_| false),
            Some(&store_drawer),
            "the store's one drawer, as before"
        );
        assert_eq!(
            drawer_printer(&devices, Some(paired(7)), |_| false),
            Some(&bar_drawer)
        );
        for (why, till) in [
            ("names a printer with no drawer", 8),
            ("names no printer", 9),
            ("is not in the node", 10),
        ] {
            assert_eq!(
                drawer_printer(&devices, Some(paired(till)), |_| false),
                None,
                "a till that {why} has no drawer, and the store's does not open for it"
            );
        }
    }

    #[tokio::test]
    async fn a_drawer_is_kicked_through_its_printer_and_nothing_else_is_written() {
        let (printers, written) = recorder(true);
        let outcome = printers
            .open_drawer(
                &session_with(PublishedDevices::new(vec![drawer_device(2)])),
                store_id(),
                event_id(0xD0),
                None,
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
                None,
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
                None,
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
                None,
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
            printers.open_drawer(&session, store_id(), event_id(0xD0), None),
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
                .open_drawer(&session, store_id(), event_id(0xD0), None)
                .await,
            DrawerOutcome::NoDrawer
        );
        assert!(claimable(&lane).await.is_empty());

        // One that said so and then said otherwise, an agent rolled back, is the same.
        printers.heard_from_agent(lane.agent, true);
        printers.heard_from_agent(lane.agent, false);
        assert_eq!(
            printers
                .open_drawer(&session, store_id(), event_id(0xD0), None)
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
                .open_drawer(&session, store_id(), event_id(0xD0), None)
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
