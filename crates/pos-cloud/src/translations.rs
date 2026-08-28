// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The translation-grid seam ([ADR-0043](../../../docs/adr/0043-translation-grid.md)).
//!
//! A tenant authors its localized menu/content strings here, feeding the edge's ICU i18n runtime
//! ([ADR-0020](../../../docs/adr/0020-i18n-runtime.md)). The grid is `key → { locale → string }`, and
//! the one rule that matters is structural: **`en` is the always-present fallback**, so a grid whose
//! every key does not carry a non-empty `en` value is refused at authoring time and the edge can
//! always degrade to English rather than to a raw key. Persistence is one `jsonb` per tenant in
//! `store-postgres`; a fake backs the tests.

use core::future::Future;
use std::collections::BTreeMap;

use pos_proto::ids::TenantId;

/// The locale every key must carry — the always-present fallback ([ADR-0020](../../../docs/adr/0020-i18n-runtime.md)).
pub const FALLBACK_LOCALE: &str = "en";

/// A tenant's translation grid: content key → (locale → rendered string).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TranslationGrid(BTreeMap<String, BTreeMap<String, String>>);

impl TranslationGrid {
    /// Wraps a `key → { locale → string }` map as a grid.
    #[must_use]
    pub fn new(entries: BTreeMap<String, BTreeMap<String, String>>) -> Self {
        Self(entries)
    }

    /// Whether the grid has no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The grid as a read-only `key → { locale → string }` map — for the CSV export (ADR-0075), which
    /// needs to walk keys and the union of locales the transparent serde wrapper does not expose.
    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<String, BTreeMap<String, String>> {
        &self.0
    }

    /// The keys that lack a non-empty [`FALLBACK_LOCALE`] value — the violations of the always-present
    /// fallback rule. Empty means the grid is valid to publish.
    #[must_use]
    pub fn keys_missing_fallback(&self) -> Vec<String> {
        self.0
            .iter()
            .filter(|(_, locales)| {
                locales
                    .get(FALLBACK_LOCALE)
                    .is_none_or(|value| value.trim().is_empty())
            })
            .map(|(key, _)| key.clone())
            .collect()
    }
}

/// A failure of the translation store itself — the database is unreachable, or a stored grid is
/// malformed.
#[derive(Debug, thiserror::Error)]
#[error("the translation store failed: {0}")]
pub struct TranslationStoreError(String);

impl TranslationStoreError {
    /// A store failure carrying a human-readable reason (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Persists and reads a tenant's translation grid.
pub trait TranslationStore {
    /// Loads a tenant's grid, or `None` if it has authored none yet.
    ///
    /// # Errors
    ///
    /// [`TranslationStoreError`] if the store could not be read or the stored grid is malformed.
    fn load(
        &self,
        tenant: TenantId,
    ) -> impl Future<Output = Result<Option<TranslationGrid>, TranslationStoreError>> + Send;

    /// Replaces a tenant's grid wholesale. The caller validates the fallback rule first.
    ///
    /// # Errors
    ///
    /// [`TranslationStoreError`] if the store could not be written.
    fn save(
        &self,
        tenant: TenantId,
        grid: &TranslationGrid,
    ) -> impl Future<Output = Result<(), TranslationStoreError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{TranslationGrid, TranslationStoreError};
    use std::collections::BTreeMap;

    fn grid(pairs: &[(&str, &[(&str, &str)])]) -> TranslationGrid {
        let mut entries = BTreeMap::new();
        for (key, locales) in pairs {
            let mut locale_map = BTreeMap::new();
            for (locale, value) in *locales {
                locale_map.insert((*locale).to_owned(), (*value).to_owned());
            }
            entries.insert((*key).to_owned(), locale_map);
        }
        TranslationGrid::new(entries)
    }

    #[test]
    fn a_key_without_a_non_empty_en_is_a_violation() {
        let g = grid(&[
            ("menu.pho", &[("en", "Pho"), ("vi", "Phở")]),
            ("menu.tea", &[("vi", "Trà")]),
            ("menu.rice", &[("en", "   "), ("ja", "ご飯")]),
        ]);
        let mut missing = g.keys_missing_fallback();
        missing.sort();
        assert_eq!(
            missing,
            vec!["menu.rice".to_owned(), "menu.tea".to_owned()],
            "a key with no en, and a key with a blank en, both violate the fallback rule"
        );
    }

    #[test]
    fn a_grid_with_en_on_every_key_is_valid() {
        let g = grid(&[
            ("menu.pho", &[("en", "Pho"), ("ja", "フォー")]),
            ("menu.tea", &[("en", "Tea")]),
        ]);
        assert!(
            g.keys_missing_fallback().is_empty(),
            "every key carries a non-empty en, so the grid publishes"
        );
    }

    #[test]
    fn the_store_error_carries_its_reason() {
        assert_eq!(
            TranslationStoreError::new("boom").to_string(),
            "the translation store failed: boom"
        );
    }
}
