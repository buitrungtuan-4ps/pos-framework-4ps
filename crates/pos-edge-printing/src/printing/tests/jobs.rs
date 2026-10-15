// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Printing a receipt end to end: a settled bill reaches its printer, a store with none says so, a
//! printer that does not answer is reported, a retried settle prints once.

use pos_proto::devices::{DeviceKind, PublishedDevices};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::printing::ReceiptLanguage;

use crate::printing::PrintOutcome;
use crate::printing::tests::tickets::{a_kitchen, fired_at, oven};
use crate::printing::tests::{
    STORE, device, event_id, recorder, session_with, store_id, totals_of,
};

#[tokio::test]
async fn a_settled_bill_reaches_the_receipt_printer() {
    let (printers, recorder) = recorder(true);
    let session = session_with(PublishedDevices::new(vec![device(
        2,
        DeviceKind::Printer,
        None,
    )]));

    let outcome = printers
        .print_receipt(
            &session,
            &STORE,
            store_id(),
            event_id(1),
            42,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 99_000)),
            None,
        )
        .await;

    assert_eq!(outcome, PrintOutcome::Printed);
    let written = recorder.written.lock().expect("the recorder");
    assert_eq!(written.len(), 1, "one receipt, one write");
    let bytes = written.first().expect("the receipt");
    assert!(
        bytes.starts_with(&printer_escpos::escpos::INIT),
        "a document begins with the initialise command"
    );
    assert!(
        bytes.windows(3).any(|window| window == b"#42"),
        "the receipt number is on the paper"
    );
}

#[tokio::test]
async fn a_store_with_no_printer_says_so_rather_than_claiming_it_printed() {
    // The bug this slice exists to close: the till has rendered "Printing receipt…" over exactly
    // this state since P5.
    let (printers, _recorder) = recorder(true);
    let session = session_with(PublishedDevices::new(Vec::new()));

    let outcome = printers
        .print_receipt(
            &session,
            &STORE,
            store_id(),
            event_id(1),
            42,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 99_000)),
            None,
        )
        .await;

    assert_eq!(outcome, PrintOutcome::NoPrinter);
    assert!(!outcome.printed());
    assert_eq!(outcome.as_wire(), "NO_PRINTER");
}

#[tokio::test]
async fn a_printer_that_does_not_answer_is_reported_and_does_not_fail_the_settle() {
    // The guest has already paid. A printer that is down is news for the cashier, not a reason to
    // unwind a settled bill.
    let (printers, _recorder) = recorder(false);
    let session = session_with(PublishedDevices::new(vec![device(
        2,
        DeviceKind::Printer,
        None,
    )]));

    let outcome = printers
        .print_receipt(
            &session,
            &STORE,
            store_id(),
            event_id(1),
            42,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 99_000)),
            None,
        )
        .await;

    assert_eq!(outcome, PrintOutcome::Unavailable);
    assert_eq!(outcome.as_wire(), "PRINTER_UNAVAILABLE");
}

#[tokio::test]
async fn one_settle_retried_prints_one_receipt() {
    // The adapter is idempotent by job id and the dispatcher holds the printer, which is what
    // makes that idempotency reach across two calls.
    let (printers, recorder) = recorder(true);
    let session = session_with(PublishedDevices::new(vec![device(
        2,
        DeviceKind::Printer,
        None,
    )]));
    let total = Money::new(CurrencyCode::VND, 99_000);

    for _ in 0..2 {
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(1),
                42,
                &[],
                &totals_of(total),
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::Printed);
    }

    assert_eq!(
        recorder.written.lock().expect("the recorder").len(),
        1,
        "a retried settle must not hand the guest a second receipt"
    );
}

#[tokio::test]
async fn a_kitchen_ticket_a_printer_cannot_spell_is_refused_when_no_font_is_installed() {
    // The remaining half of the ADR-0102 story, and the one an operator has to be told about: a
    // box with no font package installed can draw nothing, so a Vietnamese item name is still
    // refused. Refusing beats sending the bytes — question marks on a kitchen ticket are how the
    // wrong dish gets made — and the KDS still shows the order. The companion test below is the
    // same ticket on a box that *has* fonts.
    let (printers, recorder) = recorder(true);
    // A station that sets no language: the dish prints as the menu names it.
    let session = a_kitchen(None, ReceiptLanguage::Display, ReceiptLanguage::Display);
    let outcome = printers
        .print_ticket(
            &session,
            store_id(),
            event_id(1),
            "A1",
            &fired_at(oven(), vec![]),
            None,
        )
        .await;

    assert_eq!(outcome, PrintOutcome::Unprintable);
    assert_eq!(outcome.as_wire(), "UNPRINTABLE_TEXT");
    assert!(
        recorder.written.lock().expect("the recorder").is_empty(),
        "nothing goes to the printer when it cannot spell the dish"
    );
}
