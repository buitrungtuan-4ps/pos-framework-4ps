// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The e-invoice overdue alert
//! ([ADR-0241](../../../../docs/adr/0241-the-cloud-submits-each-issued-invoice-from-the-log-and-keeps-its-state.md)
//! decision 4).
//!
//! One alert per store while any of its invoices is not with the provider and its deadline is
//! within the warning window or past. The deadline is the end of the day after the invoice's date
//! (UTC) and the window is six hours: both the recommended reading, **to be confirmed by legal or
//! the accountant**. A rejected invoice counts, because it too is not with the provider.

use std::collections::BTreeMap;

use pos_proto::ids::TenantId;
use serde_json::json;
use store_postgres::SubmissionRow;

use super::model::{AlertKind, FiringAlert};
use crate::einvoice::feed::{STATE_SUBMITTED, SubmissionStore};

/// How long before its deadline an unsubmitted invoice alerts, in milliseconds.
pub const WARNING_WINDOW_MS: i64 = 6 * 60 * 60 * 1_000;

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// The deadline of an invoice dated `issued_on` (`YYYY-MM-DD`): the end of the next day, UTC.
#[must_use]
pub fn deadline_ms(issued_on: &str) -> Option<i64> {
    let mut parts = issued_on.split('-').map(str::parse::<i64>);
    let (year, month, day) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    days.checked_add(2)?.checked_mul(DAY_MS)
}

/// The firing alerts for one tenant's unsubmitted invoices at `now_ms`. Pure.
#[must_use]
pub fn overdue_alerts(tenant: TenantId, rows: &[SubmissionRow], now_ms: i64) -> Vec<FiringAlert> {
    let mut stores: BTreeMap<&str, (u64, u64, i64)> = BTreeMap::new();
    for row in rows.iter().filter(|row| row.state != STATE_SUBMITTED) {
        let Some(deadline) = deadline_ms(&row.issued_on) else {
            continue;
        };
        if now_ms < deadline.saturating_sub(WARNING_WINDOW_MS) {
            continue;
        }
        let entry = stores
            .entry(row.store_id.as_str())
            .or_insert((0, 0, deadline));
        entry.0 += 1;
        if now_ms >= deadline {
            entry.1 += 1;
        }
        entry.2 = entry.2.min(deadline);
    }
    stores
        .into_iter()
        .map(|(store, (near, past, earliest))| {
            FiringAlert::new(
                AlertKind::EInvoiceOverdue,
                Some(tenant),
                store,
                format!("{near} e-invoice(s) not with the provider near or past their deadline"),
                json!({ "store_id": store, "invoices": near, "past_deadline": past, "earliest_deadline_ms": earliest }),
            )
        })
        .collect()
}

/// The overdue alerts for `tenants`, read from `submissions`. A tenant whose submissions cannot be
/// read is logged and skipped.
pub async fn einvoice_alerts(
    submissions: Option<&dyn SubmissionStore>,
    tenants: &[TenantId],
    now_ms: i64,
) -> Vec<FiringAlert> {
    let Some(submissions) = submissions else {
        return Vec::new();
    };
    let mut firing = Vec::new();
    for &tenant in tenants {
        match submissions.list(tenant, None, true).await {
            Ok(rows) => firing.extend(overdue_alerts(tenant, &rows, now_ms)),
            Err(error) => {
                tracing::warn!(%tenant, %error, "could not read the e-invoice submissions; their alert is not evaluated this pass");
            }
        }
    }
    firing
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::TenantId;
    use pos_proto::ulid::Ulid;
    use store_postgres::SubmissionRow;

    use super::{WARNING_WINDOW_MS, deadline_ms, overdue_alerts};

    fn row(store: &str, issued_on: &str, state: &str) -> SubmissionRow {
        SubmissionRow {
            tenant_id: "t".to_owned(),
            invoice_id: format!("{store}{issued_on}{state}"),
            store_id: store.to_owned(),
            bill_id: "b".to_owned(),
            invoice_number: "1C26MAA/00000001".to_owned(),
            kind: "ORIGINAL".to_owned(),
            issued_on: issued_on.to_owned(),
            total_minor: 100,
            currency: "VND".to_owned(),
            buyer_subject_id: None,
            connection_id: None,
            corrects_invoice_number: None,
            state: state.to_owned(),
            failed_attempts: 0,
            next_attempt_ms: 0,
            authority_reference: None,
            refusal: None,
            recorded_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn the_deadline_is_the_end_of_the_next_day() {
        // 2026-11-25T00:00Z is 1_795_564_800_000; the deadline is 2026-11-27T00:00Z.
        assert_eq!(
            deadline_ms("2026-11-25"),
            Some(1_795_564_800_000 + 2 * 86_400_000)
        );
        assert_eq!(deadline_ms("not a date"), None);
    }

    #[test]
    fn a_store_alerts_inside_the_window_and_a_submitted_invoice_never_does() {
        let tenant = TenantId::new(Ulid::from_u128(1));
        let deadline = deadline_ms("2026-11-25").expect("a date");
        let rows = [
            row("s1", "2026-11-25", "PENDING"),
            row("s1", "2026-11-25", "REJECTED"),
            row("s2", "2026-11-25", "SUBMITTED"),
        ];
        assert!(overdue_alerts(tenant, &rows, deadline - WARNING_WINDOW_MS - 1).is_empty());
        let near = overdue_alerts(tenant, &rows, deadline - 1);
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].dedup_key, "s1");
        assert_eq!(near[0].detail["invoices"], 2);
        assert_eq!(near[0].detail["past_deadline"], 0);
        let past = overdue_alerts(tenant, &rows, deadline);
        assert_eq!(past[0].detail["past_deadline"], 2);
    }
}
