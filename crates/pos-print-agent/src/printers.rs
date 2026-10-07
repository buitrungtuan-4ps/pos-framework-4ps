// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Opening the address the edge named and writing the bytes it prepared.
//!
//! The whole of the agent's contact with hardware, and it makes exactly one choice: which transport
//! `connection` names. Everything else — the code page, the raster, the width, the cut — was decided
//! on the edge and travels with the job ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! A kick, a job that is [`PrintDocument::kick`](pos_ports::printer::PrintDocument::kick) alone,
//! opens the drawer wired to the printer instead of printing
//! ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6). It is taken
//! only for a printer this machine reaches over USB that the edge's lease says has a drawer, and the
//! adapter's own [`may_open_a_drawer`](pos_ports::printer::PrinterCapabilities::may_open_a_drawer)
//! checks the same again before it writes the pulse.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use printer_escpos::device::DeviceTransport;
use printer_escpos::tcp::TcpTransport;
use printer_escpos::{EscPosPrinter, Transport};

use pos_ports::printer::{PrinterConnection, PrinterDriver};
use pos_ports::{PortError, PortName};

use crate::{LeasedJob, Printing};

/// One printer held open for the life of the process.
type Held = Arc<EscPosPrinter<Box<dyn Transport>>>;

/// A held printer and the address it was opened at.
struct Opened {
    printer: Held,
    address: String,
}

/// The printers this machine's transports reach.
///
/// Held open between jobs, which is what makes `printer-escpos`'s own idempotency set mean
/// something: a redelivered job that reaches the encoder a second time prints once. That set is a
/// *second* line of defence behind [`crate::LastWritten`], not a replacement — it lives in this
/// process's memory and a restart empties it, which is precisely the case the durable record covers.
#[derive(Default)]
pub struct EscPosPrinters {
    open: Mutex<HashMap<String, Opened>>,
}

impl std::fmt::Debug for EscPosPrinters {
    /// How many printers are held, never which document is on one. Hand-written because
    /// `EscPosPrinter<Box<dyn Transport>>` is not `Debug` — its transport is a trait object — and
    /// because `pos_ports::printer` forbids a document reaching a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.open.lock().map_or(0, |open| open.len());
        f.debug_struct("EscPosPrinters")
            .field("open", &held)
            .finish()
    }
}

impl EscPosPrinters {
    /// A dispatcher with nothing open yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The held printer for this job's device, opening a channel the first time.
    ///
    /// Keyed on the device id, and opened again when a lease names another address or other
    /// capabilities, as the edge's own dispatcher does: the console changed the printer, a drawer
    /// marked or a width, and the edge's lease is the current word. A printer opened again starts
    /// a fresh in-memory idempotency set, behind the durable record that still covers its last job.
    /// Nothing here dials anything; both transports connect on the first write, so a printer that is
    /// off when the agent starts does not stop it starting.
    fn printer_for(&self, job: &LeasedJob) -> Result<Held, PortError> {
        let mut open = self.open.lock().map_err(|_poisoned| {
            PortError::internal(
                PortName::PrinterDriver,
                "the printer registry lock was poisoned",
            )
        })?;
        if let Some(opened) = open.get(&job.printer_device_id)
            && opened.address == job.address
            && opened.printer.capabilities() == job.capabilities
        {
            return Ok(Arc::clone(&opened.printer));
        }
        let transport: Box<dyn Transport> = match job.connection {
            PrinterConnection::Network => Box::new(TcpTransport::new(&job.address)),
            // A directly attached printer's address is a device path — `/dev/usb/lp0`,
            // `/dev/ttyUSB0`, `\\.\COM3` — rather than a host (ADR-0103). Dialling port 9100 at one
            // would fail with a message about the network, which is the wrong thing to hand an
            // operator holding a USB cable.
            PrinterConnection::Usb | PrinterConnection::Serial => {
                Box::new(DeviceTransport::new(&job.address))
            }
            // `PrinterConnection` is `#[non_exhaustive]`: a connection a later release adds is one
            // this build has no transport for, and saying so beats guessing at a socket.
            _ => {
                return Err(PortError::failed_precondition(
                    PortName::PrinterDriver,
                    "this build has no transport for that printer's connection",
                ));
            }
        };
        let held: Held = Arc::new(EscPosPrinter::new(job.capabilities.clone(), transport));
        open.insert(
            job.printer_device_id.clone(),
            Opened {
                printer: Arc::clone(&held),
                address: job.address.clone(),
            },
        );
        Ok(held)
    }
}

impl Printing for EscPosPrinters {
    fn write(&self, job: &LeasedJob) -> Result<(), PortError> {
        if job.job.document.is_kick() {
            // The transport and the printer both on USB, and the console's drawer mark: anything
            // else is not a drawer this machine owns, and the kick is refused unwritten.
            if !(job.connection.may_open_a_drawer() && job.capabilities.may_open_a_drawer()) {
                return Err(PortError::failed_precondition(
                    PortName::PrinterDriver,
                    "a kick is taken only for a USB printer marked as having a cash drawer",
                ));
            }
            return self.printer_for(job)?.open_drawer_blocking();
        }
        self.printer_for(job)?.print_blocking(&job.job)
    }
}
