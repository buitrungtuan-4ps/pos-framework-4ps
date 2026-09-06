// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The ordering rules the agent exists to get right
//! ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! Driven over a stubbed edge and a stubbed print head, because none of what is worth testing here
//! is about HTTP or ESC/POS: it is *write, record, acknowledge — in that order, and never
//! acknowledge a write that failed.* Those three sentences are what stands between a lost
//! acknowledgement and a duplicate receipt, and between a jammed printer and a dish nobody cooks.

use std::sync::Mutex;

use pos_ports::printer::{
    CodePage, PrintBlock, PrintDocument, PrintJob, PrinterCapabilities, PrinterConnection,
};
use pos_ports::{PortError, PortName};
use pos_print_agent::{AgentError, EdgeTransport, LastWritten, LeasedJob, Printing, one_cycle};
use pos_proto::ids::{EventId, StoreId};
use pos_proto::ulid::Ulid;
use std::num::NonZeroU16;

const PRINTER: &str = "00000000000000000000000091";

fn event(seed: u128) -> EventId {
    EventId::new(Ulid::from_u128(seed))
}

/// A leased job for [`PRINTER`], as the edge hands one over.
fn leased(job_id: u128) -> LeasedJob {
    LeasedJob {
        printer_device_id: PRINTER.to_owned(),
        address: "/dev/usb/lp0".to_owned(),
        connection: PrinterConnection::Usb,
        capabilities: PrinterCapabilities {
            connection: PrinterConnection::Usb,
            code_page: CodePage::Ascii,
            columns: NonZeroU16::new(42).expect("positive"),
            dots_per_line: NonZeroU16::new(576).expect("positive"),
            prints_bitmaps: true,
            cuts_paper: true,
            kicks_drawer: false,
        },
        claim_expires_at: 30_000,
        job: PrintJob {
            job_id: event(job_id),
            store_id: StoreId::new(Ulid::from_u128(3)),
            station_id: None,
            document: PrintDocument {
                blocks: vec![PrintBlock::Cut],
            },
        },
    }
}

/// An edge that hands over a fixed batch once and records what was acknowledged.
#[derive(Debug, Default)]
struct StubEdge {
    handing: Mutex<Vec<LeasedJob>>,
    acknowledged: Mutex<Vec<EventId>>,
    /// When set, every acknowledgement is lost — the case the durable record exists for.
    swallow_acknowledgements: bool,
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
        if self.swallow_acknowledgements {
            return Err(AgentError::Edge("the acknowledgement was lost".to_owned()));
        }
        self.acknowledged.lock().expect("lock").push(job);
        Ok(())
    }
}

/// A print head that records what it was asked to write, and can refuse.
#[derive(Debug, Default)]
struct StubHead {
    written: Mutex<Vec<EventId>>,
    refuses: bool,
}

impl StubHead {
    fn written(&self) -> Vec<EventId> {
        self.written.lock().expect("lock").clone()
    }
}

impl Printing for StubHead {
    fn write(&self, job: &LeasedJob) -> Result<(), PortError> {
        if self.refuses {
            return Err(PortError::unavailable(
                PortName::PrinterDriver,
                "out of paper",
            ));
        }
        self.written.lock().expect("lock").push(job.job.job_id);
        Ok(())
    }
}

/// A head that refuses one named printer — the kitchen's — and takes everything else.
#[derive(Debug)]
struct Jammed(Mutex<Vec<EventId>>);

impl Printing for Jammed {
    fn write(&self, job: &LeasedJob) -> Result<(), PortError> {
        if job.printer_device_id.ends_with("92") {
            return Err(PortError::unavailable(
                PortName::PrinterDriver,
                "out of paper",
            ));
        }
        self.0.lock().expect("lock").push(job.job.job_id);
        Ok(())
    }
}

/// A record in a directory that lives as long as the test.
fn record(directory: &tempfile::TempDir) -> LastWritten {
    LastWritten::load(&directory.path().join("state.json"))
}

#[tokio::test]
async fn a_job_is_written_recorded_and_acknowledged() {
    let directory = tempfile::tempdir().expect("temp dir");
    let written = record(&directory);
    let edge = StubEdge::handing(vec![leased(1)]);
    let head = StubHead::default();

    let handled = one_cycle(&edge, &head, &written)
        .await
        .expect("the edge answers");
    assert_eq!(handled, 1);
    assert_eq!(head.written(), vec![event(1)]);
    assert_eq!(edge.acknowledged(), vec![event(1)]);
    assert!(
        written.already_written(PRINTER, event(1)),
        "and the record survives for the next cycle"
    );
}

