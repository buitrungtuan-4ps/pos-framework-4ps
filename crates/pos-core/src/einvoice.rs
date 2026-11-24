// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! When a deferred e-invoice falls due
//! ([ADR-0229](../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md) decisions 1 and 2).
//!
//! Pure: the settle's instant, the store's schedule and zone, and the country's legal limit in; the
//! due instant out. The limit always wins, so a schedule that would issue too late issues at the
//! limit instead.

use jiff::Timestamp as Instant;
use jiff::civil::Time;
use pos_proto::einvoice::{EInvoiceIssueMoment, IssueLimit};
use pos_proto::time::Timestamp;

use crate::business_date::{ClockError, StoreTimeZone};

/// The store's schedule, as its `einvoice` node gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueSchedule {
    /// The mode.
    pub moment: EInvoiceIssueMoment,
    /// Minutes after the settle, for `SCHEDULED_PER_ORDER`.
    pub delay_minutes: u32,
    /// The minute of the local day, for `BATCH_WINDOW`.
    pub batch_minute: u32,
}

/// When an invoice for a bill settled at `settled` falls due, or `None` when the mode does not defer
/// it (it issues with the settle, or not at all).
///
/// # Errors
///
/// [`ClockError::OutOfRange`] if an instant or a civil time is outside the representable range.
pub fn due_time(
    schedule: IssueSchedule,
    settled: Timestamp,
    zone: &StoreTimeZone,
    limit: Option<IssueLimit>,
) -> Result<Option<Timestamp>, ClockError> {
    let start = Instant::from_millisecond(settled.as_milliseconds_since_epoch())
        .map_err(|_| ClockError::OutOfRange)?;
    let local = start.to_zoned(zone.zone().clone());
    let wanted = match schedule.moment {
        EInvoiceIssueMoment::ScheduledPerOrder => {
            start.checked_add(jiff::Span::new().minutes(i64::from(schedule.delay_minutes)))
        }
        EInvoiceIssueMoment::BatchWindow => {
            let minute = schedule.batch_minute.min(1_439);
            let hour = i8::try_from(minute / 60).map_err(|_| ClockError::OutOfRange)?;
            let minute = i8::try_from(minute % 60).map_err(|_| ClockError::OutOfRange)?;
            let at = Time::new(hour, minute, 0, 0).map_err(|_| ClockError::OutOfRange)?;
            let today = local
                .with()
                .time(at)
                .build()
                .map_err(|_| ClockError::OutOfRange)?;
            let window = if today.timestamp() > start {
                Ok(today)
            } else {
                today.tomorrow()
            };
            window.map(|zoned| zoned.timestamp())
        }
        EInvoiceIssueMoment::Unspecified
        | EInvoiceIssueMoment::Off
        | EInvoiceIssueMoment::Immediate
        | EInvoiceIssueMoment::OnRequest => return Ok(None),
    }
    .map_err(|_| ClockError::OutOfRange)?;
    let latest = match limit {
        None => wanted,
        Some(IssueLimit::MinutesAfterSale(minutes)) => start
            .checked_add(jiff::Span::new().minutes(i64::from(minutes)))
            .map_err(|_| ClockError::OutOfRange)?,
        Some(IssueLimit::EndOfSaleDay) => {
            let midnight = local
                .start_of_day()
                .and_then(|today| today.tomorrow())
                .map_err(|_| ClockError::OutOfRange)?;
            midnight
                .timestamp()
                .checked_sub(jiff::Span::new().seconds(1))
                .map_err(|_| ClockError::OutOfRange)?
        }
    };
    Timestamp::from_milliseconds_since_epoch(wanted.min(latest).as_millisecond())
        .map(Some)
        .map_err(ClockError::from)
}

#[cfg(test)]
mod tests;
