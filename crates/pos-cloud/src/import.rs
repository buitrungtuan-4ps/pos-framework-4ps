//! CSV import rail, dry-run-first ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md), Track M5).
//!
//! A pure parse-and-classify over an uploaded CSV that produces a row-by-row report
//! (would-create / would-update / rejected-with-reason) **without writing**, plus the merged result to
//! save on an explicit confirm. The HTTP layer ([`crate::http`]) runs it twice: once for the dry-run
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

use std::collections::BTreeMap;

use pos_proto::ids::{TaxClassId, TenantId};
use pos_proto::ulid::Ulid;

use crate::catalog::{CatalogItem, ItemCategoryId, ItemSubcategoryId};
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
        .map(|header| header.trim().to_owned())
        .collect();

    let mut merged = existing.as_map().clone();
    let mut rows = Vec::new();
    let (mut creates, mut updates, mut rejects) = (0usize, 0usize, 0usize);

    for record in reader.records() {
        let record = record.map_err(|error| ImportError::Malformed(error.to_string()))?;
        let key = record.get(0).unwrap_or("").trim().to_owned();
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
            let value = record.get(index + 1).unwrap_or("").trim();
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

    let by_id: BTreeMap<String, &Versioned<CatalogItem>> = existing
        .iter()
        .map(|row| (row.record.menu_item_id.to_string(), row))
        .collect();

    let mut upserts = Vec::new();
    let mut rows = Vec::new();
    let (mut creates, mut updates, mut rejects) = (0usize, 0usize, 0usize);

    for record in reader.records() {
        let record = record.map_err(|error| ImportError::Malformed(error.to_string()))?;
        let id = record.get(0).unwrap_or("").trim().to_owned();
        // What names the row in the report: the id it claims, or its name when it claims none. A row
        // with neither is reported under an empty key, exactly as the translation rail does.
        let key = if id.is_empty() {
            record.get(1).unwrap_or("").trim().to_owned()
        } else {
            id.clone()
        };

        let fields = match read_item_fields(&record, tax_classes, categories, subcategories) {
            Ok(fields) => fields,
            Err(reason) => {
                rows.push(ImportRow {
                    key,
                    action: RowAction::Reject { reason },
                });
                rejects += 1;
                continue;
            }
        };

        if id.is_empty() {
            upserts.push(ItemUpsert::Create(NewItem {
                name: fields.name,
                status: fields.status,
                tax_class_id: fields.tax_class_id,
                item_category_id: fields.item_category_id,
                item_subcategory_id: fields.item_subcategory_id,
                image_ref: fields.image_ref,
            }));
            creates += 1;
            rows.push(ImportRow {
                key,
                action: RowAction::Create,
            });
            continue;
        }

        let Some(stored) = by_id.get(&id) else {
            rows.push(ImportRow {
                key,
                action: RowAction::Reject {
                    reason: "names an item this tenant does not have".to_owned(),
                },
            });
            rejects += 1;
            continue;
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
        upserts.push(ItemUpsert::Update {
            item: Box::new(item),
            expected: stored.etag.clone(),
        });
        updates += 1;
        rows.push(ImportRow {
            key,
            action: RowAction::Update,
        });
    }

    let report = ImportReport {
        rows,
        create_count: creates,
        update_count: updates,
        reject_count: rejects,
    };
    Ok((upserts, report))
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
    let name = cell(1);
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
}

#[cfg(test)]
mod item_tests {
    use std::collections::BTreeMap;

    use pos_proto::ids::{MenuItemId, TaxClassId, TenantId};
    use pos_proto::ulid::Ulid;

    use super::{ItemUpsert, RowAction, parse_items_csv};
    use crate::catalog::{CatalogItem, ItemCategoryId};
    use crate::registry::EntityStatus;
    use crate::version::{Version, Versioned};

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn tax_class() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(2))
    }

    fn category() -> ItemCategoryId {
        ItemCategoryId::new(Ulid::from_u128(3))
    }

    fn item_id() -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(4))
    }

    /// One stored item, carrying a Vietnamese name the CSV format cannot express.
    fn stored() -> Vec<Versioned<CatalogItem>> {
        vec![Versioned::new(
            CatalogItem {
                menu_item_id: item_id(),
                tenant_id: tenant(),
                name: "Pho".to_owned(),
                name_translations: BTreeMap::from([("vi".to_owned(), "Phở".to_owned())]),
                tax_class_id: tax_class(),
                item_category_id: None,
                item_subcategory_id: None,
                image_ref: None,
                status: EntityStatus::Active,
            },
            Version::new("v1"),
        )]
    }

    const HEADER: &str = "menu_item_id,name,status,tax_class_id,item_category_id,\
                          item_subcategory_id,image_ref\n";

    #[test]
    fn a_row_naming_a_stored_item_updates_it_and_a_row_naming_none_creates_one() {
        let csv = format!(
            "{HEADER}{id},Pho bo,active,{tax},{cat},,\n\
             ,Tra da,active,{tax},,,\n",
            id = item_id(),
            tax = tax_class(),
            cat = category(),
        );
        let (upserts, report) = parse_items_csv(
            csv.as_bytes(),
            tenant(),
            &stored(),
            &[tax_class()],
            &[category()],
            &[],
        )
        .expect("parse");

        assert_eq!(report.update_count, 1);
        assert_eq!(report.create_count, 1);
        assert_eq!(report.reject_count, 0);
        assert_eq!(report.rows[0].action, RowAction::Update);
        assert_eq!(report.rows[1].action, RowAction::Create);

        // The update carries the file's fields *and* the per-locale name the file could not carry.
        // An import that silently dropped translations would be a data loss nobody sees until a
        // Vietnamese till shows an English word.
        let ItemUpsert::Update { item, expected } = &upserts[0] else {
            panic!("the first row updates");
        };
        assert_eq!(item.name, "Pho bo");
        assert_eq!(item.item_category_id, Some(category()));
        assert_eq!(
            item.name_translations.get("vi").map(String::as_str),
            Some("Phở")
        );
        assert_eq!(expected, &Version::new("v1"), "the write stays conditional");
    }

    #[test]
    fn an_id_this_tenant_does_not_have_is_rejected_rather_than_created() {
        // The id is the identity an inbound order names. A file that could mint one would let a
        // spreadsheet create a wire id — and, worse, could quietly adopt another tenant's.
        let csv = format!(
            "{HEADER}{stranger},Pho,active,{tax},,,\n",
            stranger = MenuItemId::new(Ulid::from_u128(99)),
            tax = tax_class(),
        );
        let (upserts, report) = parse_items_csv(
            csv.as_bytes(),
            tenant(),
            &stored(),
            &[tax_class()],
            &[],
            &[],
        )
        .expect("parse");
        assert!(upserts.is_empty());
        assert_eq!(report.reject_count, 1);
        let RowAction::Reject { reason } = &report.rows[0].action else {
            panic!("the row is rejected");
        };
        assert!(reason.contains("does not have"), "{reason}");
    }

    #[test]
    fn a_foreign_key_the_tenant_does_not_have_is_rejected_by_name_and_the_file_is_read_on() {
        let csv = format!(
            "{HEADER},No tax,active,{stranger},,,\n\
             ,Fine,active,{tax},,,\n",
            stranger = TaxClassId::new(Ulid::from_u128(98)),
            tax = tax_class(),
        );
        let (upserts, report) = parse_items_csv(
            csv.as_bytes(),
            tenant(),
            &stored(),
            &[tax_class()],
            &[],
            &[],
        )
        .expect("parse");
        assert_eq!(report.reject_count, 1);
        assert_eq!(
            report.create_count, 1,
            "a bad row does not stop the file: the operator wants every verdict, not the first"
        );
        assert_eq!(upserts.len(), 1);
    }

    #[test]
    fn a_missing_name_and_an_unknown_status_are_rejections_not_errors() {
        let csv = format!(
            "{HEADER},,active,{tax},,,\n\
             ,Pho,retired,{tax},,,\n",
            tax = tax_class(),
        );
        let (upserts, report) = parse_items_csv(
            csv.as_bytes(),
            tenant(),
            &stored(),
            &[tax_class()],
            &[],
            &[],
        )
        .expect("parse");
        assert!(upserts.is_empty());
        assert_eq!(report.reject_count, 2);
    }

    #[test]
    fn a_header_that_is_not_the_export_s_is_an_error() {
        // The import accepts exactly what the export writes, so the pair round-trips. A file with
        // its own columns is a different format, and guessing at it is how a price column would
        // one day arrive by accident.
        let csv = "name,price\nPho,45000\n";
        assert!(
            parse_items_csv(
                csv.as_bytes(),
                tenant(),
                &stored(),
                &[tax_class()],
                &[],
                &[]
            )
            .is_err()
        );
    }
}
