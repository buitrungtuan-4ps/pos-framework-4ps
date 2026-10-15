//! CSV import rail, dry-run-first ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md), Track M5).
//!
//! A pure parse-and-classify over an uploaded CSV that produces a row-by-row report
//! (would-create / would-update / rejected-with-reason) **without writing**, plus the merged result to
//! save on an explicit confirm. The HTTP layer ([`crate::routes`]) runs it twice: once for the dry-run
//! (report only) and once, on the operator's confirm, to apply the valid rows. Keeping it here — pure
//! and `#[cfg(test)]`-covered — keeps those two routes thin and the classification unit-testable.
//!
//! **Scope (ADR-0075 decision 5).** The rail imports the non-personal **translation grid** and, since
//! roadmap-v3 **F15**, the **catalog item master** — both clean round-trips with slice 5's exports. The
//! T1 domains (the employee roster above all) stay deferred and are still flagged in ADR-0075: a bulk
//! personal-data import is a decision about lawful basis, not a parser.
//!
//! **No price crosses this rail, in either direction.** An item's price is a per-channel *placement*,
//! which the item export has never carried and this import does not accept — so the row-by-row report
//! an operator reads, and anything that logs it, cannot carry a price in clear text. That is a
//! property of the format rather than a redaction rule somebody has to remember to apply.
//!
//! **A free-text cell is read through [`unescape_formula`]**, after it is trimmed as it always was.
//! The export writes a cell that would start a spreadsheet formula with one more leading `'`
//! ([`crate::export::escape_formula`]), and this takes exactly that `'` back off, so an exported
//! file imports as the names, keys and strings it was made from.

use std::collections::BTreeMap;

use pos_proto::ids::{TaxClassId, TenantId};
use pos_proto::ulid::Ulid;

use crate::catalog::{CatalogItem, ItemCategoryId, ItemSubcategoryId};
use crate::export::unescape_formula;
use crate::media::MediaId;
use crate::registry::EntityStatus;
use crate::translations::{FALLBACK_LOCALE, TranslationGrid};
use crate::version::{Version, Versioned};

/// What an import would do to one row. Serialises with an `action` tag and, for a rejection, a
/// `reason` — the shape the dashboard renders row-by-row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum RowAction {
    /// The key is new — the import would add it.
    Create,
    /// The key exists — the import would overwrite its row.
    Update,
    /// The row is invalid and would be skipped; `reason` says why.
    Reject {
        /// A human-readable reason the row was rejected (e.g. `missing en value`).
        reason: String,
    },
}

/// One row's key and what would happen to it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportRow {
    /// The content key the row is for (may be empty for a rejected blank-key row).
    pub key: String,
    /// The classification.
    #[serde(flatten)]
    pub action: RowAction,
}

/// The dry-run (and post-apply) report: every row's fate plus the totals the UI headlines.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportReport {
    /// One entry per data row, in file order.
    pub rows: Vec<ImportRow>,
    /// How many rows would be created.
    pub create_count: usize,
    /// How many rows would overwrite an existing key.
    pub update_count: usize,
    /// How many rows were rejected.
    pub reject_count: usize,
}

/// A failure to even parse the upload — distinct from a per-row rejection, which is data, not an error.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The bytes are not valid CSV.
    #[error("the file is not valid CSV: {0}")]
    Malformed(String),
    /// The header's first column is not `key`.
    #[error("the header's first column must be `key`")]
    BadHeader,
}

