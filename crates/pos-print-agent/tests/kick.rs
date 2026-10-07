// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A kick through the agent
//! ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
//!
//! Driven through the shipped printers over a scratch file standing in for the USB device node, so
//! what is asserted is the bytes that reach it: the drawer pulse for a kick the agent owns, and
//! nothing for one it does not.

use std::num::NonZeroU16;
use std::path::Path;
use std::sync::Mutex;

use pos_ports::printer::{
    CodePage, PrintBlock, PrintDocument, PrintJob, PrinterCapabilities, PrinterConnection,
};
use pos_print_agent::printers::EscPosPrinters;
use pos_print_agent::{AgentError, CLAIM_PATH, EdgeTransport, LastWritten, LeasedJob, one_cycle};
use pos_proto::ids::{EventId, StoreId};
use pos_proto::ulid::Ulid;
use printer_escpos::escpos::DRAWER_KICK;

const PRINTER: &str = "00000000000000000000000091";

/// A leased job for [`PRINTER`] at `address`, over `connection`, marked as having a drawer or not.
fn leased(
    job_id: u128,
    document: PrintDocument,
    address: &Path,
    connection: PrinterConnection,
    drawer: bool,
) -> LeasedJob {
    LeasedJob {
        printer_device_id: PRINTER.to_owned(),
        address: address.display().to_string(),
        connection,
        capabilities: PrinterCapabilities {
            connection,
            code_page: CodePage::Ascii,
            columns: NonZeroU16::new(42).expect("positive"),
            dots_per_line: NonZeroU16::new(576).expect("positive"),
            prints_bitmaps: true,
            cuts_paper: true,
            kicks_drawer: drawer,
        },
        claim_expires_at: 30_000,
        job: PrintJob {
            job_id: EventId::new(Ulid::from_u128(job_id)),
            store_id: StoreId::new(Ulid::from_u128(3)),
            station_id: None,
            document,
        },
    }
}

/// An edge that hands over one batch and records what was acknowledged.
#[derive(Debug, Default)]
struct StubEdge {
    handing: Mutex<Vec<LeasedJob>>,
    acknowledged: Mutex<Vec<EventId>>,
}

impl StubEdge {
    fn handing(jobs: Vec<LeasedJob>) -> Self {
        Self {
            handing: Mutex::new(jobs),
            ..Self::default()
        }
    }

    fn acknowledged(&self) -> Vec<EventId> {
        self.acknowledged.lock().expect("lock").clone()
    }
}

impl EdgeTransport for StubEdge {
    async fn claim(&self) -> Result<Vec<LeasedJob>, AgentError> {
        Ok(std::mem::take(&mut *self.handing.lock().expect("lock")))
    }

    async fn acknowledge(&self, job: EventId) -> Result<(), AgentError> {
        self.acknowledged.lock().expect("lock").push(job);
        Ok(())
    }
}

/// A scratch file standing in for a device node, and the record of written jobs beside it.
fn device() -> (tempfile::TempDir, std::path::PathBuf, LastWritten) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let node = dir.path().join("lp0");
    std::fs::write(&node, b"").expect("the device node stand-in");
    let written = LastWritten::load(&dir.path().join("state.json"));
    (dir, node, written)
}

#[test]
fn the_claim_says_the_agent_carries_a_kick() {
    assert_eq!(CLAIM_PATH, "/api/print/jobs?kicks_drawer=true");
}

#[tokio::test]
async fn a_kick_writes_the_pulse_to_its_usb_printer_and_is_acknowledged() {
    let (_dir, node, written) = device();
    let edge = StubEdge::handing(vec![leased(
        1,
        PrintDocument::kick(),
        &node,
        PrinterConnection::Usb,
        true,
    )]);
    let handled = one_cycle(&edge, &EscPosPrinters::new(), &written)
        .await
        .expect("the cycle runs");
    assert_eq!(handled, 1);
    assert_eq!(std::fs::read(&node).expect("read"), DRAWER_KICK.to_vec());
    assert_eq!(edge.acknowledged(), vec![EventId::new(Ulid::from_u128(1))]);

    // Redelivered after a lost acknowledgement: acknowledged again, and the drawer not kicked twice.
    let edge = StubEdge::handing(vec![leased(
        1,
        PrintDocument::kick(),
        &node,
        PrinterConnection::Usb,
        true,
    )]);
    one_cycle(&edge, &EscPosPrinters::new(), &written)
        .await
        .expect("the cycle runs");
    assert_eq!(std::fs::read(&node).expect("read"), DRAWER_KICK.to_vec());
    assert_eq!(edge.acknowledged().len(), 1);
}

#[tokio::test]
async fn a_kick_for_a_network_or_serial_printer_is_refused_unwritten() {
    for connection in [PrinterConnection::Network, PrinterConnection::Serial] {
        let (_dir, node, written) = device();
        let address = if connection == PrinterConnection::Network {
            Path::new("127.0.0.1:9").to_path_buf()
        } else {
            node.clone()
        };
        let edge = StubEdge::handing(vec![leased(
            2,
            PrintDocument::kick(),
            &address,
            connection,
            true,
        )]);
        let handled = one_cycle(&edge, &EscPosPrinters::new(), &written)
            .await
            .expect("the cycle runs");
        assert_eq!(handled, 0, "{connection:?}");
        assert!(
            std::fs::read(&node).expect("read").is_empty(),
            "{connection:?}"
        );
        assert!(edge.acknowledged().is_empty(), "{connection:?}");
    }
}

#[tokio::test]
async fn a_kick_for_a_printer_with_no_drawer_marked_is_refused_unwritten() {
    // A drawer this machine does not own is one the edge did not say is wired to its printer.
    let (_dir, node, written) = device();
    let edge = StubEdge::handing(vec![leased(
        3,
        PrintDocument::kick(),
        &node,
        PrinterConnection::Usb,
        false,
    )]);
    let handled = one_cycle(&edge, &EscPosPrinters::new(), &written)
        .await
        .expect("the cycle runs");
    assert_eq!(handled, 0);
    assert!(std::fs::read(&node).expect("read").is_empty());
    assert!(edge.acknowledged().is_empty());
}

#[tokio::test]
async fn a_drawer_marked_after_its_printer_printed_is_kicked_without_a_restart() {
    // The printer is held from a receipt printed before anybody marked its drawer; the next lease
    // carries the mark, and the kick does not wait for the agent to restart.
    let (_dir, node, written) = device();
    let printers = EscPosPrinters::new();
    let receipt = PrintDocument {
        blocks: vec![PrintBlock::Cut],
    };
    let edge = StubEdge::handing(vec![leased(
        4,
        receipt,
        &node,
        PrinterConnection::Usb,
        false,
    )]);
    one_cycle(&edge, &printers, &written)
        .await
        .expect("the cycle runs");
    assert!(
        !std::fs::read(&node).expect("read").is_empty(),
        "the receipt printed"
    );
    // The paper is taken away, as a device node's bytes are; the printer held from the receipt
    // knows no drawer, so only a printer opened again under the lease's mark writes the pulse.
    std::fs::write(&node, b"").expect("empty the stand-in");

    let edge = StubEdge::handing(vec![leased(
        5,
        PrintDocument::kick(),
        &node,
        PrinterConnection::Usb,
        true,
    )]);
    one_cycle(&edge, &printers, &written)
        .await
        .expect("the cycle runs");
    assert_eq!(std::fs::read(&node).expect("read"), DRAWER_KICK.to_vec());
    assert_eq!(edge.acknowledged(), vec![EventId::new(Ulid::from_u128(5))]);
}
