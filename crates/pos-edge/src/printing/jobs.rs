// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a till asks to print: the receipt and its copy, the pre-bill, the shift reports, a ticket
//! and the test page.

use pos_core::billing::BillTotals;
use pos_core::business_date::local_time;
use pos_ports::printer::{PrintDocument, PrintJob};
use pos_proto::ids::{DeviceId, EventId, StoreId};

use crate::app::{
    BuyerDetails, CombinedReport, EdgeSession, FiredLine, PreBill, ReceiptCopy, ReceiptLine,
    ShiftReport,
};
use crate::line_notes::NoteText;
use crate::printing::blocks::{MoneyStyle, printed_totals};
use crate::printing::devices::{TillPrinting, guest_printer, published_printers, station_printer};
use crate::printing::documents::{
    combined_shift_report_document, pre_bill_document, receipt_copy_document, receipt_document,
    shift_report_document, test_page_document,
};
use crate::printing::language::receipt_lines_in;
use crate::printing::tickets::ticket_at;
use crate::printing::{PrintOutcome, Printers};

impl Printers {
    /// Prints the guest's receipt for a settled bill, on `till`'s receipt printer: the store's,
    /// unless the till's terminal names its own that the node lists as a printer serving no
    /// station.
    ///
    /// `job_id` is the idempotency key: pass the settle's event id and a retried settle reprints
    /// nothing. Never returns an error — a printer that is down must not roll back a bill the guest
    /// has already paid, so the outcome is reported and the caller decides what to tell the cashier.
    #[expect(
        clippy::too_many_arguments,
        reason = "the receipt's own arguments plus the till it is printed for; a struct for them \
                  would be built at one call site and taken apart here"
    )]
    pub async fn print_receipt(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        store_id: StoreId,
        job_id: EventId,
        receipt_number: u64,
        lines: &[ReceiptLine],
        totals: &BillTotals,
        buyer: Option<&BuyerDetails>,
    ) -> PrintOutcome {
        let device = match guest_printer(&session.devices, till) {
            Ok(device) => device,
            Err(outcome) => return outcome,
        };
        // The store's own identity, from the `store_profile` node the config pull applies
        // (ADR-0106). Empty until somebody fills it in, and the document is then what it was before.
        let printed = receipt_lines_in(session, till, lines);
        let totals = printed_totals(session, till, totals);
        let second = self.second_language_for(session, till, device, lines, &printed, &totals);
        let document = receipt_document(
            &session.profile,
            &MoneyStyle::of(session),
            self.receipt_labels_for(session, till),
            receipt_number,
            &printed,
            &totals,
            buyer,
            second.as_ref(),
        );
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: None,
                document,
            },
        )
        .await
    }

    /// Prints a copy of a settled bill's receipt on `till`'s receipt printer, as the receipt is
    /// ([ADR-0164](../../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)),
    /// marked as a copy with the store's local time it was printed at.
    ///
    /// The job is keyed on the copy's own event ([`ReceiptCopy::job_id`]), so a job retried after an
    /// ambiguous failure prints once and the next press, which is a new copy, prints again.
    pub async fn print_receipt_copy(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        store_id: StoreId,
        copy: &ReceiptCopy,
        buyer: Option<&BuyerDetails>,
    ) -> PrintOutcome {
        let device = match guest_printer(&session.devices, till) {
            Ok(device) => device,
            Err(outcome) => return outcome,
        };
        // An instant always reads as a local time; the empty fallback is for a timestamp outside
        // what the calendar can represent, which a copy printed now cannot have.
        let reprinted = local_time(copy.reprinted_time, &session.timezone)
            .map(|time| time.to_string())
            .unwrap_or_default();
        let printed = receipt_lines_in(session, till, &copy.lines);
        let totals = printed_totals(session, till, &copy.totals);
        let second =
            self.second_language_for(session, till, device, &copy.lines, &printed, &totals);
        let document = receipt_copy_document(
            &session.profile,
            &MoneyStyle::of(session),
            self.receipt_labels_for(session, till),
            copy.receipt_number,
            copy.copy_number,
            &reprinted,
            &printed,
            &totals,
            buyer,
            second.as_ref(),
        );
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id: copy.job_id,
                store_id,
                station_id: None,
                document,
            },
        )
        .await
    }

    /// Prints a pre-bill on `till`'s receipt printer — the same device its receipt goes to, since
    /// both are handed to the guest.
    ///
    /// `job_id` should be fresh for each press: a table that asks twice gets two pieces of paper, and
    /// unlike a settle there is nothing to deduplicate against.
    pub async fn print_pre_bill(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        store_id: StoreId,
        job_id: EventId,
        reference: &str,
        pre_bill: &PreBill,
    ) -> PrintOutcome {
        let device = match guest_printer(&session.devices, till) {
            Ok(device) => device,
            Err(outcome) => return outcome,
        };
        let printed = receipt_lines_in(session, till, &pre_bill.lines);
        let totals = printed_totals(session, till, &pre_bill.totals);
        let second =
            self.second_language_for(session, till, device, &pre_bill.lines, &printed, &totals);
        let document = pre_bill_document(
            &session.profile,
            &MoneyStyle::of(session),
            self.receipt_labels_for(session, till),
            reference,
            &printed,
            &totals,
            second.as_ref(),
        );
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: None,
                document,
            },
        )
        .await
    }

    /// Prints a closed shift's report where it was closed: at the receipt printer of `till`, the
    /// till the close was made at, and at the store's for [`TillPrinting::STORE`], which is where
    /// every report printed before a till had a drawer of its own
    /// ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 10).
    ///
    /// Never returns an error, for the reason [`Self::print_receipt`] gives: the shift is closed
    /// whether the paper comes out or not, and the caller tells the cashier which.
    pub async fn print_shift_report(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        store_id: StoreId,
        job_id: EventId,
        report: &ShiftReport,
    ) -> PrintOutcome {
        let document = shift_report_document(
            &session.profile,
            &MoneyStyle::of(session),
            self.labels_for(session),
            report,
        );
        self.print_cash_up(session, till, store_id, job_id, document)
            .await
    }

    /// Prints the one slip for several drawers closed at once, where they were closed, as
    /// [`Self::print_shift_report`] prints one drawer's.
    pub async fn print_combined_report(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        store_id: StoreId,
        job_id: EventId,
        combined: &CombinedReport,
    ) -> PrintOutcome {
        let document = combined_shift_report_document(
            &session.profile,
            &MoneyStyle::of(session),
            self.labels_for(session),
            combined,
        );
        self.print_cash_up(session, till, store_id, job_id, document)
            .await
    }

    /// Sends a cash-up document to `till`'s receipt printer, the store's where it names none.
    async fn print_cash_up(
        &self,
        session: &EdgeSession,
        till: &TillPrinting,
        store_id: StoreId,
        job_id: EventId,
        document: PrintDocument,
    ) -> PrintOutcome {
        let device = match guest_printer(&session.devices, till) {
            Ok(device) => device,
            Err(outcome) => return outcome,
        };
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: None,
                document,
            },
        )
        .await
    }

    /// Prints the kitchen ticket for `fired` at its station, or at the station plan's declared
    /// backup.
    ///
    /// The failover is the plan's (ADR-0072, ADR-0100): one hop, to the station an operator named.
    /// The ticket is in the language of the station whose printer prints it ([`ticket_at`]), the
    /// backup's on a failover, because its cooks read it.
    ///
    /// `note` is the guest note the edge holds for the line, if it still holds one (ADR-0157).
    pub async fn print_ticket(
        &self,
        session: &EdgeSession,
        store_id: StoreId,
        job_id: EventId,
        reference: &str,
        fired: &FiredLine,
        note: Option<&NoteText>,
    ) -> PrintOutcome {
        let Some((printing, device)) =
            station_printer(&session.devices, &session.stations, fired.station_id)
        else {
            tracing::info!(
                "no printer is published for that station or its backup; the kitchen display still has the order"
            );
            return PrintOutcome::NoPrinter;
        };
        let document = ticket_at(
            session,
            self.can_rasterise(),
            printing,
            reference,
            fired,
            note,
        );
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: Some(fired.station_id),
                document,
            },
        )
        .await
    }

    /// Prints [`test_page_document`] on the printer `device_id`, through the same dispatch a receipt
    /// takes — direct or through its agent (ADR-0112) — so a page that prints proves the path a
    /// receipt will take.
    ///
    /// [`PrintOutcome::NoPrinter`] for an id this store has not published as a printer.
    pub async fn print_test_page(
        &self,
        session: &EdgeSession,
        store_id: StoreId,
        job_id: EventId,
        device_id: DeviceId,
        printed_at: &str,
    ) -> PrintOutcome {
        let Some(device) = published_printers(&session.devices)
            .into_iter()
            .find(|device| device.device_id == device_id)
        else {
            return PrintOutcome::NoPrinter;
        };
        let document =
            test_page_document(&session.profile, device, printed_at, self.can_rasterise());
        self.dispatch(
            session,
            device,
            PrintJob {
                job_id,
                store_id,
                station_id: device.station_id,
                document,
            },
        )
        .await
    }
}