/// Parses a translation-grid CSV against the tenant's `existing` grid, returning the **merged** grid to
/// save (existing keys preserved; imported valid rows added or overwritten; rejected rows excluded) and
/// a row-by-row [`ImportReport`]. Writes nothing — the caller saves the merged grid only on confirm.
///
/// The header is `key` then one column per locale. A row is rejected (not an error) when its key is
/// empty or it lacks a non-empty [`FALLBACK_LOCALE`] value — the same fallback rule the grid enforces
/// on save, so an applied import can never violate it.
///
/// # Errors
///
/// [`ImportError`] if the bytes are not CSV or the header does not start with `key`.
pub fn parse_translations_csv(
    bytes: &[u8],
    existing: &TranslationGrid,
) -> Result<(TranslationGrid, ImportReport), ImportError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(bytes);
    let headers = reader
        .headers()
        .map_err(|error| ImportError::Malformed(error.to_string()))?
        .clone();
    if headers.get(0).map(str::trim) != Some("key") {
        return Err(ImportError::BadHeader);
    }
    let locales: Vec<String> = headers
        .iter()
        .skip(1)
        .map(|header| unescape_formula(header.trim()).to_owned())
        .collect();

    let mut merged = existing.as_map().clone();
    let mut rows = Vec::new();
    let (mut creates, mut updates, mut rejects) = (0usize, 0usize, 0usize);

    for record in reader.records() {
        let record = record.map_err(|error| ImportError::Malformed(error.to_string()))?;
        let key = unescape_formula(record.get(0).unwrap_or("").trim()).to_owned();
        if key.is_empty() {
            rows.push(ImportRow {
                key,
                action: RowAction::Reject {
                    reason: "empty key".to_owned(),
                },
            });
            rejects += 1;
            continue;
        }
        let mut values = BTreeMap::new();
        for (index, locale) in locales.iter().enumerate() {
            if locale.is_empty() {
                continue;
            }
            let value = unescape_formula(record.get(index + 1).unwrap_or("").trim());
            if !value.is_empty() {
                values.insert(locale.clone(), value.to_owned());
            }
        }
        let has_fallback = values
            .get(FALLBACK_LOCALE)
            .is_some_and(|value| !value.trim().is_empty());
        if !has_fallback {
            rows.push(ImportRow {
                key,
                action: RowAction::Reject {
                    reason: format!("missing {FALLBACK_LOCALE} value"),
                },
            });
            rejects += 1;
            continue;
        }
        if existing.as_map().contains_key(&key) {
            updates += 1;
            rows.push(ImportRow {
                key: key.clone(),
                action: RowAction::Update,
            });
        } else {
            creates += 1;
            rows.push(ImportRow {
                key: key.clone(),
                action: RowAction::Create,
            });
        }
        merged.insert(key, values);
    }

    let report = ImportReport {
        rows,
        create_count: creates,
        update_count: updates,
        reject_count: rejects,
    };
    Ok((TranslationGrid::new(merged), report))
}

/// The columns an item CSV carries, in the order [`crate::export::items_csv`] writes them.
///
/// The import accepts exactly the export's header, which is what makes the pair a round-trip: an
/// operator exports the master, edits it in a spreadsheet, and imports it back. There is no price
/// column and there will not be one — see the module note.
const ITEM_HEADER: [&str; 7] = [
    "menu_item_id",
    "name",
    "status",
    "tax_class_id",
    "item_category_id",
    "item_subcategory_id",
    "image_ref",
];

/// An item the import would add — everything but the id, which the caller mints.
///
/// Minting happens in the caller rather than here so this stays a pure function over its inputs: a
/// parser that reached for a clock or a random source would give a different answer on the dry run
/// than on the apply, which is the one thing a dry run must never do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewItem {
    /// The item's name — the default caption.
    pub name: String,
    /// Active or archived.
    pub status: EntityStatus,
    /// The tax class, checked against the tenant's own before this is built.
    pub tax_class_id: TaxClassId,
    /// The operational category, or `None`.
    pub item_category_id: Option<ItemCategoryId>,
    /// The operational sub-category, or `None`.
    pub item_subcategory_id: Option<ItemSubcategoryId>,
    /// The item's photo, or `None`.
    pub image_ref: Option<MediaId>,
}

/// What the import would write for one valid row.
#[derive(Debug, Clone)]
pub enum ItemUpsert {
    /// A row naming no id: a new item, whose id the caller mints.
    Create(NewItem),
    /// A row naming an item this tenant has: the stored item with the file's fields applied, and
    /// the version it was read at, so the write is conditional like every other `/admin` update
    /// ([ADR-0094](../../../docs/adr/0094-optimistic-concurrency-on-admin.md)).
    Update {
        /// The item to write.
        item: Box<CatalogItem>,
        /// The version the row was read at.
        expected: Version,
    },
}

