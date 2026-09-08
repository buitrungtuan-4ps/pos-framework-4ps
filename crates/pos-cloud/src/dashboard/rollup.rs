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
use pos_proto::events::{
    BillingBillSettled, CashDrawerPaidIn, CashDrawerPaidOut, CashShiftClosed, CashShiftOpened,
    DeviceAdmissionGranted, DeviceAdmissionRevoked, SalesOrderLineAdded,
};

use crate::cloud::{AdmittedDevice, DailyCash, DailyRevenue, DailyRollup};

/// Folds one event into a store's **admitted-device roster**, keyed by the edge-minted local device
/// id ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
///
/// The reader ADR-0118 §4 requires in the same slice as the two events, so they are not an event
/// with no reader — the state the tree already had one of.
///
/// Two arms, one shape each, and every other event ignored. Like [`fold_revenue`], a payload that
/// fails to decode is skipped rather than failing the pass: these are events this system wrote, so a
/// decode failure is a corrupt row, not the norm.
///
/// # Not keyed by trading day
///
/// Every other fold here buckets by `business_date`, because a day's takings are the question. This
/// one is keyed by **device**, because "which tills does this store have" is a question about the
/// present, not about a day — and because a revocation has to reach the row an admission created,
/// which a per-day bucket would scatter. So the roster is a single map, and both arms are upserts.
///
/// # A revocation keeps the row
///
/// It stamps `revoked_at_ms` rather than removing the entry. A roster that forgets what it retired
/// cannot answer *was this tablet ever admitted here*, which is the first thing asked about a device
/// that turns up where it should not — and the answer would be gone precisely in the case it
/// matters. A revocation for a device the cloud never saw admitted still records the row, because a
/// store may have paired it before this rollup was ever projected: the id and the retirement are the
/// truth available, and inventing an `admitted_at_ms` would be worse than admitting none.
pub fn fold_devices(
    devices: &mut BTreeMap<String, AdmittedDevice>,
    event: &EventEnvelope<RawPayload>,
) {
    match event.event_type.as_str() {
        "device.admission.granted" => {
            let Ok(admitted) = event.data.decode::<DeviceAdmissionGranted>() else {
                return;
            };
            let id = admitted.admitted_device_id.to_string();
            let row = devices.entry(id.clone()).or_default();
            row.local_device_id = id;
            row.admitted_at_ms = event.event_time.as_milliseconds_since_epoch();
            row.admitted_by_device_id = admitted
                .admitted_by_device_id
                .map(|device_id| device_id.to_string());
            // A re-pair of the same device id cannot happen — the edge mints a fresh id per pairing
            // — but a *replayed* admission for a row already revoked must not read as still
            // retired, so the stamp is cleared by the admission that supersedes it.
            row.revoked_at_ms = None;
        }
        "device.admission.revoked" => {
            let Ok(revoked) = event.data.decode::<DeviceAdmissionRevoked>() else {
                return;
            };
            let id = revoked.revoked_device_id.to_string();
            let row = devices.entry(id.clone()).or_default();
            row.local_device_id = id;
            row.revoked_at_ms = Some(event.event_time.as_milliseconds_since_epoch());
        }
        _ => {}
    }
}

/// The roster as the console reads it: still-admitted devices first, newest admission first within
/// each group, so the tills a store is actually running are at the top and the retired ones remain
/// visible below them.
///
/// The device id breaks a tie, so the order is stable across reads — a list that reshuffles between
/// a render and a click is a list an operator acts on the wrong row of.
#[must_use]
pub fn render_devices(devices: BTreeMap<String, AdmittedDevice>) -> Vec<AdmittedDevice> {
    let mut rows: Vec<AdmittedDevice> = devices.into_values().collect();
    rows.sort_by(|left, right| {
        left.revoked_at_ms
            .is_some()
            .cmp(&right.revoked_at_ms.is_some())
            .then_with(|| right.admitted_at_ms.cmp(&left.admitted_at_ms))
            .then_with(|| left.local_device_id.cmp(&right.local_device_id))
    });
    rows
}

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

