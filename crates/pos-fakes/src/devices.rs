// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The device fakes: a printer and a card terminal.
//!
//! Both count what they physically did — tickets, authorisations — because their idempotency
//! obligations are invisible through the port. A deduplicated retry and a real second action both
//! return `Ok`, and on the terminal that difference is a customer charged twice.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use pos_ports::payment::{PaymentAttempt, PaymentReference, PaymentRequest, PaymentTerminal};
use pos_ports::printer::{PrintJob, PrinterCapabilities, PrinterDriver, PrinterStatus};
use pos_ports::{PortError, PortName};
use pos_proto::ids::{EventId, PaymentId};
use pos_proto::wire_enum::Open;
use pos_proto::{PaymentOutcome, Timestamp};

use crate::lock;

// -----------------------------------------------------------------------------------------------
// PrinterDriver
// -----------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct PrinterState {
    offline: bool,
    out_of_paper: bool,
    /// Job identifiers already printed, which is how the fake deduplicates.
    printed: BTreeSet<EventId>,
    tickets: u32,
    drawer_opened: bool,
    queued: u32,
}

/// An in-memory `PrinterDriver`.
#[derive(Debug, Clone)]
pub struct FakePrinter {
    capabilities: PrinterCapabilities,
    state: Arc<Mutex<PrinterState>>,
}

impl FakePrinter {
    /// A ready printer with the given capabilities.
    ///
    /// Taking capabilities as a parameter is what lets one suite cover both a USB drawer-kicking
    /// printer and a network one — the two configurations whose difference is a security boundary.
    #[must_use]
    pub fn new(capabilities: PrinterCapabilities) -> Self {
        Self {
            capabilities,
            state: Arc::new(Mutex::new(PrinterState::default())),
        }
    }

    /// Takes the printer off the network or the bus.
    pub fn take_offline(&self) {
        lock(&self.state).offline = true;
    }

    /// Empties the paper roll.
    pub fn empty_paper(&self) {
        lock(&self.state).out_of_paper = true;
    }

    /// How many tickets physically came out.
    #[must_use]
    pub fn tickets_printed(&self) -> u32 {
        lock(&self.state).tickets
    }

    /// Whether the drawer was opened.
    #[must_use]
    pub fn drawer_opened(&self) -> bool {
        lock(&self.state).drawer_opened
    }
}

impl PrinterDriver for FakePrinter {
    fn capabilities(&self) -> PrinterCapabilities {
        // Deliberately does not touch `state`. The port declares this synchronous and infallible, so
        // an implementation that consulted the hardware would have no way to report failure.
        self.capabilities.clone()
    }

    async fn print(&self, job: &PrintJob) -> Result<(), PortError> {
        let mut state = lock(&self.state);
        if state.offline {
            return Err(PortError::unavailable(
                PortName::PrinterDriver,
                "the printer did not answer",
            ));
        }
        if state.out_of_paper {
            return Err(PortError::failed_precondition(
                PortName::PrinterDriver,
                "the printer is out of paper",
            ));
        }
        if !state.printed.insert(job.job_id) {
            // Already printed. Success, not a conflict: the retry that produced this is the retry a
            // flaky USB cable produces constantly.
            return Ok(());
        }
        state.tickets = state.tickets.saturating_add(1);
        Ok(())
    }

    async fn status(&self) -> Result<PrinterStatus, PortError> {
        let state = lock(&self.state);
        if state.offline {
            return Ok(PrinterStatus {
                online: false,
                has_paper: None,
                cover_closed: None,
                queue_depth: state.queued,
            });
        }
        Ok(PrinterStatus {
            online: true,
            has_paper: Some(!state.out_of_paper),
            cover_closed: Some(true),
            queue_depth: state.queued,
        })
    }

    async fn open_drawer(&self) -> Result<(), PortError> {
        // The connection check comes first, and before the offline check. A network drawer must be
        // refused whether or not the printer is reachable — the rule is about the channel, not about
        // this printer's current health.
        if !self.capabilities.may_open_a_drawer() {
            return Err(PortError::failed_precondition(
                PortName::PrinterDriver,
                "a drawer opens only over USB",
            ));
        }
        let mut state = lock(&self.state);
        if state.offline {
            return Err(PortError::unavailable(
                PortName::PrinterDriver,
                "the printer did not answer",
            ));
        }
        state.drawer_opened = true;
        Ok(())
    }
}