/// Parses an item-master CSV against the tenant's existing items and the ids a row may reference,
/// returning what the import **would write** and a row-by-row [`ImportReport`]. Writes nothing.
///
/// # How a row is classified
///
/// * **Update** — `menu_item_id` names an item this tenant has. The stored item is taken and the
///   file's fields applied over it, which is why the import cannot erase what the format does not
///   carry: an item's per-locale names survive an import that never mentioned them.
/// * **Create** — `menu_item_id` is empty. The id is minted by the caller on apply.
/// * **Reject** — anything else, with the reason named. An id that is not a ULID, or names an item
///   this tenant does not have, is rejected rather than created: `menu_item_id` is the identity an
///   inbound order names, so letting a file invent one would let a spreadsheet mint a wire id.
///   A missing name, an unknown status token, and a tax class / category / sub-category / image the
///   tenant does not have are all rejections too — a foreign key is checked here, where the operator
///   can see which row broke, rather than at the write, where the first failure hides the rest.
///
/// A rejected row is data, not an error: the file is still parsed to the end and every row gets a
/// verdict, because an operator fixing a spreadsheet wants the whole list, not the first problem.
///
/// # Errors
///
/// [`ImportError`] if the bytes are not CSV or the header is not the export's.
pub fn parse_items_csv(
    bytes: &[u8],
    tenant_id: TenantId,
    existing: &[Versioned<CatalogItem>],
    tax_classes: &[TaxClassId],
    categories: &[ItemCategoryId],
    subcategories: &[ItemSubcategoryId],
) -> Result<(Vec<ItemUpsert>, ImportReport), ImportError> {
    let mut reader = item_reader(bytes)?;
    let by_id: BTreeMap<String, &Versioned<CatalogItem>> = existing
        .iter()
        .map(|row| (row.record.menu_item_id.to_string(), row))
        .collect();

    let mut upserts = Vec::new();
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|error| ImportError::Malformed(error.to_string()))?;
        let id = record.get(0).unwrap_or("").trim().to_owned();
        // What names the row in the report: the id it claims, or its name when it claims none. A row
        // with neither is reported under an empty key, exactly as the translation rail does.
        let key = if id.is_empty() {
            unescape_formula(record.get(1).unwrap_or("").trim()).to_owned()
        } else {
            id.clone()
        };
        let upsert = read_item_fields(&record, tax_classes, categories, subcategories)
            .and_then(|fields| item_upsert(&id, fields, &by_id, tenant_id));
        let action = match upsert {
            Ok(upsert) => {
                let action = match &upsert {
                    ItemUpsert::Create(_) => RowAction::Create,
                    ItemUpsert::Update { .. } => RowAction::Update,
                };
                upserts.push(upsert);
                action
            }
            Err(reason) => RowAction::Reject { reason },
        };
        rows.push(ImportRow { key, action });
    }
    Ok((upserts, item_report(rows)))
}

/// A reader over an item CSV whose header is exactly the export's.
fn item_reader(bytes: &[u8]) -> Result<csv::Reader<&[u8]>, ImportError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(bytes);
    let headers = reader
        .headers()
        .map_err(|error| ImportError::Malformed(error.to_string()))?
        .clone();
    let named: Vec<String> = headers
        .iter()
        .map(|header| header.trim().to_owned())
        .collect();
    if named != ITEM_HEADER {
        return Err(ImportError::BadHeader);
    }
    Ok(reader)
}

/// What one valid row would write: a create when it names no id, an update of the stored item
/// when it names one this tenant has, and otherwise the reason it is rejected.
fn item_upsert(
    id: &str,
    fields: ItemFields,
    by_id: &BTreeMap<String, &Versioned<CatalogItem>>,
    tenant_id: TenantId,
) -> Result<ItemUpsert, String> {
    if id.is_empty() {
        return Ok(ItemUpsert::Create(NewItem {
            name: fields.name,
            status: fields.status,
            tax_class_id: fields.tax_class_id,
            item_category_id: fields.item_category_id,
            item_subcategory_id: fields.item_subcategory_id,
            image_ref: fields.image_ref,
        }));
    }
    let Some(stored) = by_id.get(id) else {
        return Err("names an item this tenant does not have".to_owned());
    };
    // The stored item with the file's fields over it: the columns the CSV does not carry — the
    // per-locale names above all — are preserved rather than defaulted away.
    let mut item = stored.record.clone();
    item.tenant_id = tenant_id;
    item.name = fields.name;
    item.status = fields.status;
    item.tax_class_id = fields.tax_class_id;
    item.item_category_id = fields.item_category_id;
    item.item_subcategory_id = fields.item_subcategory_id;
    item.image_ref = fields.image_ref;
    Ok(ItemUpsert::Update {
        item: Box::new(item),
        expected: stored.etag.clone(),
    })
}

/// The report over `rows`, counting each verdict.
fn item_report(rows: Vec<ImportRow>) -> ImportReport {
    let count =
        |wanted: fn(&RowAction) -> bool| rows.iter().filter(|row| wanted(&row.action)).count();
    let create_count = count(|action| matches!(action, RowAction::Create));
    let update_count = count(|action| matches!(action, RowAction::Update));
    let reject_count = count(|action| matches!(action, RowAction::Reject { .. }));
    ImportReport {
        rows,
        create_count,
        update_count,
        reject_count,
    }
}