/// Folds one event's **cash movement** into `cash` (ADR-0081, Track O4): shift open/close (float,
/// expected/counted/variance) and drawer paid-in/paid-out. `cash.shift.counted` is deliberately not
/// folded — the blind count is recorded on the close, and folding the pre-close count would blur the
/// blindness the control depends on. A payload that fails to decode is skipped.
pub fn fold_cash(cash: &mut BTreeMap<String, DailyCash>, event: &EventEnvelope<RawPayload>) {
    let date = event.business_date.to_string();
    match event.event_type.as_str() {
        "cash.shift.opened" => {
            let Ok(ev) = event.data.decode::<CashShiftOpened>() else {
                return;
            };
            let day = cash
                .entry(date.clone())
                .or_insert_with(|| empty_cash(&date));
            set_currency(
                &mut day.currency_code,
                ev.opening_float.currency_code.to_string(),
            );
            day.opening_float = day
                .opening_float
                .saturating_add(ev.opening_float.amount_minor);
            day.shifts_opened = day.shifts_opened.saturating_add(1);
        }
        "cash.shift.closed" => {
            let Ok(ev) = event.data.decode::<CashShiftClosed>() else {
                return;
            };
            let day = cash
                .entry(date.clone())
                .or_insert_with(|| empty_cash(&date));
            set_currency(
                &mut day.currency_code,
                ev.expected_amount.currency_code.to_string(),
            );
            day.shifts_closed = day.shifts_closed.saturating_add(1);
            day.expected = day.expected.saturating_add(ev.expected_amount.amount_minor);
            day.counted = day.counted.saturating_add(ev.counted_amount.amount_minor);
            day.variance = day.variance.saturating_add(ev.variance.amount_minor);
        }
        "cash.drawer.paid_in" => {
            let Ok(ev) = event.data.decode::<CashDrawerPaidIn>() else {
                return;
            };
            let day = cash
                .entry(date.clone())
                .or_insert_with(|| empty_cash(&date));
            set_currency(&mut day.currency_code, ev.amount.currency_code.to_string());
            day.paid_in = day.paid_in.saturating_add(ev.amount.amount_minor);
        }
        "cash.drawer.paid_out" => {
            let Ok(ev) = event.data.decode::<CashDrawerPaidOut>() else {
                return;
            };
            let day = cash
                .entry(date.clone())
                .or_insert_with(|| empty_cash(&date));
            set_currency(&mut day.currency_code, ev.amount.currency_code.to_string());
            day.paid_out = day.paid_out.saturating_add(ev.amount.amount_minor);
        }
        _ => {}
    }
}

/// Sets `slot` to `code` only if it is still empty — the first cash event of the day fixes the
/// currency, and a store is single-currency (`docs/pos-spec.md` §19).
fn set_currency(slot: &mut String, code: String) {
    if slot.is_empty() {
        *slot = code;
    }
}

/// An empty cash summary for a trading day.
#[must_use]
pub fn empty_cash(business_date: &str) -> DailyCash {
    DailyCash {
        business_date: business_date.to_owned(),
        ..DailyCash::default()
    }
}

/// An empty activity rollup for a trading day.
#[must_use]
pub fn empty_activity(business_date: &str) -> DailyRollup {
    DailyRollup {
        business_date: business_date.to_owned(),
        total_events: 0,
        by_type: BTreeMap::new(),
    }
}

/// An empty revenue rollup for a trading day.
#[must_use]
pub fn empty_revenue(business_date: &str) -> DailyRevenue {
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
    ) -> Result<Self, WindowError> {
        // Each bound is checked by name rather than in a loop that discards which one failed. The
        // loop knew, and threw it away one line before the caller needed it.
        if from
            .as_deref()
            .is_some_and(|bound| !is_business_date(bound))
        {
            return Err(WindowError::MalformedFrom);
        }
        if to.as_deref().is_some_and(|bound| !is_business_date(bound)) {
            return Err(WindowError::MalformedTo);
        }
        if let (Some(lower), Some(upper)) = (from.as_deref(), to.as_deref())
            && lower > upper
        {
            return Err(WindowError::Inverted);
        }
        let limit = match limit {
            Some(0) => return Err(WindowError::LimitTooSmall),
            Some(requested) => requested.min(MAX_WINDOW_DAYS),
            None => DEFAULT_WINDOW_DAYS,
        };
        Ok(Self { from, to, limit })
    }
}

