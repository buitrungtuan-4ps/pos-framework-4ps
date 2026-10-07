//! CSV export rail ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md), Track M5).
//!
//! Pure serialisers that turn a domain's rows into an RFC-4180 CSV byte buffer with the `csv` crate —
//! quoting and escaping is the "fiddly-but-bounded, general" work [ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md)
//! says to buy rather than hand-roll. The HTTP layer ([`crate::http`]) gates each export on the
//! domain's manage permission, audits it (who exported which domain and how many rows, never the row
//! contents), and streams the bytes as a download; keeping the serialiser here — pure and
//! `#[cfg(test)]`-covered — keeps that route thin and the CSV shape unit-testable without a socket.
//!
//! **Data-classification scope (ADR-0075 decision 5).** This rail ships the *non-personal* domains:
//! catalog **items** and the **translation** grid. The **employee** roster is T1 (a bulk T1 export the
//! organisation's data-classification rules escalate) and the per-channel **price/placement** export
//! reproduces T2 verbatim; both wait on a human-approved DPIA/design and are deliberately absent here
//! (flagged in ADR-0075). A price is never a column below — an item CSV carries the item's authoring
//! fields only.
//!
//! **No cell is a formula.** A spreadsheet runs a cell that starts with `=`, `+`, `-`, `@`, a tab or
//! a carriage return, and the free text in these files is whatever somebody authored or imported.
//! So every free-text cell goes through [`escape_formula`], and the import takes back exactly what
//! it added ([`unescape_formula`]). Ids, tokens, dates and numbers are written as they are.

use std::borrow::Cow;
use std::collections::BTreeSet;

use crate::catalog::CatalogItem;
use crate::cloud::{DailyRevenue, DailyRollup};
use crate::registry::EntityStatus;
use crate::translations::TranslationGrid;

/// `EntityStatus` as the stable lowercase token the CSV (and a future import) uses.
fn status_token(status: EntityStatus) -> &'static str {
    match status {
        EntityStatus::Active => "active",
        EntityStatus::Archived => "archived",
    }
}

/// What a spreadsheet takes a cell to be a formula from when the cell starts with it: `=`, `+`, `-`,
/// `@`, a tab and a carriage return (OWASP, CSV injection).
const FORMULA_TRIGGERS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Whether `text` starts with a formula trigger after any number of `'`, none included.
fn quotes_then_trigger(text: &str) -> bool {
    text.trim_start_matches('\'').starts_with(FORMULA_TRIGGERS)
}

/// A free-text cell as an export writes it, so that a spreadsheet shows it rather than runs it.
///
/// A cell that starts with `=`, `+`, `-`, `@`, a tab or a carriage return is a formula to Excel,
/// Google Sheets and the like: an item named `=HYPERLINK("http://…","click")` would be a live link
/// in the file of whoever exports it and opens it. Such a cell is written with one leading `'`,
/// which a spreadsheet takes as text. A cell that starts with `'`s and then a trigger gets one more
/// `'` too, so that [`unescape_formula`] can give back exactly what was written, and every other cell
/// is written as it is. Only free text comes through here: an id, a token, a date or a number never
/// does, so a negative amount stays a number.
#[must_use]
pub(crate) fn escape_formula(text: &str) -> Cow<'_, str> {
    if quotes_then_trigger(text) {
        Cow::Owned(format!("'{text}"))
    } else {
        Cow::Borrowed(text)
    }
}

/// A free-text cell as an import reads it: one leading `'` taken off when the rest of the cell is
/// `'`s and then a formula trigger, which is exactly the `'` [`escape_formula`] added. Any other cell
/// is read as it is.
#[must_use]
pub(crate) fn unescape_formula(text: &str) -> &str {
    match text.strip_prefix('\'') {
        Some(rest) if quotes_then_trigger(rest) => rest,
        _ => text,
    }
}

/// Turns a finished `csv::Writer<Vec<u8>>` into its byte buffer, mapping the flush failure a writer's
/// `into_inner` can surface into a `csv::Error` so the caller has one error type.
fn finish(writer: csv::Writer<Vec<u8>>) -> Result<Vec<u8>, csv::Error> {
    writer
        .into_inner()
        .map_err(|error| csv::Error::from(error.into_error()))
}