/// One row's fields, once every cell has parsed and every foreign key has been found.
struct ItemFields {
    name: String,
    status: EntityStatus,
    tax_class_id: TaxClassId,
    item_category_id: Option<ItemCategoryId>,
    item_subcategory_id: Option<ItemSubcategoryId>,
    image_ref: Option<MediaId>,
}

/// Reads one CSV record into [`ItemFields`], or the reason the row is rejected.
///
/// Split out of [`parse_items_csv`] so the loop there reads as what it is — classify, then create or
/// update — rather than burying that under seven cell parses. `Err` is a row-level rejection, not a
/// failure of the import: the caller records it and reads on.
fn read_item_fields(
    record: &csv::StringRecord,
    tax_classes: &[TaxClassId],
    categories: &[ItemCategoryId],
    subcategories: &[ItemSubcategoryId],
) -> Result<ItemFields, String> {
    let cell = |index: usize| record.get(index).unwrap_or("").trim().to_owned();
    // The one free-text cell: the export may have put a `'` before it.
    let name = unescape_formula(&cell(1)).to_owned();
    if name.is_empty() {
        return Err("missing name".to_owned());
    }
    let status = status_from_token(&cell(2))
        .ok_or_else(|| format!("`{}` is not `active` or `archived`", cell(2)))?;
    let tax_class_id = parse_id(&cell(3), TaxClassId::new)
        .ok_or_else(|| "tax_class_id is missing or not an id".to_owned())?;
    if !tax_classes.contains(&tax_class_id) {
        return Err("names a tax class this tenant does not have".to_owned());
    }
    let item_category_id = optional_id(&cell(4), ItemCategoryId::new, categories, "category")?;
    let item_subcategory_id = optional_id(
        &cell(5),
        ItemSubcategoryId::new,
        subcategories,
        "sub-category",
    )?;
    let image = cell(6);
    let image_ref = if image.is_empty() {
        None
    } else {
        Some(parse_id(&image, MediaId::new).ok_or_else(|| "image_ref is not an id".to_owned())?)
    };
    Ok(ItemFields {
        name,
        status,
        tax_class_id,
        item_category_id,
        item_subcategory_id,
        image_ref,
    })
}

/// An optional id cell: empty is `None`, a non-ULID or an id the tenant does not have is a rejection
/// naming which of the two it was, because they are different mistakes to fix.
fn optional_id<T: PartialEq>(
    cell: &str,
    wrap: impl Fn(Ulid) -> T,
    known: &[T],
    what: &str,
) -> Result<Option<T>, String> {
    if cell.is_empty() {
        return Ok(None);
    }
    let parsed = parse_id(cell, wrap).ok_or_else(|| format!("the {what} is not an id"))?;
    if !known.contains(&parsed) {
        return Err(format!("names a {what} this tenant does not have"));
    }
    Ok(Some(parsed))
}

/// `active` / `archived`, the two tokens [`crate::export::items_csv`] writes.
fn status_from_token(token: &str) -> Option<EntityStatus> {
    match token {
        "active" => Some(EntityStatus::Active),
        "archived" => Some(EntityStatus::Archived),
        _ => None,
    }
}