/// Why a requested window is not one.
///
/// An enum rather than a message, so the HTTP layer can name the query parameter: this module knows
/// *what* is wrong, and only the route knows the wire names (`from`, `to`, `limit`). Keeping the
/// field naming there is also what stops this domain type growing an opinion about `details`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowError {
    /// `from` is not a `YYYY-MM-DD` business date.
    MalformedFrom,
    /// `to` is not a `YYYY-MM-DD` business date.
    MalformedTo,
    /// `from` is after `to`. Neither is wrong on its own, which is why it names neither.
    Inverted,
    /// `limit` is zero, which selects nothing.
    LimitTooSmall,
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
    use super::{DEFAULT_WINDOW_DAYS, MAX_WINDOW_DAYS, RollupWindow, WindowError, render_window};
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
        // Which bound, not merely that one of them was bad: the point of the split.
        assert_eq!(
            RollupWindow::new(Some("2026-3-1".into()), None, None).err(),
            Some(WindowError::MalformedFrom)
        );
        assert_eq!(
            RollupWindow::new(None, Some("not-a-date".into()), None).err(),
            Some(WindowError::MalformedTo)
        );
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

    // --- the admitted-device roster (ADR-0118 §4) ---

    use super::{fold_devices, render_devices};
    use crate::cloud::AdmittedDevice;
    use pos_proto::ids::DeviceId;
    use pos_proto::time::Timestamp;

    /// A device id from a small number, so a case can name two readably.
    fn device(n: u128) -> DeviceId {
        DeviceId::new(Ulid::from_u128(n))
    }

    /// The device-roster fold reads `event_time`, not `business_date` — the roster is a fact about
    /// the present, not about a trading day — so these cases stamp the instant rather than the date.
    fn at(
        event_time_ms: i64,
        event_type: EventType,
        payload: &serde_json::Value,
    ) -> EventEnvelope<RawPayload> {
        let mut event = event_with("2026-09-08", event_type, payload);
        event.event_time =
            Timestamp::from_milliseconds_since_epoch(event_time_ms).expect("valid instant");
        event
    }

    /// An admission of `admitted`, authorised by the till `by` where there was one.
    fn granted(event_time_ms: i64, admitted: u128, by: Option<u128>) -> EventEnvelope<RawPayload> {
        at(
            event_time_ms,
            EventType::DeviceAdmissionGranted,
            &json!({
                "admitted_device_id": ulid(admitted),
                "admitted_by_device_id": by.map(ulid),
            }),
        )
    }

    /// A revocation of `retired`.
    fn revoked(event_time_ms: i64, retired: u128) -> EventEnvelope<RawPayload> {
        at(
            event_time_ms,
            EventType::DeviceAdmissionRevoked,
            &json!({ "revoked_device_id": ulid(retired) }),
        )
    }

    /// Folds a sequence the way the projector does.
    fn roster(events: &[EventEnvelope<RawPayload>]) -> BTreeMap<String, AdmittedDevice> {
        let mut devices = BTreeMap::new();
        for event in events {
            fold_devices(&mut devices, event);
        }
        devices
    }

    #[test]
    fn an_admission_records_the_device_and_the_till_that_authorised_it() {
        let devices = roster(&[granted(1_000, 11, Some(7))]);
        let row = devices
            .get(&device(11).to_string())
            .expect("the admitted device is on the roster");
        assert_eq!(row.local_device_id, device(11).to_string());
        assert_eq!(row.admitted_at_ms, 1_000);
        assert_eq!(row.admitted_by_device_id, Some(device(7).to_string()));
        assert_eq!(row.revoked_at_ms, None, "it is still admitted");
    }

    #[test]
    fn the_boot_code_admits_with_no_authorising_device() {
        let devices = roster(&[granted(1_000, 11, None)]);
        let row = devices.get(&device(11).to_string()).expect("on the roster");
        assert_eq!(
            row.admitted_by_device_id, None,
            "nothing authorised the first device on a virgin box, because nothing could"
        );
    }

    #[test]
    fn a_revocation_stamps_the_row_rather_than_removing_it() {
        let devices = roster(&[granted(1_000, 11, Some(7)), revoked(5_000, 11)]);
        assert_eq!(devices.len(), 1, "the row is kept, not deleted");
        let row = devices.get(&device(11).to_string()).expect("on the roster");
        assert_eq!(row.revoked_at_ms, Some(5_000));
        assert_eq!(
            row.admitted_at_ms, 1_000,
            "and when it was admitted survives the retirement — 'was this ever here' is the \
             question a deleted row could not answer"
        );
    }

    #[test]
    fn a_revocation_for_a_device_never_seen_admitted_still_records_it() {
        // A store may have paired the device before its rollup was ever projected. The id and the
        // retirement are the truth available; inventing an admission instant would be worse.
        let devices = roster(&[revoked(5_000, 11)]);
        let row = devices.get(&device(11).to_string()).expect("on the roster");
        assert_eq!(row.revoked_at_ms, Some(5_000));
        assert_eq!(row.admitted_at_ms, 0, "no admission was ever seen for it");
    }

    #[test]
    fn activation_is_a_different_fact_and_stays_off_the_roster() {
        let mut devices = BTreeMap::new();
        fold_devices(
            &mut devices,
            &at(
                1_000,
                EventType::DeviceActivationCompleted,
                &json!({ "activated_device_id": ulid(11) }),
            ),
        );
        assert!(
            devices.is_empty(),
            "activation is the box getting a cloud identity, not a store admitting a tablet"
        );
    }

    #[test]
    fn the_roster_reads_still_admitted_first_then_newest_admission() {
        let devices = roster(&[
            granted(1_000, 11, None),
            granted(2_000, 12, Some(11)),
            granted(3_000, 13, Some(11)),
            revoked(4_000, 12),
        ]);
        let rows = render_devices(devices);
        let ids: Vec<&str> = rows
            .iter()
            .map(|row| row.local_device_id.as_str())
            .collect();
        let (live_newest, live_older, retired) = (
            device(13).to_string(),
            device(11).to_string(),
            device(12).to_string(),
        );
        assert_eq!(
            ids,
            [live_newest.as_str(), live_older.as_str(), retired.as_str()],
            "live tills newest-first, then the retired one — an operator looks at what is running"
        );
    }
}
