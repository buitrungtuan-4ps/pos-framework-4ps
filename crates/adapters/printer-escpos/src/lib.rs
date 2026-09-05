// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! ESC/POS thermal-printer adapter for the [`PrinterDriver`] port.
//!
//! This crate is the vendor protocol and nothing else
//! ([`architecture.md`](../../../docs/architecture.md) §6.1): it encodes a [`PrintDocument`] into
//! ESC/POS bytes and pushes them at a [`Transport`], applies the rules the port's contract fixes
//! (idempotent by job id, a drawer only over USB, retryable failure when unreachable), and reports
//! [`PrinterStatus`]. It decides nothing about *whether* a line is a bitmap — the framework already
//! did, using the code page this adapter reports ([ADR-0026](../../../docs/adr/0026-port-shapes.md)
//! §5).
//!
//! # The transport is the last mile
//!
//! Turning bytes into pulses over USB, a serial line, or raw TCP on port 9100 is the one part that
//! needs real hardware to validate (roadmap A5), so it is behind the [`Transport`] trait. The
//! contract suite runs against an in-memory transport that records what was sent. [`tcp`] is the
//! network case and needs no hardware to exercise — a `TcpListener` in a test is indistinguishable
//! from a printer to everything above the socket ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)).
//! USB and serial stay with hardware bring-up.
//!
//! # The queue lives at the caller
//!
//! The port contract (`docs/pos-spec.md` §2, [`pos_ports::printer`]) puts the retry queue and
//! backup-printer failover at the caller: an unreachable printer returns
//! [`PortError::unavailable`] and the caller re-queues, rather than the adapter blocking. So this
//! adapter buffers nothing — its `status().queue_depth` is zero — and the queue is `pos_edge`'s (P5).

pub mod device;
pub mod escpos;
pub mod tcp;

use std::collections::HashSet;
use std::sync::Mutex;

use pos_ports::printer::{PrintBlock, PrintJob, PrinterCapabilities, PrinterDriver, PrinterStatus};
use pos_ports::{PortError, PortName};
use pos_proto::ids::EventId;

/// A byte channel to one printer. [`tcp::TcpTransport`] carries it over the LAN and
/// [`device::DeviceTransport`] over a USB or serial cable; the contract suite uses an in-memory
/// recorder.
pub trait Transport: Send + Sync {
    /// Sends raw bytes to the printer.
    ///
    /// # Errors
    ///
    /// [`Unreachable`] if the printer cannot be reached — the ordinary transient failure, which the
    /// adapter reports as [`PortError::unavailable`] so the caller re-queues.
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable>;

    /// Reads the printer's paper and cover sensors.
    ///
    /// # Errors
    ///
    /// [`Unreachable`] if the printer cannot be reached.
    fn probe(&self) -> Result<TransportStatus, Unreachable>;
}

impl Transport for Box<dyn Transport> {
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
        (**self).write(bytes)
    }

    fn probe(&self) -> Result<TransportStatus, Unreachable> {
        (**self).probe()
    }
}

/// The printer could not be reached. A deliberately information-free marker: the adapter maps it to
/// one status, and anything more would be a detail the caller cannot act on differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unreachable;

/// What a printer's sensors report. `None` where the model has no such sensor — which many thermal
/// printers do not, and treating that as a fault would make them permanently unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportStatus {
    /// Whether paper is loaded, or `None` if the model cannot tell.
    pub has_paper: Option<bool>,
    /// Whether the cover is closed, or `None` if the model cannot tell.
    pub cover_closed: Option<bool>,
}

/// An ESC/POS printer over a [`Transport`].
#[derive(Debug)]
pub struct EscPosPrinter<T> {
    capabilities: PrinterCapabilities,
    transport: T,
    /// Job ids already printed, for idempotency. In memory: a retry within the process prints once,
    /// which is the window a flaky cable retries in.
    printed: Mutex<HashSet<EventId>>,
}

impl<T: Transport> EscPosPrinter<T> {
    /// Builds a printer with fixed capabilities over `transport`.
    pub fn new(capabilities: PrinterCapabilities, transport: T) -> Self {
        Self {
            capabilities,
            transport,
            printed: Mutex::new(HashSet::new()),
        }
    }

