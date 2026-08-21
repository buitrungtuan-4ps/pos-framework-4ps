// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Human-readable reports over the capacity model and the fleet scenarios.
//!
//! Pure — every function returns a `String`, so the report content is unit-tested and the binary
//! ([`crate`]'s `main`) is the only thing that touches stdout. Each report is built as a list of lines
//! joined at the end, so there is no fallible `write!` to thread through a display path.

use crate::capacity::{SCENARIOS, reconcile_all};

/// The capacity envelope, per scenario: what the model derives against what the table publishes.
#[must_use]
pub fn capacity_report() -> String {
    let mut lines = vec![
        "Capacity envelope — executable model vs docs/capacity-and-reliability.md §2".to_owned(),
    ];
    for scenario in SCENARIOS {
        lines.push(String::new());
        lines.push(format!(
            "Scenario {}: {} stores x {} bills/day, {}% QR",
            scenario.name, scenario.stores, scenario.bills_per_store_day, scenario.qr_percent,
        ));
        lines.push(row(
            "events/day",
            scenario.events_per_day(),
            scenario.published_events_per_day,
        ));
        lines.push(row(
            "postgres MB/month",
            scenario.postgres_mb_per_month(),
            scenario.published_postgres_mb_per_month,
        ));
        lines.push(format!(
            "  {:<20} derived {:>10}  published {}..={} MB/day",
            "bandwidth MB/day",
            scenario.bandwidth_mb_per_day(),
            scenario.published_bandwidth_mb_per_day.0,
            scenario.published_bandwidth_mb_per_day.1,
        ));
        lines.push(format!(
            "  {:<20} ceiling {:>10}  published {:>10} events/s",
            "peak ingest",
            scenario.peak_ingest_ceiling_per_second(),
            scenario.published_peak_ingest_per_second,
        ));
    }
    lines.join("\n")
}

/// One `derived vs published` line.
fn row(label: &str, derived: i64, published: i64) -> String {
    format!("  {label:<20} derived {derived:>10}  published {published:>10}")
}

/// The standing reconciliation report — the places the model and the published table disagree.
#[must_use]
pub fn reconciliation_report() -> String {
    let findings = reconcile_all();
    if findings.is_empty() {
        return "Reconciliation: the model reproduces every published table — no discrepancies."
            .to_owned();
    }
    let mut lines = vec![format!(
        "Reconciliation: {} finding(s) to settle at the pilot",
        findings.len(),
    )];
    for finding in findings {
        lines.push(format!(
            "  [scenario {}] {}: derived {} vs published {} (outside {}% tolerance)",
            finding.scenario,
            finding.quantity,
            finding.derived,
            finding.published,
            finding.tolerance_percent,
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{capacity_report, reconciliation_report};

    #[test]
    fn the_capacity_report_names_every_scenario() {
        let report = capacity_report();
        for name in ["Scenario A", "Scenario B", "Scenario C"] {
            assert!(report.contains(name), "the report must cover {name}");
        }
        assert!(report.contains("events/day"));
    }

    #[test]
    fn the_reconciliation_report_surfaces_the_one_finding() {
        let report = reconciliation_report();
        assert!(report.contains("1 finding"));
        assert!(
            report.contains("qr_sessions_per_day"),
            "the standing discrepancy must be named in the report, not hidden"
        );
    }
}
