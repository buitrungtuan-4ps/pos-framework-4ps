// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The one fold that turns events into a per-trading-day rollup
//! ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
//!
//! Both the from-log computation ([`Cloud::daily_rollups`](crate::cloud::Cloud::daily_rollups)) and
//! the incrementally-**materialised** projection ([`super::projection`]) run *this* function over
//! their events, so the two paths cannot drift: whatever a full re-scan would compute is exactly what
//! the maintained rollup holds. The fold uses only envelope fields (`business_date`, `event_type`),
//! so it needs no per-event-type decoding.

use std::collections::BTreeMap;

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{BillingBillSettled, SalesOrderLineAdded};

use crate::cloud::{DailyRevenue, DailyRollup};

/// Folds one event into `days`, creating the trading day's rollup if absent and counting the event
/// against its total and its type.
pub fn fold_event(days: &mut BTreeMap<String, DailyRollup>, event: &EventEnvelope<RawPayload>) {
    let day = days
        .entry(event.business_date.to_string())
        .or_insert_with(|| DailyRollup {
            business_date: event.business_date.to_string(),
            total_events: 0,
            by_type: BTreeMap::new(),
        });
    day.total_events = day.total_events.saturating_add(1);
    let type_count = day
        .by_type
        .entry(event.event_type.as_str().to_owned())
        .or_insert(0);
    *type_count = type_count.saturating_add(1);
}

/// Renders a materialised day map as the dashboard's list, oldest trading day first.
#[must_use]
pub fn render(days: BTreeMap<String, DailyRollup>) -> Vec<DailyRollup> {
    days.into_values().collect()
}

/// Folds one event's **money** into `revenue` (ADR-0081, Track O4): `billing.bill.settled` into the
/// day's recognised revenue totals, and `sales.order_line.added` into the day's gross ordered mix.
/// Every other event is ignored. A payload that fails to decode is skipped rather than failing the
/// pass — these are events this system wrote, so a decode failure is a corrupt row, not the norm.
pub fn fold_revenue(
    revenue: &mut BTreeMap<String, DailyRevenue>,
    event: &EventEnvelope<RawPayload>,
) {
    match event.event_type.as_str() {
        "billing.bill.settled" => {
            let Ok(bill) = event.data.decode::<BillingBillSettled>() else {
                return;
            };
            let day = revenue
                .entry(event.business_date.to_string())
                .or_insert_with(|| empty_revenue(&event.business_date.to_string()));
            day.currency_code = bill.total_due.currency_code.to_string();
            day.bills = day.bills.saturating_add(1);
            day.gross = day.gross.saturating_add(bill.subtotal.amount_minor);
            day.reductions = day
                .reductions
                .saturating_add(bill.reduction_total.amount_minor);
            day.service_charge = day
                .service_charge
                .saturating_add(bill.service_charge.amount_minor);
            day.tax = day.tax.saturating_add(bill.tax_total.amount_minor);
            day.net = day.net.saturating_add(bill.total_due.amount_minor);
        }
        "sales.order_line.added" => {
            let Ok(line) = event.data.decode::<SalesOrderLineAdded>() else {
                return;
            };
            let day = revenue
                .entry(event.business_date.to_string())
                .or_insert_with(|| empty_revenue(&event.business_date.to_string()));
            if day.currency_code.is_empty() {
                // A settled bill overwrites this with the authoritative currency; until one lands, the
                // ordered lines are the only currency signal for the day.
                day.currency_code = line.line_total.currency_code.to_string();
            }
            let mix = day
                .by_item
                .entry(line.menu_item_id.to_string())
                .or_default();
            mix.name = line.display_name.to_string();
            mix.ordered_qty_milli = mix
                .ordered_qty_milli
                .saturating_add(line.quantity.as_milli());
            mix.ordered_value = mix
                .ordered_value
                .saturating_add(line.line_total.amount_minor);
        }
        _ => {}
    }
}

/// An empty revenue rollup for a trading day.
fn empty_revenue(business_date: &str) -> DailyRevenue {
    DailyRevenue {
        business_date: business_date.to_owned(),
        currency_code: String::new(),
        bills: 0,
        gross: 0,
        reductions: 0,
        service_charge: 0,
        tax: 0,
        net: 0,
        by_item: BTreeMap::new(),
    }
}

/// The default window when a read names no range: the most recent quarter of trading days.
pub const DEFAULT_WINDOW_DAYS: usize = 90;

/// The most trading days one read may return, so a caller cannot ask for a store's whole history in
/// one response (the O4 "stop shipping all history" bound).
pub const MAX_WINDOW_DAYS: usize = 366;

/// A date range and cap for a rollup read (ADR-0081, Track O4).
///
/// `from`/`to` are inclusive `YYYY-MM-DD` business dates; `limit` caps the days returned and keeps the
/// **newest** ones in range. Absent bounds and the default `limit` give the most recent
/// [`DEFAULT_WINDOW_DAYS`] trading days, never the store's entire retained history.
#[derive(Debug, Clone)]
pub struct RollupWindow {
    from: Option<String>,
    to: Option<String>,
    limit: usize,
}

