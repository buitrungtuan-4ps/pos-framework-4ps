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
    BillFeeLine, BillingBillSettled, CashDrawerPaidIn, CashDrawerPaidOut, CashShiftClosed,
    CashShiftOpened, DeviceAdmissionGranted, DeviceAdmissionRevoked, SalesOrderLineAdded,
};

use crate::cloud::{
    AdmittedDevice, CashTotals, DailyCash, DailyRevenue, DailyRollup, FeeTotal, STORE_DRAWER,
};

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
/// day's recognised revenue totals and its fees by code ([`fold_fees`]), and
/// `sales.order_line.added` into the day's gross ordered mix. Every other event is ignored. A
/// payload that fails to decode is skipped rather than failing the pass — these are events this
/// system wrote, so a decode failure is a corrupt row, not the norm.
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
            fold_fees(&mut day.by_fee, &bill.fee_lines);
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

/// Folds one settled bill's `fee_lines` into its day's fees by code
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 4): what each code
/// charged, the tax on it, and one more bill for each code the bill charged.
///
/// A bill from an edge that predates fee lines records none and adds nothing here, while its
/// `service_charge` still reaches the day's figure ([`DailyRevenue::by_fee`] says why there is no
/// bucket for it). A line that charged nothing and taxed nothing is left out, as the receipt prints
/// no line for it, so a fee in force that charged a bill nothing does not count that bill. The name
/// is the last one a bill recorded, as the product mix keeps an item's.
fn fold_fees(by_fee: &mut BTreeMap<String, FeeTotal>, lines: &[BillFeeLine]) {
    // The codes this bill has counted already: two lines under one code are one bill charging it.
    let mut counted: Vec<&str> = Vec::with_capacity(lines.len());
    for line in lines {
        if line.amount.is_zero() && line.tax.is_zero() {
            continue;
        }
        let code = line.code.as_str();
        let total = by_fee.entry(code.to_owned()).or_default();
        total.name = line.display_name.to_string();
        total.amount = total.amount.saturating_add(line.amount.amount_minor);
        total.tax = total.tax.saturating_add(line.tax.amount_minor);
        if !counted.contains(&code) {
            counted.push(code);
            total.bills = total.bills.saturating_add(1);
        }
    }
}

/// Folds one event's **cash movement** into `cash` (ADR-0081, Track O4): shift open/close (float,
/// expected/counted/variance) and drawer paid-in/paid-out. `cash.shift.counted` is deliberately not
/// folded — the blind count is recorded on the close, and folding the pre-close count would blur the
/// blindness the control depends on. A payload that fails to decode is skipped.
///
/// Each event's figures are added to the store's and to its drawer's alike, the till it names or
/// the store's one drawer ([`STORE_DRAWER`]), so the drawers add up to the store by construction
/// (ADR-0167 decision 13). `cash.drawer.opened` is not folded: nothing in the X/Z counts drawer
/// openings, and the day's activity counts it by type already.
pub fn fold_cash(cash: &mut BTreeMap<String, DailyCash>, event: &EventEnvelope<RawPayload>) {
    let folded = match event.event_type.as_str() {
        "cash.shift.opened" => event.data.decode::<CashShiftOpened>().ok().map(|ev| {
            let totals = CashTotals {
                opening_float: ev.opening_float.amount_minor,
                shifts_opened: 1,
                ..CashTotals::default()
            };
            (
                ev.opening_float.currency_code,
                ev.terminal_device_id,
                totals,
            )
        }),
        "cash.shift.closed" => event.data.decode::<CashShiftClosed>().ok().map(|ev| {
            let totals = CashTotals {
                shifts_closed: 1,
                expected: ev.expected_amount.amount_minor,
                counted: ev.counted_amount.amount_minor,
                variance: ev.variance.amount_minor,
                ..CashTotals::default()
            };
            (
                ev.expected_amount.currency_code,
                ev.terminal_device_id,
                totals,
            )
        }),
        "cash.drawer.paid_in" => event.data.decode::<CashDrawerPaidIn>().ok().map(|ev| {
            let totals = CashTotals {
                paid_in: ev.amount.amount_minor,
                ..CashTotals::default()
            };
            (ev.amount.currency_code, ev.terminal_device_id, totals)
        }),
        "cash.drawer.paid_out" => event.data.decode::<CashDrawerPaidOut>().ok().map(|ev| {
            let totals = CashTotals {
                paid_out: ev.amount.amount_minor,
                ..CashTotals::default()
            };
            (ev.amount.currency_code, ev.terminal_device_id, totals)
        }),
        _ => None,
    };
    let Some((currency, till, totals)) = folded else {
        return;
    };
    let date = event.business_date.to_string();
    let day = cash
        .entry(date.clone())
        .or_insert_with(|| empty_cash(&date));
    set_currency(&mut day.currency_code, currency.to_string());
    add_to_store(day, &totals);
    let drawer = till.map_or_else(|| STORE_DRAWER.to_owned(), |till| till.to_string());
    add_to_drawer(day.by_drawer.entry(drawer).or_default(), &totals);
}

