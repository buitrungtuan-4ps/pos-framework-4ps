// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Turning a settled bill into paper
//! ([ADR-0100](../../../../docs/adr/0100-receipt-and-ticket-printing.md), production-readiness C2).
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
//! ([ADR-0165](../../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)).
//! This module only performs it, through the printer the console marked `drawer_attached`
//! ([`drawer_printer`]). Where the store keeps a drawer per till, that is the receipt printer the
//! till's `TERMINAL` entry names, and a till that names none has no drawer the edge opens
//! ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
//! [`PrinterConnection::may_open_a_drawer`](pos_ports::printer::PrinterConnection::may_open_a_drawer)
//! is false for anything but USB, because port 9100 has no authentication and the drawer-kick rides
//! the same channel as everything else (`docs/architecture.md` §5). A published node marking a network
//! printer's drawer is accepted here and the drawer command is still not sent. Behind a print agent
//! the kick is a job of its own, sent only to an agent whose claim says it carries one, and `OPENED`
//! waits on the agent's acknowledgement
//! ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
//!
//! # A printer may name another device as the one that writes the bytes
//!
//! [ADR-0112](../../../../docs/adr/0112-print-agents.md): a published printer may carry an
//! `agent_device_id`, and **absent means the edge is the agent**, which is why a fleet that takes
//! this release prints tomorrow the way it printed today. When it is present the last hop moves —
//! and only the last hop. Everything above [`Printers::dispatch`] is unchanged, the rasterising in
//! [`Printers::prepare`] still runs *here*, and what crosses to the agent is a finished
//! [`PrintJob`](pos_ports::printer::PrintJob): a device that drew its own glyphs would draw them from whatever fonts that device
//! happens to have, and a store with three terminals would print three different tickets from one
//! order.
//!
//! # A till may print its guests' paper at a printer of its own
//!
//! [ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
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
/// ([ADR-0165](../../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 4).
///
/// None of the three is an error. The act the drawer opened for has already been recorded, and a
/// drawer that did not spring is opened with its key; what the till needs is which of these happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawerOutcome {
    /// The kick went down the drawer printer's cable, or, behind a print agent, the agent
    /// acknowledged writing it within [`KICK_WAIT`](crate::print_agent::KICK_WAIT). Whether the drawer sprang is the hardware's
    /// part (gate P8), which the edge cannot see.
    Opened,
    /// No printer this edge may open a drawer through: none is marked, or the marked one is not on
    /// USB, serves a station, or prints through an agent that does not carry a kick, one built
    /// before kicks. The ordinary state of a store with no drawer.
    NoDrawer,
    /// The drawer printer was chosen and did not take the kick: unplugged or switched off, or its
    /// agent silent, its queue full, or no acknowledgement within [`KICK_WAIT`](crate::print_agent::KICK_WAIT).
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
/// duplicates removed ([ADR-0117](../../../../docs/adr/0117-a-headless-store-keeps-a-log.md)
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
    /// ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6), at most
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
mod tests;
