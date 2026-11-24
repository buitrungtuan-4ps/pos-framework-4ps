// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::einvoice::{EInvoiceIssueMoment, IssueLimit};
use pos_proto::time::Timestamp;

use super::{IssueSchedule, due_time};
use crate::business_date::StoreTimeZone;

fn at(rfc3339: &str) -> Timestamp {
    rfc3339.parse().expect("valid RFC 3339 in a test fixture")
}

fn saigon() -> StoreTimeZone {
    StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("a real zone")
}

fn schedule(moment: EInvoiceIssueMoment, delay_minutes: u32, batch_minute: u32) -> IssueSchedule {
    IssueSchedule {
        moment,
        delay_minutes,
        batch_minute,
    }
}

#[test]
fn a_mode_that_does_not_defer_has_no_due_time() {
    for moment in [
        EInvoiceIssueMoment::Off,
        EInvoiceIssueMoment::Immediate,
        EInvoiceIssueMoment::OnRequest,
        EInvoiceIssueMoment::Unspecified,
    ] {
        let due = due_time(
            schedule(moment, 30, 1_320),
            at("2026-11-24T05:00:00Z"),
            &saigon(),
            None,
        );
        assert_eq!(due, Ok(None), "{moment:?}");
    }
}

#[test]
fn a_scheduled_order_is_due_its_delay_after_the_settle() {
    // 12:00 in Saigon, 45 minutes later.
    let due = due_time(
        schedule(EInvoiceIssueMoment::ScheduledPerOrder, 45, 0),
        at("2026-11-24T05:00:00Z"),
        &saigon(),
        Some(IssueLimit::EndOfSaleDay),
    );
    assert_eq!(due, Ok(Some(at("2026-11-24T05:45:00Z"))));
}

#[test]
fn a_batch_is_due_at_the_days_window_or_the_next_days() {
    let batch = schedule(EInvoiceIssueMoment::BatchWindow, 0, 1_320);
    // Settled 12:00 local: today's 22:00 window, 15:00Z.
    assert_eq!(
        due_time(batch, at("2026-11-24T05:00:00Z"), &saigon(), None),
        Ok(Some(at("2026-11-24T15:00:00Z")))
    );
    // Settled 22:30 local, after the window and with no limit: tomorrow's.
    assert_eq!(
        due_time(batch, at("2026-11-24T15:30:00Z"), &saigon(), None),
        Ok(Some(at("2026-11-25T15:00:00Z")))
    );
}

#[test]
fn the_legal_limit_wins_over_any_schedule() {
    let limit = Some(IssueLimit::EndOfSaleDay);
    // Settled 22:30 local: the next window is tomorrow, past the end of the sale's day, so the
    // invoice is due at 23:59:59 local, 16:59:59Z.
    assert_eq!(
        due_time(
            schedule(EInvoiceIssueMoment::BatchWindow, 0, 1_320),
            at("2026-11-24T15:30:00Z"),
            &saigon(),
            limit
        ),
        Ok(Some(at("2026-11-24T16:59:59Z")))
    );
    // A delay that would cross midnight is held to it too.
    assert_eq!(
        due_time(
            schedule(EInvoiceIssueMoment::ScheduledPerOrder, 120, 0),
            at("2026-11-24T16:30:00Z"),
            &saigon(),
            limit
        ),
        Ok(Some(at("2026-11-24T16:59:59Z")))
    );
    // A limit in minutes.
    assert_eq!(
        due_time(
            schedule(EInvoiceIssueMoment::ScheduledPerOrder, 120, 0),
            at("2026-11-24T05:00:00Z"),
            &saigon(),
            Some(IssueLimit::MinutesAfterSale(30))
        ),
        Ok(Some(at("2026-11-24T05:30:00Z")))
    );
}
