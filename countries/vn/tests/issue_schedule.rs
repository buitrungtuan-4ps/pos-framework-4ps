// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Vietnam's acceptance cases for the e-invoice issue schedule
//! ([ADR-0229](../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md)): each mode, run
//! through `pos_core::einvoice::due_time` with this pack's own legal limit. `CONFORMANCE.md` R18 and
//! R24 map them. The limit is the conservative reading and is to be confirmed by legal.

use pos_core::business_date::StoreTimeZone;
use pos_core::einvoice::{IssueSchedule, due_time};
use pos_country::{CountryModule, SettingDefault, SettingDefaultValue};
use pos_country_vn::Vietnam;
use pos_country_vn::einvoice::{ISSUE_LIMIT, IssueMoment, RECOMMENDED_ISSUE_MOMENT};
use pos_proto::einvoice::IssueLimit;
use pos_proto::time::Timestamp;

#[cfg(test)]
fn at(rfc3339: &str) -> Timestamp {
    rfc3339.parse().expect("valid RFC 3339 in a test fixture")
}

#[cfg(test)]
fn saigon() -> StoreTimeZone {
    StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("a real zone")
}

#[cfg(test)]
fn due(moment: IssueMoment, delay: u32, batch: u32, settled: &str) -> Option<Timestamp> {
    due_time(
        IssueSchedule {
            moment,
            delay_minutes: delay,
            batch_minute: batch,
        },
        at(settled),
        &saigon(),
        Vietnam.einvoice_issue_limit(),
    )
    .expect("a due time")
}

#[test]
fn the_pack_seeds_immediate_and_states_the_end_of_the_sales_day() {
    assert_eq!(RECOMMENDED_ISSUE_MOMENT, IssueMoment::Immediate);
    assert!(Vietnam.setting_defaults().contains(&SettingDefault {
        setting_key: "einvoice.issue_moment",
        value: SettingDefaultValue::Choice("EINVOICE_ISSUE_MOMENT_IMMEDIATE"),
    }));
    assert_eq!(Vietnam.einvoice_issue_limit(), Some(ISSUE_LIMIT));
    assert_eq!(ISSUE_LIMIT, IssueLimit::EndOfSaleDay);
    assert_eq!(
        ISSUE_LIMIT.max_delay_minutes(),
        1_439,
        "the cloud refuses a delay of a day or more"
    );
}

#[test]
fn immediate_on_request_and_off_issue_with_the_settle_or_not_at_all() {
    for moment in [
        IssueMoment::Immediate,
        IssueMoment::OnRequest,
        IssueMoment::Off,
    ] {
        assert_eq!(
            due(moment, 30, 1_320, "2026-11-24T05:00:00Z"),
            None,
            "{moment:?}"
        );
    }
    assert!(IssueMoment::Immediate.issues(false));
    assert!(!IssueMoment::OnRequest.issues(false));
    assert!(IssueMoment::OnRequest.issues(true));
    assert!(!IssueMoment::Off.issues(true));
}

#[test]
fn a_scheduled_order_is_issued_its_delay_after_the_settle_within_the_day() {
    // Settled 12:00 in Saigon, issued at 12:30.
    assert_eq!(
        due(
            IssueMoment::ScheduledPerOrder,
            30,
            0,
            "2026-11-24T05:00:00Z"
        ),
        Some(at("2026-11-24T05:30:00Z"))
    );
}

#[test]
fn a_batch_is_issued_at_the_days_window() {
    // Settled 12:00, issued in the 22:00 batch.
    assert_eq!(
        due(IssueMoment::BatchWindow, 0, 1_320, "2026-11-24T05:00:00Z"),
        Some(at("2026-11-24T15:00:00Z"))
    );
}

#[test]
fn no_schedule_issues_after_the_end_of_the_sales_day() {
    // Settled 22:30, after the batch: tomorrow's batch would be too late, so 23:59:59 today.
    assert_eq!(
        due(IssueMoment::BatchWindow, 0, 1_320, "2026-11-24T15:30:00Z"),
        Some(at("2026-11-24T16:59:59Z"))
    );
    // Settled 23:30 with an hour's delay: held to 23:59:59 the same day.
    assert_eq!(
        due(
            IssueMoment::ScheduledPerOrder,
            60,
            0,
            "2026-11-24T16:30:00Z"
        ),
        Some(at("2026-11-24T16:59:59Z"))
    );
}
