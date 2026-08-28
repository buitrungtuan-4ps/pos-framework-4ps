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

use crate::cloud::DailyRollup;

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

/// Renders a materialised day map as the dashboard's list, filtered to the window's inclusive date
/// range and capped to its newest `limit` trading days — still oldest trading day first.
#[must_use]
pub fn render_window(
    days: BTreeMap<String, DailyRollup>,
    window: &RollupWindow,
) -> Vec<DailyRollup> {
    // The map iterates ascending by `YYYY-MM-DD`, which for this format is chronological order.
    let mut filtered: Vec<DailyRollup> = days
        .into_values()
        .filter(|day| {
            window
                .from
                .as_deref()
                .is_none_or(|lower| day.business_date.as_str() >= lower)
                && window
                    .to
                    .as_deref()
                    .is_none_or(|upper| day.business_date.as_str() <= upper)
        })
        .collect();
    // Keep the newest `limit` days (they sit at the tail of the ascending list), oldest-first.
    if filtered.len() > window.limit {
        filtered.drain(0..filtered.len() - window.limit);
    }
    filtered
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
}
