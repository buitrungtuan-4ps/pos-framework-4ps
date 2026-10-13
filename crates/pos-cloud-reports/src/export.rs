// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The reports' CSV files.

use pos_cloud_api::csv_file::{escape_formula, finish};

use crate::types::{DailyRevenue, DailyRollup};

/// Serialises a store's daily activity rollups to CSV: one row per trading day with its total event
/// count (ADR-0081, Track O4). Counts only — no money, no PII — so the route gates it on the ordinary
/// read permission. `days` is expected oldest-first (as the windowed read returns them).
///
/// # Errors
///
/// A `csv::Error` if serialisation fails.
pub fn rollups_csv(days: &[DailyRollup]) -> Result<Vec<u8>, csv::Error> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(["business_date", "total_events"])?;
    for day in days {
        writer.write_record([day.business_date.clone(), day.total_events.to_string()])?;
    }
    finish(writer)
}

/// Serialises a store's daily revenue rollups to CSV: one row per trading day with the settled
/// totals (ADR-0081, Track O4). Amounts are the store's currency's minor units. Revenue is **T2**,
/// so the route gates this on `console.reports.revenue` and audits only the row count. The product
/// mix (`by_item`) is two-dimensional and not flattened here — the daily totals are the export —
/// and the fees by code (`by_fee`) and the tax by component (`by_tax_component`) are exports of
/// their own, [`revenue_fees_csv`] and [`revenue_tax_csv`].
///
/// # Errors
///
/// A `csv::Error` if serialisation fails.
pub fn revenue_csv(days: &[DailyRevenue]) -> Result<Vec<u8>, csv::Error> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record([
        "business_date",
        "currency_code",
        "bills",
        "gross",
        "reductions",
        "service_charge",
        "tax",
        "net",
    ])?;
    for day in days {
        writer.write_record([
            day.business_date.clone(),
            day.currency_code.clone(),
            day.bills.to_string(),
            day.gross.to_string(),
            day.reductions.to_string(),
            day.service_charge.to_string(),
            day.tax.to_string(),
            day.net.to_string(),
        ])?;
    }
    finish(writer)
}

/// Serialises a store's daily fees by code to CSV, long-format: one row per trading day per fee
/// code the day's settled bills charged, with the fee's name, how many bills charged it, what it
/// charged and the tax on it ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision
/// 4). Days oldest-first as `days` holds them, and codes in order within a day; a day that charged
/// no fee has no row. Amounts are the store's currency's minor units, and T2 like [`revenue_csv`],
/// whose header this leaves as it is.
///
/// A bill settled by an edge from before fee lines is in `revenue.csv`'s `service_charge` and in
/// no row here ([`DailyRevenue::by_fee`]).
///
/// A fee's code and name are free text an operator typed, written through [`escape_formula`].
///
/// # Errors
///
/// A `csv::Error` if serialisation fails.
pub fn revenue_fees_csv(days: &[DailyRevenue]) -> Result<Vec<u8>, csv::Error> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record([
        "business_date",
        "currency_code",
        "fee_code",
        "fee_name",
        "bills",
        "amount",
        "tax",
    ])?;
    for day in days {
        for (code, fee) in &day.by_fee {
            writer.write_record([
                day.business_date.clone(),
                day.currency_code.clone(),
                escape_formula(code).into_owned(),
                escape_formula(&fee.name).into_owned(),
                fee.bills.to_string(),
                fee.amount.to_string(),
                fee.tax.to_string(),
            ])?;
        }
    }
    finish(writer)
}

