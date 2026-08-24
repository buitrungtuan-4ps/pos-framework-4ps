// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `printer-escpos` against the shared `PrinterDriver` contract suite.
//!
//! The harness wires the adapter to an in-memory transport it can inspect and fault-inject: it
//! counts physical tickets (a document write, recognised by its ESC/POS initialise prefix), sees
//! whether the drawer pulsed, and can take the printer offline or empty its paper. The adapter's
//! futures never suspend — it does no async I/O in this build — so the suite runs under
//! `run_ready`, a one-poll executor, rather than a runtime.

// The whole file is test scaffolding; `allow-expect-in-tests` scopes to `#[test]`/`#[cfg(test)]`
// and does not reach an integration test's module-level helpers.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a poisoned lock in a recorder is an unrecoverable test fault"
)]

use std::sync::Mutex;

use pos_contract_tests::harness::{PrinterDriverHarness, Setup};
use pos_fakes::executor::run_ready;
use pos_ports::PrinterCapabilities;
use printer_escpos::escpos::{DRAWER_KICK, INIT};
use printer_escpos::{EscPosPrinter, Transport, TransportStatus, Unreachable};

/// An in-memory printer: records every write, and can be taken offline or emptied of paper.
#[derive(Debug, Default)]
struct RecordingTransport {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    writes: Vec<Vec<u8>>,
    offline: bool,
    has_paper: Option<bool>,
    cover_closed: Option<bool>,
}

impl RecordingTransport {
    fn set_offline(&self) {
        self.state.lock().expect("recorder lock").offline = true;
    }

    fn empty_paper(&self) {
        self.state.lock().expect("recorder lock").has_paper = Some(false);
    }

    /// Documents begin with the ESC/POS initialise prefix; the drawer pulse does not.
    fn tickets(&self) -> u32 {
        let state = self.state.lock().expect("recorder lock");
        let count = state
            .writes
            .iter()
            .filter(|write| write.starts_with(&INIT))
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn drawer_opened(&self) -> bool {
        let state = self.state.lock().expect("recorder lock");
        state
            .writes
            .iter()
            .any(|write| write.as_slice() == DRAWER_KICK)
    }
}

impl Transport for RecordingTransport {
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
        let mut state = self.state.lock().expect("recorder lock");
        if state.offline {
            return Err(Unreachable);
        }
        state.writes.push(bytes.to_vec());
        Ok(())
    }

    fn probe(&self) -> Result<TransportStatus, Unreachable> {
        let state = self.state.lock().expect("recorder lock");
        if state.offline {
            return Err(Unreachable);
        }
        Ok(TransportStatus {
            has_paper: state.has_paper,
            cover_closed: state.cover_closed,
        })
    }
}

struct PrinterHarness;

impl PrinterDriverHarness for PrinterHarness {
    type Printer = EscPosPrinter<RecordingTransport>;

    async fn fresh(&self, capabilities: PrinterCapabilities) -> Setup<Self::Printer> {
        Ok(EscPosPrinter::new(
            capabilities,
            RecordingTransport::default(),
        ))
    }

    async fn take_offline(&self, printer: &Self::Printer) -> Setup<()> {
        printer.transport().set_offline();
        Ok(())
    }

    async fn empty_paper(&self, printer: &Self::Printer) -> Setup<()> {
        printer.transport().empty_paper();
        Ok(())
    }

    async fn tickets_printed(&self, printer: &Self::Printer) -> Setup<u32> {
        Ok(printer.transport().tickets())
    }

    async fn drawer_opened(&self, printer: &Self::Printer) -> Setup<bool> {
        Ok(printer.transport().drawer_opened())
    }
}

mod printer {
    use super::{PrinterHarness, run_ready};
    pos_contract_tests::printer_driver_suite!(PrinterHarness, run_ready);
}