// -----------------------------------------------------------------------------------------------
// PaymentTerminal
// -----------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct TerminalState {
    /// What the next authorisation concludes. `None` means the terminal was never staged, which the
    /// fake treats as a capture so the happy path needs no setup.
    staged: Option<PaymentOutcome>,
    /// Attempts by payment identifier, which is how the fake deduplicates.
    attempts: Vec<PaymentAttempt>,
    /// How many times the acquirer was actually asked to move money.
    authorisations: u32,
}

/// An in-memory `PaymentTerminal`.
#[derive(Debug, Clone, Default)]
pub struct FakePaymentTerminal {
    state: Arc<Mutex<TerminalState>>,
}

impl FakePaymentTerminal {
    /// A terminal with no attempts on it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the next authorisation conclude with `outcome`.
    pub fn stage_outcome(&self, outcome: PaymentOutcome) {
        lock(&self.state).staged = Some(outcome);
    }

    /// How many times money was actually moved.
    #[must_use]
    pub fn authorisation_count(&self) -> u32 {
        lock(&self.state).authorisations
    }

    /// The reference this fake issues for `payment_id`.
    ///
    /// Derived from the identifier rather than random, so a retry produces the same reference and the
    /// idempotency case can compare them.
    fn reference_for(payment_id: PaymentId) -> PaymentReference {
        PaymentReference::new(format!("fake-{payment_id}"))
    }
}

impl PaymentTerminal for FakePaymentTerminal {
    async fn authorize(&self, request: &PaymentRequest) -> Result<PaymentAttempt, PortError> {
        let mut state = lock(&self.state);
        if let Some(existing) = state
            .attempts
            .iter()
            .find(|attempt| attempt.payment_id == request.payment_id)
        {
            // The acquirer is not asked again. This is the obligation that keeps a customer from
            // being charged twice, and it is invisible from the port — only the counter shows it.
            return Ok(existing.clone());
        }

        let outcome = state.staged.take().unwrap_or(PaymentOutcome::Captured);
        state.authorisations = state.authorisations.saturating_add(1);
        let attempt = PaymentAttempt {
            payment_id: request.payment_id,
            // Present even for an unknown outcome. An unknown attempt with no reference could never
            // be resolved, so the bill would stay amber forever.
            reference: Self::reference_for(request.payment_id),
            outcome: Open::from_known(outcome),
            amount: request.amount,
            at: Timestamp::EPOCH,
        };
        state.attempts.push(attempt.clone());
        Ok(attempt)
    }

    async fn look_up(&self, reference: &PaymentReference) -> Result<PaymentAttempt, PortError> {
        let state = lock(&self.state);
        state
            .attempts
            .iter()
            .find(|attempt| &attempt.reference == reference)
            .cloned()
            .ok_or_else(|| {
                PortError::not_found(
                    PortName::PaymentTerminal,
                    "the acquirer has no record of this reference",
                )
            })
    }

    async fn void(&self, reference: &PaymentReference) -> Result<PaymentAttempt, PortError> {
        let mut state = lock(&self.state);
        let Some(attempt) = state
            .attempts
            .iter_mut()
            .find(|attempt| &attempt.reference == reference)
        else {
            return Err(PortError::not_found(
                PortName::PaymentTerminal,
                "the acquirer has no record of this reference",
            ));
        };
        if attempt.outcome.known() == PaymentOutcome::Captured {
            // A settled capture is reversed by a refund, which is a new movement of money and belongs
            // to the domain rather than to the terminal.
            return Err(PortError::failed_precondition(
                PortName::PaymentTerminal,
                "the attempt has already settled; issue a refund instead",
            ));
        }
        attempt.outcome = Open::from_known(PaymentOutcome::Declined);
        Ok(attempt.clone())
    }
}
