// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! How the edge reaches a printer it writes to itself, and what it assumes such a printer can do.

use std::fmt;
use std::num::NonZeroU16;

use pos_ports::printer::{CodePage, PrinterCapabilities, PrinterConnection};
use pos_ports::{PortError, PortName};
use pos_proto::devices::PublishedDevice;
use printer_escpos::Transport;
use printer_escpos::device::DeviceTransport;
use printer_escpos::tcp::TcpTransport;

use crate::printing::devices::connection_of;

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
