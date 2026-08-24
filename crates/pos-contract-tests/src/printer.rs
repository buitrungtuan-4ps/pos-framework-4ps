// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `PrinterDriver` suite.
//!
//! Two obligations here are unobservable through the port alone, which is why the harness has to
//! report physical outcomes. A deduplicated reprint and a real second ticket both return `Ok`;
//! only [`PrinterDriverHarness::tickets_printed`] tells them apart. And a drawer opening over the
//! network also returns `Ok` — only [`PrinterDriverHarness::drawer_opened`] catches it.
//!
//! That second one is the security case. `docs/architecture.md` §5: port 9100 has no
//! authentication of any kind, and the drawer-kick command rides the same channel as everything
//! else. Anyone on the network can open a network-attached drawer, so the framework refuses to
//! ask.

use pos_ports::PortName;
use pos_ports::printer::{
    CodePage, PrintBlock, PrintDocument, PrintJob, PrinterCapabilities, PrinterConnection,
    PrinterDriver, TextStyle,
};
use pos_proto::{ErrorStatus, StationId, StoreId, Ulid};

use crate::harness::PrinterDriverHarness;
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `PrinterDriver` case as a `#[test]`.
#[macro_export]
macro_rules! printer_driver_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_PRINTER_DRIVER,
            module = printer,
            cases = [
                prints_a_document,
                is_idempotent_by_job_id,
                a_reprint_is_a_new_job,
                returns_unavailable_when_offline_so_the_caller_requeues,
                refuses_when_out_of_paper,
                opens_a_usb_drawer,
                refuses_to_open_a_network_drawer,
                reports_capabilities_without_asking_the_hardware,
            ]
        }
    };
}

fn idempotency() -> Obligation {
    Obligation::new(PortName::PrinterDriver, "idempotency by job identifier")
}

fn queueing() -> Obligation {
    Obligation::new(
        PortName::PrinterDriver,
        "a queued job survives a busy printer",
    )
}

fn drawer_rule() -> Obligation {
    Obligation::new(PortName::PrinterDriver, "a drawer opens only over USB")
}

/// A USB thermal printer with a drawer, which is the common shop configuration.
fn usb_with_drawer() -> PrinterCapabilities {
    PrinterCapabilities {
        connection: PrinterConnection::Usb,
        code_page: CodePage::Ascii,
        columns: core::num::NonZeroU16::MIN.saturating_add(41),
        prints_bitmaps: true,
        cuts_paper: true,
        kicks_drawer: true,
    }
}

/// The same printer on the network, which is the configuration that must refuse the drawer.
fn network_with_drawer() -> PrinterCapabilities {
    PrinterCapabilities {
        connection: PrinterConnection::Network,
        ..usb_with_drawer()
    }
}

fn job(seed: u32) -> PrintJob {
    PrintJob {
        job_id: fixtures::event_id(seed),
        store_id: StoreId::new(Ulid::from_u128(1)),
        station_id: StationId::new(Ulid::from_u128(1)),
        document: PrintDocument {
            blocks: vec![
                PrintBlock::Text {
                    line: "TOTAL 120,000".to_owned(),
                    style: TextStyle {
                        emphasised: true,
                        double_size: true,
                        centred: false,
                    },
                },
                PrintBlock::Feed { lines: 2 },
                PrintBlock::Cut,
            ],
        },
    }
}

/// A ready printer prints.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn prints_a_document<H: PrinterDriverHarness>(harness: &H) -> Result<(), CaseFailure> {
    let printer = harness.fresh(usb_with_drawer()).await?;
    printer.print(&job(1)).await?;
    idempotency().require_eq(
        &harness.tickets_printed(&printer).await?,
        &1,
        "one job, one ticket",
    )
}

/// The same job twice produces one ticket.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_idempotent_by_job_id<H: PrinterDriverHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let printer = harness.fresh(usb_with_drawer()).await?;
    let job = job(1);
    printer.print(&job).await?;
    printer.print(&job).await?;
    idempotency().require_eq(
        &harness.tickets_printed(&printer).await?,
        &1,
        "a retried job produces one ticket. A flaky USB cable retries constantly, and without \
         this the kitchen gets two of everything",
    )
}

