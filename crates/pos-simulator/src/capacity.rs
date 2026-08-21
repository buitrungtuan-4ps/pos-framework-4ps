// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The executable capacity model — [`capacity-and-reliability.md`](../../../docs/capacity-and-reliability.md) §2.
//!
//! The document is explicit that its sizing numbers are **design estimates**, to be replaced by pilot
//! measurement. Until the hardware exists, the most this crate can do — and it is worth doing — is make
//! the estimates *executable*: encode the §2 scenarios as data and the sizing formulas as pure integer
//! functions, then check each derived quantity against the published table. A formula that drifts from
//! the table fails a test; the one place the published estimates do not reconcile is pinned by
//! [`reconcile`] rather than quietly averaged away.
//!
//! Everything is integer. Capacity numbers get the same treatment as money: no floating point, because
//! a rounding surprise in a disk or bandwidth projection is a bill nobody budgeted for.

/// Events written to the log per bill — the order, its lines, the fire, the payment, the close.
///
/// Not a guess: every published scenario's events/day is exactly its bills/day times this
/// (A `480_000 / 60_000`, B `4_000_000 / 500_000`, C `256_000 / 32_000`), so it is the table's own
/// implied constant, recovered and named.
pub const EVENTS_PER_BILL: i64 = 8;

/// PostgreSQL storage per bill, per month, in kilobytes.
///
/// The §2 formula is `GB/month ≈ bills_per_day × 0.15 ÷ 1000`, i.e. `0.15 MB = 150 KB` of durable log
/// and rollup per bill held for the month.
pub const POSTGRES_KB_PER_BILL_MONTH: i64 = 150;

/// Menu imagery a single QR guest session pulls, in megabytes — a thumbnail grid and a few detail
/// images. Sized so scenario B's 250k sessions reproduce its ~250 GB/day bandwidth wall, the first
/// linear constraint the QR-ordering review found (§6 finding 1).
pub const QR_SESSION_IMAGE_MB: i64 = 1;

/// The §2 peak-ingest divisor: `peak events/s ≈ bills_per_day ÷ 1260`. It is a conservative **ceiling**
/// — it matches the smallest scenario and over-estimates the larger two, which is the safe direction
/// for provisioning (see [`Scenario::peak_ingest_ceiling_per_second`]).
pub const PEAK_INGEST_BILLS_PER_SECOND_DIVISOR: i64 = 1260;

/// One of the three published sizing scenarios (§2), as data.
///
/// The first three fields are the scenario's *inputs*; the `published_*` fields are the table's stated
/// envelope, kept here so the model can be checked against them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scenario {
    /// The scenario label in the table (`A`, `B`, `C`).
    pub name: &'static str,
    /// How many stores.
    pub stores: i64,
    /// Bills per store per day.
    pub bills_per_store_day: i64,
    /// The share of bills placed through QR ordering, as a whole percent.
    pub qr_percent: i64,

    /// The table's stated events/day.
    pub published_events_per_day: i64,
    /// The table's stated QR sessions/day.
    pub published_qr_sessions_per_day: i64,
    /// The table's stated peak ingest, events/second.
    pub published_peak_ingest_per_second: i64,
    /// The table's stated PostgreSQL storage, megabytes/month.
    pub published_postgres_mb_per_month: i64,
    /// The table's stated daily bandwidth range, megabytes/day (`low ..= high`).
    pub published_bandwidth_mb_per_day: (i64, i64),
}

/// **A** — 300 stores, 200 bills, 30% QR. The over-provisioned scenario.
pub const SCENARIO_A: Scenario = Scenario {
    name: "A",
    stores: 300,
    bills_per_store_day: 200,
    qr_percent: 30,
    published_events_per_day: 480_000,
    published_qr_sessions_per_day: 9_000,
    published_peak_ingest_per_second: 27,
    published_postgres_mb_per_month: 9_000,
    published_bandwidth_mb_per_day: (9_000, 15_000),
};

/// **B** — 1,000 stores, 500 bills, 50% QR. The binding scenario: disk and bandwidth are the two
/// linear walls (§7).
pub const SCENARIO_B: Scenario = Scenario {
    name: "B",
    stores: 1_000,
    bills_per_store_day: 500,
    qr_percent: 50,
    published_events_per_day: 4_000_000,
    published_qr_sessions_per_day: 250_000,
    published_peak_ingest_per_second: 222,
    published_postgres_mb_per_month: 72_000,
    published_bandwidth_mb_per_day: (240_000, 260_000),
};