    /// The transport, for a harness to inspect or fault-inject.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Prints `job`, blocking the calling thread.
    ///
    /// The port's [`print`](PrinterDriver::print) is this method behind an always-ready future: this
    /// adapter encodes bytes and hands them to a synchronous [`Transport`], so there is nothing to
    /// await. A caller that already owns a blocking thread — `pos_edge`'s dispatcher runs printing
    /// under `spawn_blocking`, because a printer that has been unplugged must not stall an async
    /// worker — calls this directly rather than driving a future that never yields.
    ///
    /// # Errors
    ///
    /// The same as [`PrinterDriver::print`].
    pub fn print_blocking(&self, job: &PrintJob) -> Result<(), PortError> {
        {
            let printed = self.printed.lock().map_err(|_| lock_poisoned())?;
            if printed.contains(&job.job_id) {
                return Ok(());
            }
        }
        // A bitmap on a text-only printer has no correct output; the framework should not produce
        // one, but refuse rather than send garbage the printer renders as noise.
        if !self.capabilities.prints_bitmaps
            && job
                .document
                .blocks
                .iter()
                .any(|block| matches!(block, PrintBlock::Bitmap { .. }))
        {
            return Err(PortError::invalid_argument(
                PortName::PrinterDriver,
                "this printer cannot print bitmaps",
            ));
        }

        let status = self.transport.probe().map_err(|_| unreachable_error())?;
        if matches!(status.has_paper, Some(false)) || matches!(status.cover_closed, Some(false)) {
            return Err(PortError::failed_precondition(
                PortName::PrinterDriver,
                "the printer is out of paper or its cover is open",
            ));
        }

        let bytes = escpos::encode(&job.document);
        self.transport
            .write(&bytes)
            .map_err(|_| unreachable_error())?;
        self.printed
            .lock()
            .map_err(|_| lock_poisoned())?
            .insert(job.job_id);
        Ok(())
    }

    /// Asks the printer how it is, blocking the calling thread. See [`Self::print_blocking`].
    ///
    /// # Errors
    ///
    /// The same as [`PrinterDriver::status`].
    pub fn status_blocking(&self) -> Result<PrinterStatus, PortError> {
        let status = self.transport.probe().map_err(|_| unreachable_error())?;
        Ok(PrinterStatus {
            online: true,
            has_paper: status.has_paper,
            cover_closed: status.cover_closed,
            // The caller owns the queue (port §2); this adapter holds nothing.
            queue_depth: 0,
        })
    }

    /// Opens the attached cash drawer, blocking the calling thread. See [`Self::print_blocking`].
    ///
    /// # Errors
    ///
    /// The same as [`PrinterDriver::open_drawer`].
    pub fn open_drawer_blocking(&self) -> Result<(), PortError> {
        // Both conditions, and the connection is the one that is not negotiable: port 9100 has no
        // authentication, so a drawer never opens over the network (ADR-0026, architecture.md §5).
        if !self.capabilities.may_open_a_drawer() {
            return Err(PortError::failed_precondition(
                PortName::PrinterDriver,
                "a drawer opens only over USB and only when one is attached",
            ));
        }
        self.transport
            .write(&escpos::DRAWER_KICK)
            .map_err(|_| unreachable_error())?;
        Ok(())
    }
}

impl<T: Transport> PrinterDriver for EscPosPrinter<T> {
    fn capabilities(&self) -> PrinterCapabilities {
        self.capabilities.clone()
    }

    async fn print(&self, job: &PrintJob) -> Result<(), PortError> {
        self.print_blocking(job)
    }

    async fn status(&self) -> Result<PrinterStatus, PortError> {
        self.status_blocking()
    }

    async fn open_drawer(&self) -> Result<(), PortError> {
        self.open_drawer_blocking()
    }
}

fn unreachable_error() -> PortError {
    PortError::unavailable(PortName::PrinterDriver, "the printer could not be reached")
}

fn lock_poisoned() -> PortError {
    PortError::internal(
        PortName::PrinterDriver,
        "the printer's idempotency lock was poisoned",
    )
}
