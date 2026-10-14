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

use std::collections::BTreeSet;

use pos_cloud_api::csv_file::finish;
pub(crate) use pos_cloud_api::csv_file::{escape_formula, unescape_formula};
// The reports' CSV writers moved to pos-cloud-reports (ADR-0170 decision 4); re-exported here.
pub use pos_cloud_reports::export::{revenue_csv, revenue_fees_csv, revenue_tax_csv, rollups_csv};

use crate::catalog::CatalogItem;
use crate::registry::EntityStatus;
use crate::translations::TranslationGrid;

/// `EntityStatus` as the stable lowercase token the CSV (and a future import) uses.
fn status_token(status: EntityStatus) -> &'static str {
    match status {
        EntityStatus::Active => "active",
        EntityStatus::Archived => "archived",
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{escape_formula, items_csv, translations_csv, unescape_formula};
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
}