/// A different job identifier is a different ticket, deliberately.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_reprint_is_a_new_job<H: PrinterDriverHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let printer = harness.fresh(usb_with_drawer()).await?;
    printer.print(&job(1)).await?;
    printer.print(&job(2)).await?;
    idempotency().require_eq(
        &harness.tickets_printed(&printer).await?,
        &2,
        "a reprint mints a new job identifier and must produce a second ticket — \
         docs/pos-spec.md §12 counts reprint rates per employee, so a suppressed reprint would \
         also be a missing audit entry",
    )
}

/// An unreachable printer fails retryably rather than blocking.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn returns_unavailable_when_offline_so_the_caller_requeues<H: PrinterDriverHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let printer = harness.fresh(usb_with_drawer()).await?;
    harness.take_offline(&printer).await?;
    queueing().require_error(
        printer.print(&job(1)).await,
        ErrorStatus::Unavailable,
        "an unreachable printer must report unavailability so the print queue re-queues the job. \
         docs/pos-spec.md §2 puts printing behind a queue with retry, and a non-retryable status \
         would drop the ticket instead",
    )
}

/// Out of paper is a precondition, not a transient fault.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_when_out_of_paper<H: PrinterDriverHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let printer = harness.fresh(usb_with_drawer()).await?;
    harness.empty_paper(&printer).await?;
    let obligation = queueing();

    obligation.require_error(
        printer.print(&job(1)).await,
        ErrorStatus::FailedPrecondition,
        "an empty roll is a precondition failure, not unavailability. Retrying it forever would \
         spin instead of showing the red badge docs/ui-ux.md §4 requires",
    )?;

    let status = printer.status().await?;
    obligation.require(
        !status.is_ready(),
        "and the status must say so, or the badge has nothing to read",
    )
}

/// A USB drawer opens.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn opens_a_usb_drawer<H: PrinterDriverHarness>(harness: &H) -> Result<(), CaseFailure> {
    let printer = harness.fresh(usb_with_drawer()).await?;
    printer.open_drawer().await?;
    drawer_rule().require(
        harness.drawer_opened(&printer).await?,
        "a USB-attached drawer opens when asked",
    )
}

/// A network drawer does not, whatever the firmware would accept.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_to_open_a_network_drawer<H: PrinterDriverHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let printer = harness.fresh(network_with_drawer()).await?;
    let obligation = drawer_rule();

    obligation.require_error(
        printer.open_drawer().await,
        ErrorStatus::FailedPrecondition,
        "a network-attached drawer must be refused. Port 9100 has no authentication, so the \
         drawer-kick command is available to anyone on the network — the firmware will happily \
         accept it, which is exactly why the framework must not send it",
    )?;

    obligation.require(
        !harness.drawer_opened(&printer).await?,
        "and the drawer must not have opened. An adapter that returns an error after opening it \
         has satisfied the status check and broken the rule",
    )
}

/// Capabilities are configuration, so reading them costs nothing.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_capabilities_without_asking_the_hardware<H: PrinterDriverHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let printer = harness.fresh(usb_with_drawer()).await?;
    harness.take_offline(&printer).await?;
    let obligation = Obligation::new(
        PortName::PrinterDriver,
        "capabilities do not lie by omission",
    );

    // Offline on purpose. `capabilities` is synchronous and infallible, so an adapter that reads
    // the hardware inside it has no way to report failure and will either block or lie.
    let capabilities = printer.capabilities();
    obligation.require_eq(
        &capabilities.connection,
        &PrinterConnection::Usb,
        "capabilities are known without the printer answering",
    )?;
    obligation.require(
        capabilities.may_open_a_drawer(),
        "and they still describe the drawer",
    )?;

    // The bitmap decision belongs to the framework, and it needs the code page to make it.
    obligation.require_eq(
        &capabilities.needs_bitmap("Phở"),
        &Ok(true),
        "a line outside the code page needs a bitmap — pos-spec.md §13, and the reason \
         Vietnamese does not print as question marks",
    )?;
    obligation.require_eq(
        &capabilities.needs_bitmap("TOTAL"),
        &Ok(false),
        "and an ASCII line does not",
    )
}
