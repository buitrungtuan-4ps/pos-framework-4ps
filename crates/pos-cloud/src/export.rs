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
            item.name.clone(),
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
    header.extend(locales.iter().map(|locale| (*locale).to_owned()));
    writer.write_record(&header)?;

    for (key, by_locale) in map {
        let mut record = Vec::with_capacity(locales.len() + 1);
        record.push(key.clone());
        for locale in &locales {
            record.push(by_locale.get(*locale).cloned().unwrap_or_default());
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

/// Serialises a store's daily revenue rollups to CSV: one row per trading day with the settled totals
/// (ADR-0081, Track O4). Amounts are the store's currency's minor units. Revenue is **T2**, so the
/// route gates this on `console.reports.revenue` and audits only the row count. The product mix
/// (`by_item`) is two-dimensional and not flattened here — the daily totals are the export.
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{items_csv, revenue_csv, rollups_csv, translations_csv};
    use crate::catalog::{CatalogItem, ItemCategoryId};
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

    #[test]
    fn revenue_csv_carries_the_daily_totals() {
        let day = crate::cloud::DailyRevenue {
            business_date: "2026-03-15".to_owned(),
            currency_code: "VND".to_owned(),
            bills: 3,
            gross: 300_000,
            reductions: 20_000,
            service_charge: 15_000,
            tax: 24_000,
            net: 319_000,
            by_item: BTreeMap::new(),
        };
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
    }
}