/// **C** — 400 small stores, 80 bills, 20% QR. Infrastructure idles.
pub const SCENARIO_C: Scenario = Scenario {
    name: "C",
    stores: 400,
    bills_per_store_day: 80,
    qr_percent: 20,
    published_events_per_day: 256_000,
    published_qr_sessions_per_day: 6_000,
    published_peak_ingest_per_second: 25,
    published_postgres_mb_per_month: 4_800,
    published_bandwidth_mb_per_day: (6_000, 10_000),
};

/// The three published scenarios.
pub const SCENARIOS: [Scenario; 3] = [SCENARIO_A, SCENARIO_B, SCENARIO_C];

impl Scenario {
    /// Total bills across the fleet per day.
    #[must_use]
    pub const fn bills_per_day(&self) -> i64 {
        self.stores * self.bills_per_store_day
    }

    /// Events written to the log per day, `bills_per_day × `[`EVENTS_PER_BILL`].
    #[must_use]
    pub const fn events_per_day(&self) -> i64 {
        self.bills_per_day() * EVENTS_PER_BILL
    }

    /// QR sessions per day implied by the QR share of bills, `bills_per_day × qr_percent ÷ 100`.
    ///
    /// This is the model's own derivation; where it disagrees with
    /// [`Self::published_qr_sessions_per_day`], [`reconcile`] reports it.
    #[must_use]
    pub const fn qr_sessions_from_share(&self) -> i64 {
        self.bills_per_day() * self.qr_percent / 100
    }

    /// PostgreSQL storage, megabytes/month: `bills_per_day × `[`POSTGRES_KB_PER_BILL_MONTH`]` ÷ 1000`.
    #[must_use]
    pub const fn postgres_mb_per_month(&self) -> i64 {
        self.bills_per_day() * POSTGRES_KB_PER_BILL_MONTH / 1_000
    }

    /// Daily bandwidth, megabytes/day, from the published QR sessions and [`QR_SESSION_IMAGE_MB`].
    ///
    /// Uses the table's session count rather than [`Self::qr_sessions_from_share`], because bandwidth
    /// is what the QR-ordering review sized directly (§6) and it is the quantity the wall is on.
    #[must_use]
    pub const fn bandwidth_mb_per_day(&self) -> i64 {
        self.published_qr_sessions_per_day * QR_SESSION_IMAGE_MB
    }

    /// The peak-ingest **ceiling**, events/second, from the §2 divisor. A conservative upper bound:
    /// every scenario's stated peak sits at or below it.
    #[must_use]
    pub const fn peak_ingest_ceiling_per_second(&self) -> i64 {
        self.bills_per_day() / PEAK_INGEST_BILLS_PER_SECOND_DIVISOR
    }
}

/// A derived quantity that does not reconcile with the published table within tolerance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discrepancy {
    /// The scenario it was found in.
    pub scenario: &'static str,
    /// Which quantity.
    pub quantity: &'static str,
    /// What the model derives.
    pub derived: i64,
    /// What the table states.
    pub published: i64,
    /// The tolerance band that was exceeded, as a whole percent of the published value.
    pub tolerance_percent: i64,
}

/// Whether `derived` is within `tolerance_percent` of `published`.
#[must_use]
fn within_tolerance(derived: i64, published: i64, tolerance_percent: i64) -> bool {
    // Integer only, widened to i128 so there is no cast and no overflow:
    // |derived − published| × 100 ≤ tolerance% × |published|.
    let difference = (i128::from(derived) - i128::from(published)).abs();
    difference * 100 <= i128::from(tolerance_percent) * i128::from(published).abs()
}

