//! CSV import rail, dry-run-first ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md), Track M5).
//!
//! A pure parse-and-classify over an uploaded CSV that produces a row-by-row report
//! (would-create / would-update / rejected-with-reason) **without writing**, plus the merged result to
//! save on an explicit confirm. The HTTP layer ([`crate::http`]) runs it twice: once for the dry-run
//! (report only) and once, on the operator's confirm, to apply the valid rows. Keeping it here — pure
//! and `#[cfg(test)]`-covered — keeps those two routes thin and the classification unit-testable.
//!
//! **Scope (ADR-0075 decision 5).** This rail imports the non-personal **translation grid** — the clean
//! round-trip with slice 5's translation export. Item import (upsert with FK validation and id minting)
//! and the T1/T2 domains are deferred to a human-reviewed slice, flagged in ADR-0075.

use std::collections::BTreeMap;

use crate::translations::{FALLBACK_LOCALE, TranslationGrid};

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