/// Serialises a store's daily tax by component to CSV, long-format: one row per trading day per
/// component name and rate the day's settled bills recorded, with the tax their tax lines recorded
/// under it ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md)
/// decision 5). Days oldest-first as `days` holds them, and by name and then rate within a day; a
/// day that recorded none has no row, so a store whose tax table names no components, as in
/// Vietnam and Japan, exports only the header. Amounts are the store's currency's minor units, and
/// T2 like [`revenue_csv`].
///
/// Nothing is invented: a bill that recorded no components is in `revenue.csv`'s `tax` and in no
/// row here, so a day's rows may sum to less than its tax ([`DailyRevenue::by_tax_component`]).
///
/// A component's name is a token, which cannot start a formula, and it is written through
/// [`escape_formula`] all the same, as every text cell of an export is.
///
/// # Errors
///
/// A `csv::Error` if serialisation fails.
pub fn revenue_tax_csv(days: &[DailyRevenue]) -> Result<Vec<u8>, csv::Error> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record([
        "business_date",
        "currency_code",
        "component_name",
        "rate_basis_points",
        "tax",
    ])?;
    for day in days {
        for total in &day.by_tax_component {
            writer.write_record([
                day.business_date.clone(),
                day.currency_code.clone(),
                escape_formula(&total.component_name).into_owned(),
                total.rate_basis_points.to_string(),
                total.tax.to_string(),
            ])?;
        }
    }
    finish(writer)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{revenue_csv, revenue_fees_csv, revenue_tax_csv, rollups_csv};
    use crate::types::{DailyRevenue, FeeTotal, TaxComponentTotal};

    fn as_string(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).expect("CSV is valid UTF-8")
    }

    /// Every record in `bytes`, the header first, as its cells: what a reader of the file sees,
    /// whatever quoting a cell needed.
    fn records(bytes: &[u8]) -> Vec<Vec<String>> {
        csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(bytes)
            .records()
            .map(|record| {
                record
                    .expect("a record")
                    .iter()
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn revenue_fees_csv_writes_a_code_and_a_name_as_text_and_a_negative_amount_as_a_number() {
        let days = [revenue_day(
            "2026-03-15",
            &[
                ("=SVC", "+Service", 1, -3_000, -240),
                ("PACK", "Packaging", 1, 3_000, 0),
            ],
        )];
        let rows = records(&revenue_fees_csv(&days).expect("serialise"));
        assert_eq!(
            rows.get(1..),
            Some(
                &[
                    [
                        "2026-03-15",
                        "VND",
                        "'=SVC",
                        "'+Service",
                        "1",
                        "-3000",
                        "-240"
                    ],
                    ["2026-03-15", "VND", "PACK", "Packaging", "1", "3000", "0"],
                ]
                .map(|row| row.map(str::to_owned).to_vec())[..]
            )
        );
    }

    #[test]
    fn revenue_csv_writes_a_negative_amount_as_a_number() {
        // A day whose refunds outweighed its takings: every cell is a date, a code or a number.
        let day = DailyRevenue {
            gross: -10_000,
            reductions: -2_000,
            net: -5_000,
            ..revenue_day("2026-03-15", &[])
        };
        let csv = as_string(revenue_csv(&[day]).expect("serialise"));
        assert_eq!(
            csv.lines().nth(1),
            Some("2026-03-15,VND,3,-10000,-2000,15000,24000,-5000")
        );
    }

    #[test]
    fn rollups_csv_has_a_header_and_a_row_per_day() {
        let day = crate::types::DailyRollup {
            business_date: "2026-03-15".to_owned(),
            total_events: 42,
            by_type: BTreeMap::new(),
        };
        let csv = as_string(rollups_csv(&[day]).expect("serialise"));
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "business_date,total_events");
        assert_eq!(lines.next().unwrap(), "2026-03-15,42");
    }

    /// A day of three settled bills that charged `fees`, each as (code, name, bills, amount, tax).
    fn revenue_day(date: &str, fees: &[(&str, &str, u64, i64, i64)]) -> DailyRevenue {
        DailyRevenue {
            business_date: date.to_owned(),
            currency_code: "VND".to_owned(),
            bills: 3,
            gross: 300_000,
            reductions: 20_000,
            service_charge: 15_000,
            tax: 24_000,
            net: 319_000,
            by_item: BTreeMap::new(),
            by_fee: fees
                .iter()
                .map(|&(code, name, bills, amount, tax)| {
                    let name = name.to_owned();
                    (
                        code.to_owned(),
                        FeeTotal {
                            name,
                            bills,
                            amount,
                            tax,
                        },
                    )
                })
                .collect(),
            by_tax_component: Vec::new(),
        }
    }

    #[test]
    fn revenue_csv_carries_the_daily_totals() {
        // A day with fees still exports the same eight columns: they are their own export.
        let day = revenue_day("2026-03-15", &[("SVC", "Service charge", 3, 15_000, 1_200)]);
        let csv = as_string(revenue_csv(&[day]).expect("serialise"));
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "business_date,currency_code,bills,gross,reductions,service_charge,tax,net"
        );
        assert_eq!(
            lines.next().unwrap(),
            "2026-03-15,VND,3,300000,20000,15000,24000,319000"
        );
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn revenue_fees_csv_has_a_row_per_day_per_fee_code() {
        let days = [
            revenue_day(
                "2026-03-15",
                &[
                    ("SVC", "Service charge", 3, 12_000, 960),
                    ("PACK", "Packaging, per box", 1, 3_000, 0),
                ],
            ),
            // A day that charged no fee has no row.
            revenue_day("2026-03-16", &[]),
            revenue_day("2026-03-17", &[("SVC", "Phí phục vụ", 2, 8_000, 640)]),
        ];
        let csv = as_string(revenue_fees_csv(&days).expect("serialise"));
        assert_eq!(
            csv.lines().collect::<Vec<_>>(),
            [
                "business_date,currency_code,fee_code,fee_name,bills,amount,tax",
                // Oldest day first, then by code; a name with a comma is quoted.
                "2026-03-15,VND,PACK,\"Packaging, per box\",1,3000,0",
                "2026-03-15,VND,SVC,Service charge,3,12000,960",
                "2026-03-17,VND,SVC,Phí phục vụ,2,8000,640",
            ]
        );
    }

    #[test]
    fn revenue_fees_csv_of_a_window_with_no_fees_is_its_header() {
        let csv =
            as_string(revenue_fees_csv(&[revenue_day("2026-03-15", &[])]).expect("serialise"));
        assert_eq!(
            csv,
            "business_date,currency_code,fee_code,fee_name,bills,amount,tax\n"
        );
    }

    /// An Indian day: three bills that recorded `components` as (name, rate, tax), in the order the
    /// rollup keeps them.
    fn split_day(date: &str, components: &[(&str, u32, i64)]) -> DailyRevenue {
        DailyRevenue {
            currency_code: "INR".to_owned(),
            by_tax_component: components
                .iter()
                .map(|&(name, rate_basis_points, tax)| TaxComponentTotal {
                    component_name: name.to_owned(),
                    rate_basis_points,
                    tax,
                })
                .collect(),
            ..revenue_day(date, &[])
        }
    }

    #[test]
    fn revenue_tax_csv_has_a_row_per_day_per_component_and_rate() {
        let days = [
            split_day(
                "2026-03-15",
                &[
                    ("CGST", 250, 5_000),
                    ("CGST", 900, 1_800),
                    ("SGST", 250, 5_000),
                    ("SGST", 900, 1_800),
                ],
            ),
            // A day that recorded none has no row.
            revenue_day("2026-03-16", &[]),
            split_day("2026-03-17", &[("CGST", 250, 2_500), ("SGST", 250, 2_500)]),
        ];
        let csv = as_string(revenue_tax_csv(&days).expect("serialise"));
        assert_eq!(
            csv.lines().collect::<Vec<_>>(),
            [
                "business_date,currency_code,component_name,rate_basis_points,tax",
                // Oldest day first, then as the day keeps them: by name, then by rate.
                "2026-03-15,INR,CGST,250,5000",
                "2026-03-15,INR,CGST,900,1800",
                "2026-03-15,INR,SGST,250,5000",
                "2026-03-15,INR,SGST,900,1800",
                "2026-03-17,INR,CGST,250,2500",
                "2026-03-17,INR,SGST,250,2500",
            ]
        );
    }

    #[test]
    fn revenue_tax_csv_writes_a_name_as_text_and_a_negative_tax_as_a_number() {
        // No token starts with a formula character, and the file does not rest on that.
        let day = split_day("2026-03-15", &[("=CGST", 250, -1_250)]);
        let rows = records(&revenue_tax_csv(&[day]).expect("serialise"));
        assert_eq!(
            rows.get(1..),
            Some(
                &[["2026-03-15", "INR", "'=CGST", "250", "-1250"]
                    .map(str::to_owned)
                    .to_vec()][..]
            )
        );
    }

    #[test]
    fn revenue_tax_csv_of_a_window_with_no_components_is_its_header() {
        // Every Vietnamese and Japanese day: their tax lines record no components.
        let days = [
            revenue_day("2026-03-15", &[]),
            revenue_day("2026-03-16", &[]),
        ];
        let csv = as_string(revenue_tax_csv(&days).expect("serialise"));
        assert_eq!(
            csv,
            "business_date,currency_code,component_name,rate_basis_points,tax\n"
        );
    }
}
