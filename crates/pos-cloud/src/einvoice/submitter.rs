// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Submitting the invoices stores issued, with retry
//! ([ADR-0225](../../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md)
//! decision 6).
//!
//! A store numbers its invoice in the sale and the event reaches the cloud whenever the store is
//! online. This queue holds each invoice until the tenant's provider has it:
//!
//! - an unreachable provider, or one that does not answer in time, is **retried** with a doubling
//!   wait from one minute up to an hour, idempotent by the bill, so a retry never makes a second
//!   invoice;
//! - a provider that **refuses** an invoice stops it: it is not resubmitted unchanged, and is
//!   corrected in the console by an adjustment or a replacement (decision 7);
//! - an invoice still not accepted a set number of days after its date is **overdue**, which the
//!   alert reads. The deadline is the country's and is to be confirmed by legal.
//!
//! The queue is bounded (AGENTS.md §2). One that is full refuses the next invoice rather than drop
//! one, and the caller keeps it in the event log, which is the queue of record.

use std::collections::BTreeMap;

use pos_ports::fiscalization::{Fiscalization, InvoiceRequest};
use pos_proto::ErrorStatus;
use pos_proto::ids::BillId;
use pos_proto::time::{CalendarDate, Timestamp};

/// The most invoices the queue holds at once.
pub const QUEUE_LIMIT: usize = 10_000;

/// The first wait after a failed attempt, in seconds.
pub const RETRY_FIRST_SECONDS: i64 = 60;

/// The longest wait between attempts, in seconds.
pub const RETRY_LONGEST_SECONDS: i64 = 3_600;

/// Where one invoice's submission stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionState {
    /// Waiting for its first attempt.
    Pending,
    /// Failed for a reason that may pass; tried again at `next_attempt`.
    Retrying {
        /// When it is next tried.
        next_attempt: Timestamp,
    },
    /// The provider has it.
    Submitted {
        /// The provider's own reference, where it gave one.
        authority_reference: Option<Box<str>>,
    },
    /// The provider refused it. Corrected in the console, never resubmitted unchanged.
    Rejected,
}

/// One invoice in the queue.
#[derive(Debug, Clone)]
pub struct Submission {
    /// What is submitted, carrying the store's own number.
    pub request: InvoiceRequest,
    /// Where it stands.
    pub state: SubmissionState,
    /// How many attempts failed so far.
    pub failed_attempts: u32,
}

/// The queue was full, and the invoice stays in the log for the next pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueFull;

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassReport {
    /// Accepted by the provider.
    pub submitted: u32,
    /// Refused by the provider.
    pub rejected: u32,
    /// Failed for now, and waiting to be retried.
    pub retrying: u32,
}

/// The invoices waiting for the provider, by bill.
#[derive(Debug, Default)]
pub struct SubmissionQueue {
    submissions: BTreeMap<BillId, Submission>,
}

impl SubmissionQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an invoice. A bill already queued keeps its place and its state, so an event delivered
    /// twice is one submission.
    ///
    /// # Errors
    ///
    /// [`QueueFull`] at [`QUEUE_LIMIT`].
    pub fn enqueue(&mut self, request: InvoiceRequest) -> Result<(), QueueFull> {
        if self.submissions.contains_key(&request.bill_id) {
            return Ok(());
        }
        if self.submissions.len() >= QUEUE_LIMIT {
            return Err(QueueFull);
        }
        self.submissions.insert(
            request.bill_id,
            Submission {
                request,
                state: SubmissionState::Pending,
                failed_attempts: 0,
            },
        );
        Ok(())
    }

    /// The submission for `bill_id`, if it is queued.
    #[must_use]
    pub fn get(&self, bill_id: BillId) -> Option<&Submission> {
        self.submissions.get(&bill_id)
    }

    /// Tries every submission that is due at `now` once, through `provider`.
    pub async fn run_due<F: Fiscalization>(&mut self, provider: &F, now: Timestamp) -> PassReport {
        let mut report = PassReport::default();
        for submission in self.submissions.values_mut() {
            let due = match &submission.state {
                SubmissionState::Pending => true,
                SubmissionState::Retrying { next_attempt } => *next_attempt <= now,
                SubmissionState::Submitted { .. } | SubmissionState::Rejected => false,
            };
            if !due {
                continue;
            }
            match provider.issue(&submission.request).await {
                Ok(issued) => {
                    submission.state = SubmissionState::Submitted {
                        authority_reference: issued.authority_reference,
                    };
                    report.submitted = report.submitted.saturating_add(1);
                }
                Err(error) if retryable(error.status()) => {
                    submission.failed_attempts = submission.failed_attempts.saturating_add(1);
                    submission.state = SubmissionState::Retrying {
                        next_attempt: later(now, retry_wait_seconds(submission.failed_attempts)),
                    };
                    report.retrying = report.retrying.saturating_add(1);
                }
                Err(_refused) => {
                    submission.failed_attempts = submission.failed_attempts.saturating_add(1);
                    submission.state = SubmissionState::Rejected;
                    report.rejected = report.rejected.saturating_add(1);
                }
            }
        }
        report
    }

    /// The bills whose invoice is not yet accepted `deadline_days` or more after its date: what
    /// the deadline alert names. A refused invoice counts, because it too is not with the provider.
    #[must_use]
    pub fn overdue(&self, today: CalendarDate, deadline_days: i32) -> Vec<BillId> {
        self.submissions
            .values()
            .filter(|submission| !matches!(submission.state, SubmissionState::Submitted { .. }))
            .filter(|submission| days_between(submission.request.issued_on, today) >= deadline_days)
            .map(|submission| submission.request.bill_id)
            .collect()
    }
}

/// Whether a failure may pass on its own: the provider unreachable, busy or broken. Anything else
/// is the provider's refusal of this invoice.
#[must_use]
pub const fn retryable(status: ErrorStatus) -> bool {
    matches!(
        status,
        ErrorStatus::Unavailable | ErrorStatus::ResourceExhausted | ErrorStatus::Internal
    )
}

/// How long to wait after the `failed_attempts`-th failure: one minute, doubling, at most an hour.
#[must_use]
pub fn retry_wait_seconds(failed_attempts: u32) -> i64 {
    let doublings = failed_attempts.saturating_sub(1).min(16);
    RETRY_FIRST_SECONDS
        .saturating_mul(1_i64 << doublings)
        .min(RETRY_LONGEST_SECONDS)
}

fn later(now: Timestamp, seconds: i64) -> Timestamp {
    let millis = now
        .as_milliseconds_since_epoch()
        .saturating_add(seconds.saturating_mul(1_000));
    Timestamp::from_milliseconds_since_epoch(millis).unwrap_or(now)
}

/// Whole days from `from` to `to`, by the civil calendar.
fn days_between(from: CalendarDate, to: CalendarDate) -> i32 {
    day_number(to).saturating_sub(day_number(from))
}

/// Days since 1970-01-01 for a civil date (the proleptic Gregorian calendar).
fn day_number(date: CalendarDate) -> i32 {
    let (year, month, day) = (
        i32::from(date.year()),
        i32::from(date.month()),
        i32::from(date.day()),
    );
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests;