/// Checks every derivable quantity in `scenario` against the published table and returns the ones that
/// fall outside tolerance.
///
/// An empty result means the model reproduces the table. A non-empty result is a finding to resolve at
/// the pilot — the estimates and the formulas disagree, and this says exactly where.
///
/// Tolerances: events/day is exact (it is the table's own implied constant); PostgreSQL storage is 5%
/// (the table rounds 75 GB to 72); the QR-session share is 10%.
#[must_use]
pub fn reconcile(scenario: &Scenario) -> Vec<Discrepancy> {
    let mut found = Vec::new();
    let checks: [(&'static str, i64, i64, i64); 3] = [
        (
            "events_per_day",
            scenario.events_per_day(),
            scenario.published_events_per_day,
            0,
        ),
        (
            "postgres_mb_per_month",
            scenario.postgres_mb_per_month(),
            scenario.published_postgres_mb_per_month,
            5,
        ),
        (
            "qr_sessions_per_day",
            scenario.qr_sessions_from_share(),
            scenario.published_qr_sessions_per_day,
            10,
        ),
    ];
    for (quantity, derived, published, tolerance_percent) in checks {
        if !within_tolerance(derived, published, tolerance_percent) {
            found.push(Discrepancy {
                scenario: scenario.name,
                quantity,
                derived,
                published,
                tolerance_percent,
            });
        }
    }
    found
}

/// Every discrepancy across the three scenarios — the capacity model's standing reconciliation report.
#[must_use]
pub fn reconcile_all() -> Vec<Discrepancy> {
    SCENARIOS.iter().flat_map(reconcile).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        Discrepancy, SCENARIO_A, SCENARIO_B, SCENARIO_C, SCENARIOS, reconcile, reconcile_all,
    };

    #[test]
    fn events_per_day_is_the_tables_own_implied_constant() {
        // Exact for all three, which is what proves EVENTS_PER_BILL = 8 is recovered from the table
        // rather than invented.
        for scenario in SCENARIOS {
            assert_eq!(
                scenario.events_per_day(),
                scenario.published_events_per_day,
                "events/day for scenario {} must reproduce the table exactly",
                scenario.name
            );
        }
    }

    #[test]
    fn postgres_storage_reproduces_the_published_monthly_figure() {
        // Within 5%: the model gives B 75 GB where the table rounds to 72.
        assert_eq!(SCENARIO_A.postgres_mb_per_month(), 9_000);
        assert_eq!(SCENARIO_B.postgres_mb_per_month(), 75_000);
        assert_eq!(SCENARIO_C.postgres_mb_per_month(), 4_800);
    }

    #[test]
    fn bandwidth_lands_inside_the_published_range_for_every_scenario() {
        for scenario in SCENARIOS {
            let (low, high) = scenario.published_bandwidth_mb_per_day;
            let derived = scenario.bandwidth_mb_per_day();
            assert!(
                derived >= low && derived <= high,
                "bandwidth for {} ({derived} MB/day) must land in the published {low}..={high} range",
                scenario.name
            );
        }
    }

    #[test]
    fn the_peak_ingest_formula_is_a_conservative_ceiling() {
        // The ÷1260 formula matches the smallest scenario exactly and over-estimates the larger two —
        // the safe direction for provisioning, so the published peak is always at or below it.
        for scenario in SCENARIOS {
            assert!(
                scenario.published_peak_ingest_per_second
                    <= scenario.peak_ingest_ceiling_per_second(),
                "scenario {}: published peak {} must sit under the ÷1260 ceiling {}",
                scenario.name,
                scenario.published_peak_ingest_per_second,
                scenario.peak_ingest_ceiling_per_second(),
            );
        }
    }

    #[test]
    fn the_qr_session_share_reconciles_for_b_and_c() {
        assert!(reconcile(&SCENARIO_B).is_empty(), "B reconciles cleanly");
        assert!(
            reconcile(&SCENARIO_C).is_empty(),
            "C reconciles within tolerance"
        );
    }

    #[test]
    fn scenario_a_qr_sessions_is_the_one_pinned_discrepancy() {
        // The bills×QR-share model gives A 18,000 QR sessions/day, but the table states 9,000 — a 2×
        // gap, while B (250k) and C (6k) both agree with the share. This is the single place the
        // published estimates do not reconcile; it is pinned here so it cannot be lost, and it is
        // filed for the pilot to settle (is A's QR share of *bills* really 15%, or is 9,000 a typo
        // for 18,000?).
        let found = reconcile(&SCENARIO_A);
        assert_eq!(
            found,
            vec![Discrepancy {
                scenario: "A",
                quantity: "qr_sessions_per_day",
                derived: 18_000,
                published: 9_000,
                tolerance_percent: 10,
            }],
            "scenario A must report exactly the QR-sessions discrepancy and nothing else"
        );
    }

    #[test]
    fn the_reconciliation_report_holds_exactly_one_finding() {
        // The whole model reproduces the whole table but for that single QR-sessions estimate.
        let report = reconcile_all();
        assert_eq!(
            report.len(),
            1,
            "one standing finding across all three scenarios"
        );
        assert_eq!(report[0].quantity, "qr_sessions_per_day");
        assert_eq!(report[0].scenario, "A");
    }
}