/// Serialises a tenant's catalog items to CSV: the stable item id and its authoring fields (name,
/// status, tax class, category/sub-category, image ref). Never a price — prices are per-channel
/// placements, a separate (and deferred) export. The stable ids let a future import round-trip.
/// The name is the one free-text cell, written through [`escape_formula`].
///
/// # Errors
///
/// A `csv::Error` if serialisation fails — not expected for an in-memory buffer, but propagated
/// rather than swallowed.
pub fn items_csv(items: &[CatalogItem]) -> Result<Vec<u8>, csv::Error> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record([
        "menu_item_id",
        "name",
        "status",
        "tax_class_id",
        "item_category_id",
        "item_subcategory_id",
        "image_ref",
    ])?;
    for item in items {
        writer.write_record([
            item.menu_item_id.to_string(),
            escape_formula(&item.name).into_owned(),
            status_token(item.status).to_owned(),
            item.tax_class_id.to_string(),
            item.item_category_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            item.item_subcategory_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            item.image_ref
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        ])?;
    }
    finish(writer)
}

/// Serialises a tenant's translation grid to CSV: a `key` column, then one column per locale (the
/// union of every locale present, sorted for a stable header), and a row per content key. A cell the
/// grid omits is empty.
///
/// The grid's keys, locales and strings are all free text, written through [`escape_formula`]: a
/// locale is a header cell, but the grid is what names it.
///
/// # Errors
///
/// A `csv::Error` if serialisation fails.
pub fn translations_csv(grid: &TranslationGrid) -> Result<Vec<u8>, csv::Error> {
    let map = grid.as_map();
    let locales: BTreeSet<&str> = map
        .values()
        .flat_map(|by_locale| by_locale.keys().map(String::as_str))
        .collect();
    let locales: Vec<&str> = locales.into_iter().collect();

    let mut writer = csv::Writer::from_writer(Vec::new());
    let mut header = Vec::with_capacity(locales.len() + 1);
    header.push("key".to_owned());
    header.extend(
        locales
            .iter()
            .map(|locale| escape_formula(locale).into_owned()),
    );
    writer.write_record(&header)?;

    for (key, by_locale) in map {
        let mut record = Vec::with_capacity(locales.len() + 1);
        record.push(escape_formula(key).into_owned());
        for locale in &locales {
            record.push(
                by_locale
                    .get(*locale)
                    .map(|text| escape_formula(text).into_owned())
                    .unwrap_or_default(),
            );
        }
        writer.write_record(&record)?;
    }
    finish(writer)
}

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

    use super::{
        escape_formula, items_csv, revenue_csv, revenue_fees_csv, revenue_tax_csv, rollups_csv,
        translations_csv, unescape_formula,
    };
    use crate::catalog::{CatalogItem, ItemCategoryId};
    use crate::cloud::{DailyRevenue, FeeTotal, TaxComponentTotal};
    use crate::registry::EntityStatus;
    use crate::translations::TranslationGrid;
    use pos_proto::ids::{MenuItemId, TaxClassId, TenantId};
    use pos_proto::ulid::Ulid;

    fn ulid(byte: u128) -> Ulid {
        Ulid::from_u128(byte)
    }

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

    /// A text starting with each formula trigger, and how it is written: with one more leading `'`.
    const TRIGGERED: [(&str, &str); 6] = [
        ("=1+1", "'=1+1"),
        ("+84 hotline", "'+84 hotline"),
        ("-5", "'-5"),
        ("@a", "'@a"),
        ("\tTab", "'\tTab"),
        ("\rReturn", "'\rReturn"),
    ];

    fn item(seed: u128, name: &str) -> CatalogItem {
        CatalogItem {
            menu_item_id: MenuItemId::new(ulid(seed)),
            tenant_id: TenantId::new(ulid(9)),
            name: name.to_owned(),
            name_translations: BTreeMap::new(),
            tax_class_id: TaxClassId::new(ulid(2)),
            item_category_id: None,
            item_subcategory_id: None,
            course_id: None,
            image_ref: None,
            status: EntityStatus::Active,
        }
    }

    #[test]
    fn a_text_that_would_start_a_formula_is_written_as_text_and_read_back_as_it_was() {
        for (text, written) in TRIGGERED.into_iter().chain([
            // A `'` already there, then a trigger: one more, so the import takes back only its own.
            ("'=x", "''=x"),
            ("''=x", "'''=x"),
            // Nothing a spreadsheet runs: written as it is.
            ("Phở bò", "Phở bò"),
            ("'quoted'", "'quoted'"),
            ("a=b", "a=b"),
            ("", ""),
        ]) {
            assert_eq!(escape_formula(text), written, "{text:?}");
            assert_eq!(unescape_formula(written), text, "{written:?}");
        }
    }

    #[test]
    fn items_csv_writes_a_name_that_would_start_a_formula_as_text_and_the_rest_as_they_are() {
        let names = TRIGGERED.iter().chain(&[("Margherita", "Margherita")]);
        let items: Vec<CatalogItem> = (1..)
            .zip(names.clone())
            .map(|(seed, (name, _))| item(seed, name))
            .collect();
        let rows = records(&items_csv(&items).expect("serialise"));
        let expected: Vec<Vec<String>> = items
            .iter()
            .zip(names)
            .map(|(item, (_, written))| {
                vec![
                    item.menu_item_id.to_string(),
                    (*written).to_owned(),
                    "active".to_owned(),
                    item.tax_class_id.to_string(),
                    String::new(),
                    String::new(),
                    String::new(),
                ]
            })
            .collect();
        assert_eq!(
            rows.get(1..),
            Some(expected.as_slice()),
            "each trigger gets a `'`; the plain name, the ids and the status are as they were"
        );
    }

    #[test]
    fn translations_csv_writes_a_key_a_locale_and_a_string_that_would_start_a_formula_as_text() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "=k".to_owned(),
            BTreeMap::from([
                ("en".to_owned(), "-5 off".to_owned()),
                ("@xx".to_owned(), "\tTab".to_owned()),
            ]),
        );
        entries.insert(
            "menu.pho".to_owned(),
            BTreeMap::from([("en".to_owned(), "Pho".to_owned())]),
        );
        let rows = records(&translations_csv(&TranslationGrid::new(entries)).expect("serialise"));
        assert_eq!(
            rows,
            [
                ["key", "'@xx", "en"],
                ["'=k", "'\tTab", "'-5 off"],
                ["menu.pho", "", "Pho"],
            ]
        );
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
    fn items_csv_has_a_header_and_a_row_per_item() {
        let item = CatalogItem {
            menu_item_id: MenuItemId::new(ulid(1)),
            tenant_id: TenantId::new(ulid(9)),
            name: "Margherita".to_owned(),
            name_translations: BTreeMap::new(),
            tax_class_id: TaxClassId::new(ulid(2)),
            item_category_id: Some(ItemCategoryId::new(ulid(3))),
            item_subcategory_id: None,
            course_id: None,
            image_ref: None,
            status: EntityStatus::Active,
        };
        let csv = as_string(items_csv(&[item]).expect("serialise"));
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "menu_item_id,name,status,tax_class_id,item_category_id,item_subcategory_id,image_ref"
        );
        let row = lines.next().unwrap();
        assert!(row.contains("Margherita"));
        assert!(row.contains("active"));
        // The absent sub-category and image are empty trailing fields, not "None".
        assert!(row.ends_with(",,"));
    }

    #[test]
    fn items_csv_quotes_a_name_with_a_comma() {
        let item = CatalogItem {
            menu_item_id: MenuItemId::new(ulid(1)),
            tenant_id: TenantId::new(ulid(9)),
            name: "Ham, egg".to_owned(),
            name_translations: BTreeMap::new(),
            tax_class_id: TaxClassId::new(ulid(2)),
            item_category_id: None,
            item_subcategory_id: None,
            course_id: None,
            image_ref: None,
            status: EntityStatus::Archived,
        };
        let csv = as_string(items_csv(&[item]).expect("serialise"));
        // The comma-bearing field is quoted, so the record still parses as seven columns.
        assert!(csv.contains("\"Ham, egg\""));
        assert!(csv.contains("archived"));
    }

    #[test]
    fn translations_csv_unions_locales_and_fills_blanks() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "order.pay".to_owned(),
            BTreeMap::from([
                ("en".to_owned(), "Pay".to_owned()),
                ("vi".to_owned(), "Trả".to_owned()),
            ]),
        );
        entries.insert(
            "order.void".to_owned(),
            BTreeMap::from([("en".to_owned(), "Void".to_owned())]),
        );
        let grid = TranslationGrid::new(entries);
        let csv = as_string(translations_csv(&grid).expect("serialise"));
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "key,en,vi");
        // order.pay before order.void (BTreeMap order); the vi cell of order.void is an empty trailer.
        assert_eq!(lines.next().unwrap(), "order.pay,Pay,Trả");
        assert_eq!(lines.next().unwrap(), "order.void,Void,");
    }

    #[test]
    fn rollups_csv_has_a_header_and_a_row_per_day() {
        let day = crate::cloud::DailyRollup {
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