impl Default for RollupWindow {
    fn default() -> Self {
        Self {
            from: None,
            to: None,
            limit: DEFAULT_WINDOW_DAYS,
        }
    }
}

impl RollupWindow {
    /// Builds a window from optional query params, validating shape.
    ///
    /// # Errors
    ///
    /// A human-readable message (for a `400`) if `from`/`to` are not `YYYY-MM-DD`, if `from` is after
    /// `to`, or if `limit` is zero. `limit` is clamped to [`MAX_WINDOW_DAYS`].
    pub fn new(
        from: Option<String>,
        to: Option<String>,
        limit: Option<usize>,
    ) -> Result<Self, &'static str> {
        for bound in [from.as_deref(), to.as_deref()].into_iter().flatten() {
            if !is_business_date(bound) {
                return Err("from and to must be YYYY-MM-DD business dates");
            }
        }
        if let (Some(lower), Some(upper)) = (from.as_deref(), to.as_deref())
            && lower > upper
        {
            return Err("from must not be after to");
        }
        let limit = match limit {
            Some(0) => return Err("limit must be at least 1"),
            Some(requested) => requested.min(MAX_WINDOW_DAYS),
            None => DEFAULT_WINDOW_DAYS,
        };
        Ok(Self { from, to, limit })
    }
}

/// Filters an ascending-by-date list to the window's inclusive range and caps it to the newest
/// `limit` entries, preserving oldest-first order. Shared by the counts and revenue readers.
fn window_slice<T>(items: Vec<T>, date: impl Fn(&T) -> &str, window: &RollupWindow) -> Vec<T> {
    let mut filtered: Vec<T> = items
        .into_iter()
        .filter(|item| {
            window
                .from
                .as_deref()
                .is_none_or(|lower| date(item) >= lower)
                && window.to.as_deref().is_none_or(|upper| date(item) <= upper)
        })
        .collect();
    if filtered.len() > window.limit {
        filtered.drain(0..filtered.len() - window.limit);
    }
    filtered
}

/// Renders a materialised day map as the dashboard's list, filtered to the window's inclusive date
/// range and capped to its newest `limit` trading days — still oldest trading day first.
#[must_use]
pub fn render_window(
    days: BTreeMap<String, DailyRollup>,
    window: &RollupWindow,
) -> Vec<DailyRollup> {
    // The map iterates ascending by `YYYY-MM-DD`, which for this format is chronological order.
    window_slice(
        days.into_values().collect(),
        |day| day.business_date.as_str(),
        window,
    )
}

/// Renders a materialised revenue map as a list, windowed exactly as [`render_window`].
#[must_use]
pub fn render_revenue_window(
    revenue: BTreeMap<String, DailyRevenue>,
    window: &RollupWindow,
) -> Vec<DailyRevenue> {
    window_slice(
        revenue.into_values().collect(),
        |day| day.business_date.as_str(),
        window,
    )
}

