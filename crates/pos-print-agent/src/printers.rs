// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Opening the address the edge named and writing the bytes it prepared.
//!
//! The whole of the agent's contact with hardware, and it makes exactly one choice: which transport
//! `connection` names. Everything else — the code page, the raster, the width, the cut — was decided
//! on the edge and travels with the job ([ADR-0112](../../../docs/adr/0112-print-agents.md)).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use printer_escpos::device::DeviceTransport;
use printer_escpos::tcp::TcpTransport;
use printer_escpos::{EscPosPrinter, Transport};

use pos_ports::printer::PrinterConnection;
use pos_ports::{PortError, PortName};

use crate::{LeasedJob, Printing};

/// One printer held open for the life of the process.
type Held = Arc<EscPosPrinter<Box<dyn Transport>>>;

/// The printers this machine's transports reach.
///
/// Held open between jobs, which is what makes `printer-escpos`'s own idempotency set mean
/// something: a redelivered job that reaches the encoder a second time prints once. That set is a
/// *second* line of defence behind [`crate::LastWritten`], not a replacement — it lives in this
/// process's memory and a restart empties it, which is precisely the case the durable record covers.
#[derive(Default)]
pub struct EscPosPrinters {
    open: Mutex<HashMap<String, Held>>,
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
    /// Keyed on the device id, so the capabilities are the ones the **first** job for that printer
    /// carried — the same caching the edge's own dispatcher does, and the same consequence: a
    /// published device whose capabilities change takes effect on the next restart. Nothing here
    /// dials anything; both transports connect on the first write, so a printer that is off when the
    /// agent starts does not stop it starting.
    fn printer_for(&self, job: &LeasedJob) -> Result<Held, PortError> {
        let mut open = self.open.lock().map_err(|_poisoned| {
            PortError::internal(
                PortName::PrinterDriver,
                "the printer registry lock was poisoned",
            )
        })?;
        if let Some(held) = open.get(&job.printer_device_id) {
            return Ok(Arc::clone(held));
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
        open.insert(job.printer_device_id.clone(), Arc::clone(&held));
        Ok(held)
    }
}

impl Printing for EscPosPrinters {
    fn write(&self, job: &LeasedJob) -> Result<(), PortError> {
        self.printer_for(job)?.print_blocking(&job.job)
    }
}