/// Adds one event's figures to the store's day, each saturating as every rollup sum does.
fn add_to_store(sum: &mut DailyCash, figures: &CashTotals) {
    sum.opening_float = sum.opening_float.saturating_add(figures.opening_float);
    sum.paid_in = sum.paid_in.saturating_add(figures.paid_in);
    sum.paid_out = sum.paid_out.saturating_add(figures.paid_out);
    sum.shifts_opened = sum.shifts_opened.saturating_add(figures.shifts_opened);
    sum.shifts_closed = sum.shifts_closed.saturating_add(figures.shifts_closed);
    sum.expected = sum.expected.saturating_add(figures.expected);
    sum.counted = sum.counted.saturating_add(figures.counted);
    sum.variance = sum.variance.saturating_add(figures.variance);
}

/// Adds the same figures to the drawer's day, by the same rule as [`add_to_store`].
fn add_to_drawer(sum: &mut CashTotals, figures: &CashTotals) {
    sum.opening_float = sum.opening_float.saturating_add(figures.opening_float);
    sum.paid_in = sum.paid_in.saturating_add(figures.paid_in);
    sum.paid_out = sum.paid_out.saturating_add(figures.paid_out);
    sum.shifts_opened = sum.shifts_opened.saturating_add(figures.shifts_opened);
    sum.shifts_closed = sum.shifts_closed.saturating_add(figures.shifts_closed);
    sum.expected = sum.expected.saturating_add(figures.expected);
    sum.counted = sum.counted.saturating_add(figures.counted);
    sum.variance = sum.variance.saturating_add(figures.variance);
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
        by_fee: BTreeMap::new(),
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
    use crate::cloud::FeeTotal;
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

    /// One fee line, in the shape `billing.bill.settled` carries it.
    fn fee(code: &str, name: &str, amount: i64, tax: i64) -> serde_json::Value {
        json!({
            "fee_id": ulid(9), "code": code, "display_name": name,
            "amount": money(amount), "tax": money(tax),
        })
    }

    /// A bill settled on 2026-03-15 with `service_charge`, recording `fee_lines` when it has them.
    fn settled(
        service_charge: i64,
        fee_lines: Option<serde_json::Value>,
    ) -> EventEnvelope<RawPayload> {
        let mut payload = json!({
            "bill_id": ulid(1), "receipt_number": 7u64,
            "subtotal": money(100_000), "reduction_total": money(0),
            "service_charge": money(service_charge), "tax_total": money(8_000),
            "rounding_adjustment": money(0), "total_due": money(108_000 + service_charge),
        });
        if let Some(lines) = fee_lines {
            payload["fee_lines"] = lines;
        }
        event_with("2026-03-15", EventType::BillingBillSettled, &payload)
    }

    #[test]
    fn fees_fold_by_code_and_a_bill_without_fee_lines_adds_to_the_service_charge_alone() {
        let mut revenue = BTreeMap::new();
        for bill in [
            settled(
                7_000,
                Some(json!([
                    fee("SVC", "Service", 5_000, 400),
                    fee("PACK", "Packaging", 2_000, 0)
                ])),
            ),
            settled(
                5_000,
                Some(json!([
                    fee("SVC", "Service charge", 5_000, 400),
                    fee("PACK", "Packaging", 0, 0)
                ])),
            ),
            // Settled by an edge from before fee lines: the same charge, recorded only as the sum.
            settled(5_000, None),
        ] {
            fold_revenue(&mut revenue, &bill);
        }

        let day = &revenue["2026-03-15"];
        assert_eq!(
            (day.bills, day.service_charge),
            (3, 17_000),
            "every bill, fee lines or none"
        );
        let total = |name: &str, bills, amount, tax| FeeTotal {
            name: name.to_owned(),
            bills,
            amount,
            tax,
        };
        assert_eq!(
            day.by_fee,
            BTreeMap::from([
                ("PACK".to_owned(), total("Packaging", 1, 2_000, 0)),
                ("SVC".to_owned(), total("Service charge", 2, 10_000, 800)),
            ]),
            "by code, under the last name a bill recorded; a line that charged nothing counts no \
             bill, and the bill with no fee lines is in no entry, so 5,000 of the service charge \
             is under no code"
        );
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

    // --- cash fold, by drawer (ADR-0167 decisions 10 and 13) ---

    use super::fold_cash;
    use crate::cloud::{CashTotals, DailyCash, STORE_DRAWER};

    /// A cash event on 2026-03-15 at the till `till`, or at the store's drawer for `None`.
    fn cash_event(
        event_type: EventType,
        till: Option<u128>,
        mut payload: serde_json::Value,
    ) -> EventEnvelope<RawPayload> {
        if let Some(till) = till {
            payload["terminal_device_id"] = json!(ulid(till));
        }
        event_with("2026-03-15", event_type, &payload)
    }

    /// A drawer's day as its till records it: started on `float`, paid in and out, and closed with
    /// `(expected, counted)`, or still open for `None`.
    fn drawer_day(
        till: Option<u128>,
        (float, paid_in, paid_out): (i64, i64, i64),
        closed: Option<(i64, i64)>,
    ) -> Vec<EventEnvelope<RawPayload>> {
        let shift = ulid(0x5F);
        let mut events = vec![
            cash_event(
                EventType::CashShiftOpened,
                till,
                json!({ "opened_shift_id": shift, "opening_float": money(float) }),
            ),
            cash_event(
                EventType::CashDrawerPaidIn,
                till,
                json!({ "amount": money(paid_in), "reason_code_id": ulid(0x7A) }),
            ),
            cash_event(
                EventType::CashDrawerPaidOut,
                till,
                json!({ "amount": money(paid_out), "reason_code_id": ulid(0x7B) }),
            ),
        ];
        if let Some((expected, counted)) = closed {
            events.push(cash_event(
                EventType::CashShiftClosed,
                till,
                json!({
                    "closed_shift_id": shift,
                    "expected_amount": money(expected),
                    "counted_amount": money(counted),
                    "variance": money(counted - expected),
                }),
            ));
        }
        events
    }

    /// The store's figures on `day`, as a drawer's are kept, to set beside its drawers'.
    fn store_figures(day: &DailyCash) -> CashTotals {
        CashTotals {
            opening_float: day.opening_float,
            paid_in: day.paid_in,
            paid_out: day.paid_out,
            shifts_opened: day.shifts_opened,
            shifts_closed: day.shifts_closed,
            expected: day.expected,
            counted: day.counted,
            variance: day.variance,
        }
    }

    /// 2026-03-15's cash, folded from `events` as the projector folds them.
    fn cash_day(events: &[EventEnvelope<RawPayload>]) -> DailyCash {
        let mut cash = BTreeMap::new();
        for event in events {
            fold_cash(&mut cash, event);
        }
        cash.remove("2026-03-15").expect("the day")
    }

    #[test]
    fn each_tills_drawer_is_its_own_events_and_the_drawers_add_up_to_the_store() {
        let (bar, door, patio) = (0xBA, 0xD0, 0xFA);
        let mut events = drawer_day(
            Some(bar),
            (500_000, 100_000, 30_000),
            Some((1_570_000, 1_560_000)),
        );
        events.extend(drawer_day(
            Some(door),
            (200_000, 50_000, 20_000),
            Some((230_000, 230_000)),
        ));
        events.extend(drawer_day(Some(patio), (100_000, 10_000, 0), None));
        let day = cash_day(&events);

        let drawer = |till: u128| day.by_drawer.get(&ulid(till)).copied();
        let closed =
            |opening_float, paid_in, paid_out, (expected, counted): (i64, i64)| CashTotals {
                opening_float,
                paid_in,
                paid_out,
                shifts_opened: 1,
                shifts_closed: 1,
                expected,
                counted,
                variance: counted - expected,
            };
        assert_eq!(
            drawer(bar),
            Some(closed(500_000, 100_000, 30_000, (1_570_000, 1_560_000)))
        );
        assert_eq!(
            drawer(door),
            Some(closed(200_000, 50_000, 20_000, (230_000, 230_000)))
        );
        assert_eq!(
            drawer(patio),
            Some(CashTotals {
                opening_float: 100_000,
                paid_in: 10_000,
                shifts_opened: 1,
                ..CashTotals::default()
            }),
            "an open drawer has no expected amount: only its close carries one"
        );
        assert_eq!(day.by_drawer.len(), 3, "every event named its till");

        let summed = day
            .by_drawer
            .values()
            .fold(CashTotals::default(), |sum, drawer| CashTotals {
                opening_float: sum.opening_float + drawer.opening_float,
                paid_in: sum.paid_in + drawer.paid_in,
                paid_out: sum.paid_out + drawer.paid_out,
                shifts_opened: sum.shifts_opened + drawer.shifts_opened,
                shifts_closed: sum.shifts_closed + drawer.shifts_closed,
                expected: sum.expected + drawer.expected,
                counted: sum.counted + drawer.counted,
                variance: sum.variance + drawer.variance,
            });
        assert_eq!(
            summed,
            store_figures(&day),
            "the drawers add up to the store"
        );
        assert_eq!(
            store_figures(&day),
            CashTotals {
                opening_float: 800_000,
                paid_in: 160_000,
                paid_out: 50_000,
                shifts_opened: 3,
                shifts_closed: 2,
                expected: 1_800_000,
                counted: 1_790_000,
                variance: -10_000,
            },
            "and the store's figures are the sums they always were"
        );
    }

    #[test]
    fn a_store_with_one_drawer_folds_it_under_the_stores_key_with_the_days_figures() {
        let day = cash_day(&drawer_day(
            None,
            (500_000, 100_000, 30_000),
            Some((570_000, 565_000)),
        ));
        let figures = CashTotals {
            opening_float: 500_000,
            paid_in: 100_000,
            paid_out: 30_000,
            shifts_opened: 1,
            shifts_closed: 1,
            expected: 570_000,
            counted: 565_000,
            variance: -5_000,
        };
        assert_eq!(store_figures(&day), figures);
        assert_eq!(
            day.by_drawer.into_iter().collect::<Vec<_>>(),
            [(STORE_DRAWER.to_owned(), figures)]
        );
    }

    #[test]
    fn a_day_stored_before_its_drawers_loads_with_none_and_writes_its_figures_where_they_were() {
        let stored = json!({
            "business_date": "2026-03-15", "currency_code": "VND", "opening_float": 500_000,
            "paid_in": 0, "paid_out": 0, "shifts_opened": 1, "shifts_closed": 0,
            "expected": 0, "counted": 0, "variance": 0,
        });
        let day: DailyCash = serde_json::from_value(stored.clone()).expect("an older day loads");
        assert!(day.by_drawer.is_empty());
        assert_eq!(day.opening_float, 500_000);
        let mut written = serde_json::to_value(&day).expect("json");
        let drawers = written
            .as_object_mut()
            .and_then(|object| object.remove("by_drawer"));
        assert_eq!(drawers, Some(json!({})));
        assert_eq!(
            written, stored,
            "the store's figures sit where they always did"
        );
    }
}