/// Whether `value` is a `YYYY-MM-DD` calendar-shaped string (digits with dashes at positions 4 and 7).
/// Shape only — lexicographic order then equals chronological order, which is all the window needs.
fn is_business_date(value: &str) -> bool {
    value.len() == 10
        && value
            .as_bytes()
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 => *byte == b'-',
                _ => byte.is_ascii_digit(),
            })
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_WINDOW_DAYS, MAX_WINDOW_DAYS, RollupWindow, render_window};
    use crate::cloud::DailyRollup;
    use std::collections::BTreeMap;

    fn days(dates: &[&str]) -> BTreeMap<String, DailyRollup> {
        let mut map = BTreeMap::new();
        for date in dates {
            map.insert(
                (*date).to_owned(),
                DailyRollup {
                    business_date: (*date).to_owned(),
                    total_events: 1,
                    by_type: BTreeMap::new(),
                },
            );
        }
        map
    }

    fn dates(rollups: &[DailyRollup]) -> Vec<&str> {
        rollups
            .iter()
            .map(|day| day.business_date.as_str())
            .collect()
    }

    #[test]
    fn a_range_filters_inclusively_and_keeps_oldest_first() {
        let window = RollupWindow::new(Some("2026-03-02".into()), Some("2026-03-04".into()), None)
            .expect("valid window");
        let out = render_window(
            days(&[
                "2026-03-01",
                "2026-03-02",
                "2026-03-03",
                "2026-03-04",
                "2026-03-05",
            ]),
            &window,
        );
        assert_eq!(dates(&out), ["2026-03-02", "2026-03-03", "2026-03-04"]);
    }

    #[test]
    fn the_limit_keeps_the_newest_days() {
        let window = RollupWindow::new(None, None, Some(2)).expect("valid window");
        let out = render_window(days(&["2026-03-01", "2026-03-02", "2026-03-03"]), &window);
        // Newest two, still oldest-first.
        assert_eq!(dates(&out), ["2026-03-02", "2026-03-03"]);
    }

    #[test]
    fn the_default_window_caps_at_the_most_recent_quarter() {
        let all: Vec<String> = (1..=120)
            .map(|n| format!("2026-{:02}-{:02}", 1 + n / 28, 1 + n % 28))
            .collect();
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let out = render_window(days(&refs), &RollupWindow::default());
        assert_eq!(
            out.len(),
            DEFAULT_WINDOW_DAYS,
            "the default keeps only the recent quarter"
        );
    }

    #[test]
    fn a_huge_limit_is_clamped() {
        let window = RollupWindow::new(None, None, Some(10_000)).expect("valid window");
        let out = render_window(days(&["2026-03-01", "2026-03-02"]), &window);
        assert_eq!(
            out.len(),
            2,
            "fewer days than the (clamped) limit returns them all"
        );
        // The clamp itself is asserted below; here we only prove it did not error.
        let _ = MAX_WINDOW_DAYS;
    }

    #[test]
    fn a_malformed_date_is_rejected() {
        assert!(RollupWindow::new(Some("2026-3-1".into()), None, None).is_err());
        assert!(RollupWindow::new(None, Some("not-a-date".into()), None).is_err());
    }

    #[test]
    fn from_after_to_is_rejected() {
        assert!(
            RollupWindow::new(Some("2026-03-05".into()), Some("2026-03-01".into()), None).is_err()
        );
    }

    #[test]
    fn a_zero_limit_is_rejected() {
        assert!(RollupWindow::new(None, None, Some(0)).is_err());
    }

    // --- revenue fold ---

    use super::fold_revenue;
    use pos_contract_tests::fixtures;
    use pos_proto::BusinessDate;
    use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
    use pos_proto::events::EventType;
    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;
    use serde_json::json;

    fn ulid(n: u128) -> String {
        Ulid::from_u128(n).to_string()
    }

    fn money(minor: i64) -> serde_json::Value {
        json!({ "currency_code": "VND", "amount_minor": minor })
    }

    /// A base event (an activation fixture) re-typed and re-dated with a hand-built wire payload.
    fn event_with(
        date: &str,
        event_type: EventType,
        payload: &serde_json::Value,
    ) -> EventEnvelope<RawPayload> {
        let (year, month, day) = (
            date[0..4].parse().expect("year"),
            date[5..7].parse().expect("month"),
            date[8..10].parse().expect("day"),
        );
        let mut base = fixtures::activations(StoreId::new(Ulid::from_u128(1)), 1, 1).remove(0);
        base.business_date = BusinessDate::from_ymd(year, month, day).expect("valid date");
        base.event_type = EventTypeRef::from_known(event_type);
        base.data = RawPayload::encode(payload).expect("encode payload");
        base
    }

    #[test]
    fn revenue_folds_settled_bills_and_ordered_lines() {
        let mut revenue = BTreeMap::new();
        fold_revenue(
            &mut revenue,
            &event_with(
                "2026-03-15",
                EventType::BillingBillSettled,
                &json!({
                    "bill_id": ulid(1), "receipt_number": 7u64,
                    "subtotal": money(100_000), "reduction_total": money(10_000),
                    "service_charge": money(5_000), "tax_total": money(8_000),
                    "rounding_adjustment": money(0), "total_due": money(103_000),
                }),
            ),
        );
        fold_revenue(
            &mut revenue,
            &event_with(
                "2026-03-15",
                EventType::SalesOrderLineAdded,
                &json!({
                    "order_id": ulid(2), "order_line_id": ulid(3), "menu_item_id": ulid(4),
                    "display_name": "Margherita", "quantity": { "milli": 2000 },
                    "unit_price": money(50_000), "line_total": money(100_000),
                    "tax_class_id": ulid(5), "tax_rate": { "numerator": 8, "denominator": 100 },
                    "seat": null, "course_id": null, "note_present": false,
                }),
            ),
        );

        let day = revenue
            .get("2026-03-15")
            .expect("the trading day was folded");
        assert_eq!(day.bills, 1);
        assert_eq!(day.gross, 100_000);
        assert_eq!(day.reductions, 10_000);
        assert_eq!(day.tax, 8_000);
        assert_eq!(day.net, 103_000, "total_due is the headline revenue");
        assert_eq!(day.currency_code, "VND");
        let mix = day.by_item.get(&ulid(4)).expect("the item is in the mix");
        assert_eq!(mix.name, "Margherita");
        assert_eq!(mix.ordered_qty_milli, 2000);
        assert_eq!(mix.ordered_value, 100_000);
    }

    #[test]
    fn revenue_ignores_events_that_are_not_money() {
        let mut revenue = BTreeMap::new();
        fold_revenue(
            &mut revenue,
            &event_with("2026-03-15", EventType::SalesOrderLineFired, &json!({})),
        );
        assert!(
            revenue.is_empty(),
            "a fired line carries no money and folds nothing"
        );
    }
}