/// Parses a ULID cell into a typed id, or `None` when it is empty or not a ULID.
fn parse_id<T>(cell: &str, wrap: impl Fn(Ulid) -> T) -> Option<T> {
    cell.parse::<Ulid>().ok().map(wrap)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{RowAction, parse_translations_csv};
    use crate::translations::TranslationGrid;

    fn existing() -> TranslationGrid {
        let mut entries = BTreeMap::new();
        entries.insert(
            "menu.pho".to_owned(),
            BTreeMap::from([("en".to_owned(), "Pho".to_owned())]),
        );
        TranslationGrid::new(entries)
    }

    #[test]
    fn classifies_create_update_and_reject_without_writing() {
        let csv = "key,en,vi\n\
                   menu.pho,Pho noodles,Phở\n\
                   menu.tea,Tea,Trà\n\
                   menu.rice,,Cơm\n\
                   ,Orphan,\n";
        let (merged, report) = parse_translations_csv(csv.as_bytes(), &existing()).expect("parse");
        assert_eq!(report.create_count, 1, "menu.tea is new");
        assert_eq!(report.update_count, 1, "menu.pho exists");
        assert_eq!(
            report.reject_count, 2,
            "the no-en row and the blank-key row"
        );
        // menu.pho updated, menu.tea created, menu.rice rejected (no en), blank key rejected.
        assert_eq!(report.rows[0].action, RowAction::Update);
        assert_eq!(report.rows[1].action, RowAction::Create);
        assert!(matches!(report.rows[2].action, RowAction::Reject { .. }));
        assert!(matches!(report.rows[3].action, RowAction::Reject { .. }));
        // The merged grid carries the valid rows and preserves nothing rejected.
        let map = merged.as_map();
        assert_eq!(
            map.get("menu.pho").unwrap().get("en").unwrap(),
            "Pho noodles"
        );
        assert!(map.contains_key("menu.tea"));
        assert!(!map.contains_key("menu.rice"));
        // The merged grid still satisfies the fallback rule, so an apply cannot 422.
        assert!(merged.keys_missing_fallback().is_empty());
    }

    #[test]
    fn a_bad_header_is_an_error_not_a_row() {
        let csv = "name,en\nmenu.pho,Pho\n";
        assert!(parse_translations_csv(csv.as_bytes(), &existing()).is_err());
    }

    #[test]
    fn an_escaped_key_locale_and_string_are_read_without_the_quote_the_export_added() {
        // As the export writes `=k`, a locale `@xx`, `=1+1` and `'=x`; a `'` before anything that
        // is not a trigger is the operator's own and stays.
        let csv = "key,en,'@xx\n\
                   '=k,'=1+1,''=x\n\
                   menu.tea,'Tea,\n";
        let (merged, report) =
            parse_translations_csv(csv.as_bytes(), &TranslationGrid::default()).expect("parse");
        assert_eq!(report.create_count, 2);
        assert_eq!(
            merged.as_map().get("=k"),
            Some(&BTreeMap::from([
                ("en".to_owned(), "=1+1".to_owned()),
                ("@xx".to_owned(), "'=x".to_owned()),
            ]))
        );
        assert_eq!(
            merged.as_map().get("menu.tea"),
            Some(&BTreeMap::from([("en".to_owned(), "'Tea".to_owned())]))
        );
    }
}

#[cfg(test)]
#[path = "import/tests/items.rs"]
mod item_tests;

#[cfg(test)]
mod round_trip_tests {
    //! Export then import gives back what was exported, for every string: the formula escape and
    //! the import's undoing of it are each other's inverse, through the `csv` crate's quoting and
    //! through both import paths.
    //!
    //! No property-testing crate, which `pos-cloud` does not depend on: every string of up to four
    //! characters from an alphabet that holds each trigger, and a fixed-seed run of longer ones, so
    //! a failure names an input that fails again.

    use std::collections::BTreeMap;

    use pos_proto::ids::{MenuItemId, TaxClassId, TenantId};
    use pos_proto::ulid::Ulid;

    use super::{ItemUpsert, parse_items_csv, parse_translations_csv};
    use crate::catalog::CatalogItem;
    use crate::export::{escape_formula, items_csv, translations_csv, unescape_formula};
    use crate::registry::EntityStatus;
    use crate::translations::TranslationGrid;
    use crate::version::{Version, Versioned};

    /// Each formula trigger, the quote the escape adds, and what else a cell may hold around them:
    /// a double quote, a comma, a line feed, a space, a letter, a digit and a letter outside ASCII.
    const ALPHABET: [char; 14] = [
        '=', '+', '-', '@', '\t', '\r', '\'', '"', ',', '\n', ' ', 'a', '1', 'đ',
    ];

    /// Every string of at most `longest` characters from [`ALPHABET`], the empty one included:
    /// 41,371 of them at four.
    fn every_string(longest: usize) -> Vec<String> {
        let mut all = vec![String::new()];
        let mut previous = vec![String::new()];
        for _ in 0..longest {
            previous = previous
                .iter()
                .flat_map(|prefix| {
                    ALPHABET.iter().map(move |next| {
                        let mut longer = prefix.clone();
                        longer.push(*next);
                        longer
                    })
                })
                .collect();
            all.extend(previous.iter().cloned());
        }
        all
    }