#[tokio::test]
async fn a_redelivered_job_is_acknowledged_without_printing_a_second_ticket() {
    // The case the whole record exists for: the write landed, the acknowledgement did not, and the
    // edge hands the same job back at the claim lease. A duplicate *receipt* is the sharp one —
    // ADR-0025 bounds it to a second copy of one receipt rather than a second sale, but a second
    // copy is still a second copy.
    let directory = tempfile::tempdir().expect("temp dir");
    let written = record(&directory);
    let head = StubHead::default();

    let lossy = StubEdge {
        handing: Mutex::new(vec![leased(1)]),
        swallow_acknowledgements: true,
        ..StubEdge::default()
    };
    one_cycle(&lossy, &head, &written)
        .await
        .expect("the edge answers");
    assert_eq!(head.written(), vec![event(1)], "printed once");
    assert!(
        lossy.acknowledged().is_empty(),
        "and the acknowledgement was lost"
    );

    // The same job comes back. It must not reach the print head.
    let again = StubEdge::handing(vec![leased(1)]);
    let handled = one_cycle(&again, &head, &written)
        .await
        .expect("the edge answers");
    assert_eq!(handled, 1);
    assert_eq!(head.written(), vec![event(1)], "still printed exactly once");
    assert_eq!(
        again.acknowledged(),
        vec![event(1)],
        "and this time the edge is told, so the job leaves the queue"
    );
}

#[tokio::test]
async fn a_refused_write_is_never_acknowledged() {
    // A job acknowledged after a failed write is a job deleted from the only place it exists. The
    // lease is what hands it back, so the agent's whole job here is to stay quiet.
    let directory = tempfile::tempdir().expect("temp dir");
    let written = record(&directory);
    let edge = StubEdge::handing(vec![leased(1)]);
    let head = StubHead {
        refuses: true,
        ..StubHead::default()
    };

    let handled = one_cycle(&edge, &head, &written)
        .await
        .expect("a jammed printer is not an edge failure");
    assert_eq!(handled, 0);
    assert!(edge.acknowledged().is_empty(), "nothing was acknowledged");
    assert!(
        !written.already_written(PRINTER, event(1)),
        "and nothing was recorded, so the retry really does print"
    );
}

#[tokio::test]
async fn one_printer_s_jam_does_not_stop_another_printer_s_ticket() {
    // At most one job per printer, but an agent may own several. A cycle that gave up on the first
    // refusal would let a jammed kitchen printer hold up the counter's receipt.
    let directory = tempfile::tempdir().expect("temp dir");
    let written = record(&directory);
    let kitchen = LeasedJob {
        printer_device_id: "00000000000000000000000092".to_owned(),
        ..leased(1)
    };
    let edge = StubEdge::handing(vec![kitchen, leased(2)]);

    let head = Jammed(Mutex::new(Vec::new()));

    let handled = one_cycle(&edge, &head, &written)
        .await
        .expect("the edge answers");
    assert_eq!(handled, 1, "the counter's ticket");
    assert_eq!(head.0.lock().expect("lock").clone(), vec![event(2)]);
    assert_eq!(edge.acknowledged(), vec![event(2)]);
}

#[tokio::test]
async fn a_record_that_will_not_parse_starts_empty_rather_than_refusing_to_run() {
    // A kitchen with no tickets is worse than one duplicate ticket, so a corrupt record is a warning
    // and a fresh start.
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("state.json");
    std::fs::write(&path, "{ this is not json").expect("seed a corrupt record");

    let written = LastWritten::load(&path);
    assert!(!written.already_written(PRINTER, event(1)));
    written
        .record(PRINTER, event(1))
        .expect("and it is writable");
    assert!(written.already_written(PRINTER, event(1)));
}

#[test]
fn the_device_token_comes_from_the_environment_and_the_file_is_the_fallback() {
    // A credential in a file that sits beside the state is a credential read by whoever is
    // diagnosing a printer. The generated installers put it in the service's own environment, so
    // this has to prefer that — and has to refuse when it is nowhere, because an agent that
    // silently never claims looks exactly like a printer nobody plugged in.
    let path = std::path::Path::new("print-agent.toml");
    let bare = || pos_print_agent::Config {
        edge_url: "http://192.0.2.10:8787".to_owned(),
        device_token: String::new(),
        state_path: "state.json".into(),
    };

    let refused = bare()
        .with_token(None, path)
        .expect_err("no token anywhere is a refusal");
    assert!(
        refused.to_string().contains("POS_PRINT_AGENT_TOKEN"),
        "the refusal says where to put it: {refused}"
    );
    assert!(
        bare().with_token(Some("   ".to_owned()), path).is_err(),
        "an empty variable is the same as an unset one, not a token of spaces"
    );

    let from_service = bare()
        .with_token(Some("from-the-service".to_owned()), path)
        .expect("the environment supplies it");
    assert_eq!(from_service.device_token, "from-the-service");

    let in_file = pos_print_agent::Config {
        device_token: "from-the-file".to_owned(),
        ..bare()
    };
    assert_eq!(
        in_file
            .clone()
            .with_token(Some("from-the-service".to_owned()), path)
            .expect("still loads")
            .device_token,
        "from-the-service",
        "the service environment is the credential's home, so it wins"
    );
    assert_eq!(
        in_file
            .with_token(None, path)
            .expect("a technician's hand-written file still works")
            .device_token,
        "from-the-file"
    );
}
