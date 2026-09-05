// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The directly-attached transport: ESC/POS bytes to a device file
//! ([ADR-0103](../../../docs/adr/0103-directly-attached-printers.md)).
//!
//! # Why a file, and not a USB library
//!
//! A USB thermal printer is a USB *printer-class* device, and every operating system that supports
//! one already exposes it as something you write bytes to: `/dev/usb/lp0` on Linux, `/dev/ulpt0` on
//! the BSDs, a port on Windows. Serial printers are the same shape — `/dev/ttyUSB0`, `/dev/ttyS0`,
//! `\\.\COM3`. So the transport is a file handle, and it needs no USB stack, no `libusb`, and no C
//! toolchain to cross-compile for the ARM boxes a store runs on.
//!
//! Talking raw USB instead — claiming the interface and pushing bulk transfers — would mean
//! detaching the kernel driver that is already doing this correctly, and would gain nothing a
//! restaurant can use.
//!
//! # What this transport does not configure
//!
//! **Baud rate, on a serial printer.** Setting it from inside the process means `termios`, which
//! means unsafe FFI in a crate that has none. It is a one-line deployment step instead
//! (`stty -F /dev/ttyUSB0 19200 raw`, or the `ExecStartPre` in the service unit), which
//! `deploy/edge/README.md` carries. USB printer-class devices have no baud rate at all, so the
//! common case needs nothing.
//!
//! # The drawer works here, and only here
//!
//! This is the transport a cash drawer can be opened over. `docs/architecture.md` §5 forbids it on
//! the network because port 9100 has no authentication; a cable in the back of a till is a
//! different threat model, and [`PrinterConnection::may_open_a_drawer`] already encodes that.
//!
//! [`PrinterConnection::may_open_a_drawer`]: pos_ports::printer::PrinterConnection::may_open_a_drawer

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::{Transport, TransportStatus, Unreachable};

/// A byte channel to a printer plugged into this machine.
#[derive(Debug)]
pub struct DeviceTransport {
    path: PathBuf,
    /// The open handle, kept between jobs. Opening one per receipt is slow enough to be felt at a
    /// till, and on some drivers it also resets the printer.
    handle: Mutex<Option<File>>,
}

impl DeviceTransport {
    /// A transport to the device at `path`.
    ///
    /// The path is what the published device carries as its address: `/dev/usb/lp0` for a USB
    /// printer-class device on Linux, `/dev/ttyUSB0` or `/dev/ttyS0` for serial, `\\.\COM3` on
    /// Windows. Nothing is opened until the first write, so constructing this cannot fail and an
    /// unplugged printer is discovered where every other unreachable printer is.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            handle: Mutex::new(None),
        }
    }

    /// The device path, for diagnostics.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opens the device for writing.
    ///
    /// Write-only and no truncation: a printer is a stream, not a file, and asking to truncate one
    /// is refused by some drivers and ignored by the rest.
    fn open(&self) -> Result<File, Unreachable> {
        OpenOptions::new()
            .write(true)
            .create(false)
            .truncate(false)
            .open(&self.path)
            .map_err(|_| Unreachable)
    }
}

impl Transport for DeviceTransport {
    /// Writes to the device, reopening once if the held handle has gone stale.
    ///
    /// A printer that was switched off and on again, or a USB cable pulled and replugged, leaves a
    /// handle that errors on the next write while the device itself is fine — the same shape as the
    /// dropped idle socket the TCP transport reconnects for. One retry, so a genuinely absent
    /// printer still fails fast rather than looping in front of a cashier.
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
        let mut held = self.handle.lock().map_err(|_| Unreachable)?;
        if held.is_none() {
            *held = Some(self.open()?);
        }
        if let Some(file) = held.as_mut() {
            // `write_all` then `flush`: a partial write to a printer is a torn ticket, and the
            // driver buffers until told not to.
            if file.write_all(bytes).and_then(|()| file.flush()).is_ok() {
                return Ok(());
            }
        }

        // Stale handle. Drop it, reopen, and try once more.
        *held = None;
        let mut file = self.open()?;
        file.write_all(bytes)
            .and_then(|()| file.flush())
            .map_err(|_| Unreachable)?;
        *held = Some(file);
        Ok(())
    }

    /// Reports whether the device can be opened, and both sensors as unknown.
    ///
    /// Same reasoning as the network transport: real-time status is `DLE EOT n`, a *read* whose
    /// reply format and timing vary by model, and a wrong answer here reads as "out of paper" and
    /// refuses to print on a printer that is fine. Reachability is what the dispatcher acts on, and
    /// on a directly-attached printer that is exactly "is the device node there and openable" — which
    /// also catches the two failures a shop actually hits: the cable is out, or the service user is
    /// not in the `lp` group.
    fn probe(&self) -> Result<TransportStatus, Unreachable> {
        if self.handle.lock().map_err(|_| Unreachable)?.is_some() {
            return Ok(TransportStatus::default());
        }
        drop(self.open()?);
        Ok(TransportStatus::default())
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceTransport;
    use crate::Transport as _;
    use std::io::Read as _;

    /// A device file stands in for the printer: on Linux `/dev/usb/lp0` is an ordinary writable
    /// node, so a temporary file exercises the same code path the hardware does. What it cannot
    /// exercise is the printer's own behaviour, which is the hardware gate
    /// (`docs/gate-register.md` §6).
    fn scratch(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("pos-printer-{name}-{}.bin", std::process::id()));
        path
    }

    #[test]
    fn bytes_reach_the_device() {
        let path = scratch("write");
        std::fs::write(&path, b"").expect("create the device node stand-in");
        let transport = DeviceTransport::new(&path);
        transport.write(b"HELLO").expect("the device accepts bytes");

        let mut written = Vec::new();
        std::fs::File::open(&path)
            .expect("open")
            .read_to_end(&mut written)
            .expect("read");
        assert_eq!(written, b"HELLO");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn two_writes_both_arrive_on_one_held_handle() {
        // The handle is kept between jobs, so the second ticket must append rather than overwrite
        // the first — the bug a `truncate(true)` would introduce.
        let path = scratch("append");
        std::fs::write(&path, b"").expect("create");
        let transport = DeviceTransport::new(&path);
        transport.write(b"ONE").expect("first");
        transport.write(b"TWO").expect("second");

        let mut written = Vec::new();
        std::fs::File::open(&path)
            .expect("open")
            .read_to_end(&mut written)
            .expect("read");
        assert_eq!(written, b"ONETWO");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_printer_that_is_not_there_is_unreachable_rather_than_a_panic() {
        // An unplugged USB printer, or a service user without the `lp` group. Both are the same
        // answer to the dispatcher: re-queue, do not roll back the sale.
        let transport = DeviceTransport::new(scratch("absent"));
        assert!(transport.write(b"HELLO").is_err());
        assert!(transport.probe().is_err());
    }

    #[test]
    fn probing_an_openable_device_succeeds_with_both_sensors_unknown() {
        let path = scratch("probe");
        std::fs::write(&path, b"").expect("create");
        let transport = DeviceTransport::new(&path);
        let status = transport.probe().expect("the device is there");
        assert_eq!(status.has_paper, None, "no sensor is read over a device file");
        assert_eq!(status.cover_closed, None);
        let _ = std::fs::remove_file(&path);
    }
}