    /// `count` strings of 5 to 40 characters from [`ALPHABET`], from a xorshift64 with a fixed
    /// seed: the same strings on every run.
    fn longer_strings(count: usize) -> Vec<String> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            usize::try_from(state % u64::try_from(bound).unwrap_or(1)).unwrap_or(0)
        };
        (0..count)
            .map(|_| {
                let length = 5 + next(36);
                (0..length)
                    .map(|_| ALPHABET.get(next(ALPHABET.len())).copied().unwrap_or('a'))
                    .collect()
            })
            .collect()
    }

    /// The strings every property here is checked over.
    fn inputs() -> Vec<String> {
        let mut inputs = every_string(4);
        inputs.extend(longer_strings(20_000));
        inputs
    }

    /// Whether the import keeps `text` once it has taken back the escape. It trims a cell and
    /// refuses an empty one, as it always has, so a text that ends in whitespace, starts with
    /// whitespace other than the tab or return the escape keeps, or holds nothing else, is not one
    /// it gives back.
    fn kept_by_the_import(text: &str) -> bool {
        !text.trim().is_empty()
            && text.trim_end() == text
            && (text.trim_start() == text || text.starts_with(['\t', '\r']))
    }

    #[test]
    fn every_string_survives_the_escape_and_a_csv_cell_and_comes_back_as_it_was() {
        let inputs = inputs();
        let mut writer = csv::Writer::from_writer(Vec::new());
        for text in &inputs {
            let written = escape_formula(text);
            assert!(
                !written.starts_with(['=', '+', '-', '@', '\t', '\r']),
                "{text:?} is written as {written:?}, which a spreadsheet would run"
            );
            writer
                .write_record([written.as_ref(), "end"])
                .expect("write the cell");
        }
        let bytes = writer.into_inner().expect("flush the file");
        let read: Vec<String> = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(bytes.as_slice())
            .records()
            .map(|record| {
                let record = record.expect("a record");
                unescape_formula(record.get(0).expect("the cell")).to_owned()
            })
            .collect();
        assert_eq!(read.len(), inputs.len(), "one record per string");
        for (text, back) in inputs.iter().zip(&read) {
            assert_eq!(back, text);
        }
    }

    #[test]
    fn an_exported_item_master_imports_as_the_names_it_was_made_from() {
        let names: Vec<String> = inputs()
            .into_iter()
            .filter(|name| kept_by_the_import(name))
            .collect();
        let tenant = TenantId::new(Ulid::from_u128(1));
        let tax_class = TaxClassId::new(Ulid::from_u128(2));
        let items: Vec<CatalogItem> = (1_000..)
            .zip(&names)
            .map(|(seed, name)| CatalogItem {
                menu_item_id: MenuItemId::new(Ulid::from_u128(seed)),
                tenant_id: tenant,
                name: name.clone(),
                name_translations: BTreeMap::new(),
                tax_class_id: tax_class,
                item_category_id: None,
                item_subcategory_id: None,
                course_id: None,
                image_ref: None,
                status: EntityStatus::Active,
            })
            .collect();
        let stored: Vec<Versioned<CatalogItem>> = items
            .iter()
            .map(|item| Versioned::new(item.clone(), Version::new("v1")))
            .collect();

        let exported = items_csv(&items).expect("export");
        let (upserts, report) =
            parse_items_csv(&exported, tenant, &stored, &[tax_class], &[], &[]).expect("import");
        assert_eq!(report.reject_count, 0);
        assert_eq!(upserts.len(), names.len());
        for (name, upsert) in names.iter().zip(&upserts) {
            let ItemUpsert::Update { item, .. } = upsert else {
                panic!("{name:?} names a stored item, so it updates it");
            };
            assert_eq!(&item.name, name);
        }
    }

    #[test]
    fn an_exported_translation_grid_imports_as_the_keys_locales_and_strings_it_was_made_from() {
        // A locale that would start a formula, beside the fallback every key needs.
        let grid = TranslationGrid::new(
            inputs()
                .into_iter()
                .filter(|text| kept_by_the_import(text))
                .map(|text| {
                    let strings = BTreeMap::from([
                        ("en".to_owned(), text.clone()),
                        ("=xx".to_owned(), text.clone()),
                    ]);
                    (text, strings)
                })
                .collect(),
        );

        let exported = translations_csv(&grid).expect("export");
        let (merged, report) =
            parse_translations_csv(&exported, &TranslationGrid::default()).expect("import");
        assert_eq!(report.reject_count, 0);
        assert_eq!(merged.as_map().len(), grid.as_map().len());
        for (key, strings) in grid.as_map() {
            assert_eq!(merged.as_map().get(key), Some(strings), "{key:?}");
        }
    }
}
