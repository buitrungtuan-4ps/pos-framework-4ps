// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_fakes::executor::run_ready;
use pos_ports::fiscalization::{InvoiceNumber, InvoiceRequest};
use pos_proto::ErrorStatus;
use pos_proto::ids::{BillId, StoreId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::time::{CalendarDate, Timestamp};
use pos_proto::ulid::Ulid;

use super::{
    PassReport, QUEUE_LIMIT, QueueFull, SubmissionQueue, SubmissionState, retry_wait_seconds,
    retryable,
};
use crate::einvoice::sandbox::{SandboxEInvoice, SandboxMode};

fn day(year: i16, month: u8, date: u8) -> CalendarDate {
    CalendarDate::from_ymd(year, month, date).expect("a date")
}

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(1_800_000_000_000 + seconds * 1_000)
        .expect("an instant")
}

fn invoice(seed: u128, sequence: u32) -> InvoiceRequest {
    InvoiceRequest {
        bill_id: BillId::new(Ulid::from_u128(seed)),
        store_id: StoreId::new(Ulid::from_u128(1)),
        issued_on: day(2026, 11, 24),
        buyer: None,
        lines: Vec::new(),
        total: Money::new(CurrencyCode::VND, 165_000),
        issued_number: Some(InvoiceNumber::new(format!("1C26MAA/{sequence:08}"))),
        correction: None,
    }
}

#[test]
fn an_accepted_invoice_is_submitted_once() {
    let sandbox = SandboxEInvoice::new();
    let mut queue = SubmissionQueue::new();
    queue.enqueue(invoice(1, 1)).expect("queued");
    queue
        .enqueue(invoice(1, 1))
        .expect("an event delivered twice is one submission");
    let report = run_ready(queue.run_due(&sandbox, at(0)));
    assert_eq!(
        report,
        PassReport {
            submitted: 1,
            ..PassReport::default()
        }
    );
    let state = &queue
        .get(BillId::new(Ulid::from_u128(1)))
        .expect("queued")
        .state;
    assert_eq!(
        *state,
        SubmissionState::Submitted {
            authority_reference: Some("SANDBOX-1C26MAA/00000001".into())
        }
    );
    let again = run_ready(queue.run_due(&sandbox, at(10)));
    assert_eq!(again, PassReport::default(), "nothing is submitted twice");
}

#[test]
fn a_timeout_is_retried_after_a_doubling_wait_and_then_goes_through() {
    let sandbox = SandboxEInvoice::new();
    sandbox.set_mode(SandboxMode::TimeOut);
    let mut queue = SubmissionQueue::new();
    queue.enqueue(invoice(2, 1)).expect("queued");
    let bill = BillId::new(Ulid::from_u128(2));

    assert_eq!(run_ready(queue.run_due(&sandbox, at(0))).retrying, 1);
    assert_eq!(
        queue.get(bill).expect("queued").state,
        SubmissionState::Retrying {
            next_attempt: at(60)
        }
    );
    assert_eq!(
        run_ready(queue.run_due(&sandbox, at(30))),
        PassReport::default(),
        "not tried again before its wait"
    );
    assert_eq!(run_ready(queue.run_due(&sandbox, at(60))).retrying, 1);
    assert_eq!(
        queue.get(bill).expect("queued").state,
        SubmissionState::Retrying {
            next_attempt: at(180)
        },
        "the second wait is two minutes"
    );

    sandbox.set_mode(SandboxMode::Accept);
    assert_eq!(run_ready(queue.run_due(&sandbox, at(180))).submitted, 1);
}

#[test]
fn a_refusal_stops_and_is_never_resubmitted_unchanged() {
    let sandbox = SandboxEInvoice::new();
    sandbox.set_mode(SandboxMode::Refuse);
    let mut queue = SubmissionQueue::new();
    queue.enqueue(invoice(3, 1)).expect("queued");
    assert_eq!(run_ready(queue.run_due(&sandbox, at(0))).rejected, 1);
    sandbox.set_mode(SandboxMode::Accept);
    assert_eq!(
        run_ready(queue.run_due(&sandbox, at(7_200))),
        PassReport::default(),
        "a refused invoice is corrected in the console"
    );
    assert_eq!(
        queue
            .get(BillId::new(Ulid::from_u128(3)))
            .expect("queued")
            .state,
        SubmissionState::Rejected
    );
}

#[test]
fn an_invoice_not_accepted_by_the_deadline_is_overdue() {
    let sandbox = SandboxEInvoice::new();
    let mut queue = SubmissionQueue::new();
    queue.enqueue(invoice(4, 1)).expect("queued");
    queue.enqueue(invoice(5, 2)).expect("queued");
    sandbox.set_mode(SandboxMode::TimeOut);
    let _waiting = run_ready(queue.run_due(&sandbox, at(0)));
    // Dated 24 November; the deadline here is illustrative, the country's is to be confirmed.
    assert!(queue.overdue(day(2026, 11, 24), 1).is_empty());
    assert_eq!(queue.overdue(day(2026, 11, 25), 1).len(), 2);
    assert_eq!(
        queue.overdue(day(2027, 1, 1), 30).len(),
        2,
        "counted across a month and a year by the calendar"
    );
    sandbox.set_mode(SandboxMode::Accept);
    let _through = run_ready(queue.run_due(&sandbox, at(60)));
    assert!(queue.overdue(day(2027, 1, 1), 1).is_empty());
}

#[test]
fn the_wait_doubles_from_a_minute_to_at_most_an_hour() {
    let waits: Vec<i64> = (1..=8).map(retry_wait_seconds).collect();
    assert_eq!(waits, [60, 120, 240, 480, 960, 1_920, 3_600, 3_600]);
    assert_eq!(retry_wait_seconds(u32::MAX), 3_600);
}

#[test]
fn only_a_failure_that_may_pass_is_retried() {
    assert!(retryable(ErrorStatus::Unavailable));
    assert!(retryable(ErrorStatus::ResourceExhausted));
    assert!(retryable(ErrorStatus::Internal));
    assert!(!retryable(ErrorStatus::InvalidArgument));
    assert!(!retryable(ErrorStatus::PermissionDenied));
    assert!(!retryable(ErrorStatus::FailedPrecondition));
}

#[test]
fn a_full_queue_refuses_rather_than_drops() {
    let mut queue = SubmissionQueue::new();
    for seed in 0..QUEUE_LIMIT {
        let seed = u128::try_from(seed).expect("fits") + 1_000;
        queue.enqueue(invoice(seed, 1)).expect("room");
    }
    assert_eq!(queue.enqueue(invoice(99, 1)), Err(QueueFull));
}
