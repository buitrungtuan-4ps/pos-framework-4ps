// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The tenant's translation grid as this store holds it
//! ([ADR-0043](../../../docs/adr/0043-translation-grid.md)): `key → { locale → text }`, pulled whole
//! from the cloud beside the store's configuration, so a package's labels (a custom field's name, a
//! list entry's) can be shown in the till's language, falling back to English.
//!
//! The grid holds labels HQ wrote, never personal data. It is bounded by [`GRID_KEYS_MAX`],
//! [`LOCALES_PER_KEY_MAX`] and [`LABEL_BYTES_MAX`]; a grid past them is refused whole and the store
//! keeps the one it had.

use std::collections::BTreeMap;

/// The most keys a held grid has.
pub const GRID_KEYS_MAX: usize = 20_000;

/// The most locales one key has.
pub const LOCALES_PER_KEY_MAX: usize = 32;

/// The longest key and the longest text, in bytes of UTF-8.
pub const LABEL_BYTES_MAX: usize = 1024;

/// The locale every key carries (ADR-0043).
pub const FALLBACK_LOCALE: &str = "en";

/// A tenant's translation grid, within its bounds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabelGrid {
    entries: BTreeMap<String, BTreeMap<String, String>>,
}

impl LabelGrid {
    /// The grid, or `None` when it is past a bound.
    #[must_use]
    pub fn bounded(entries: BTreeMap<String, BTreeMap<String, String>>) -> Option<Self> {
        let within = entries.len() <= GRID_KEYS_MAX
            && entries.iter().all(|(key, locales)| {
                key.len() <= LABEL_BYTES_MAX
                    && locales.len() <= LOCALES_PER_KEY_MAX
                    && locales
                        .iter()
                        .all(|(locale, text)| locale.len() <= 35 && text.len() <= LABEL_BYTES_MAX)
            });
        within.then_some(Self { entries })
    }

    /// Whether the grid holds no key.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The whole grid.
    #[must_use]
    pub const fn entries(&self) -> &BTreeMap<String, BTreeMap<String, String>> {
        &self.entries
    }

    /// `key`'s text in `locale`, else in English, else `None`.
    #[must_use]
    pub fn label(&self, key: &str, locale: &str) -> Option<&str> {
        let locales = self.entries.get(key)?;
        locales
            .get(locale)
            .or_else(|| locales.get(FALLBACK_LOCALE))
            .map(String::as_str)
            .filter(|text| !text.is_empty())
    }

    /// The entries of the keys `strings` names, each once: what a screen that uses those keys
    /// needs, and no more.
    #[must_use]
    pub fn subset<'a>(
        &self,
        strings: impl IntoIterator<Item = &'a str>,
    ) -> BTreeMap<String, BTreeMap<String, String>> {
        strings
            .into_iter()
            .filter_map(|key| {
                self.entries
                    .get_key_value(key)
                    .map(|(key, locales)| (key.clone(), locales.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{GRID_KEYS_MAX, LABEL_BYTES_MAX, LabelGrid};

    fn grid(pairs: &[(&str, &[(&str, &str)])]) -> BTreeMap<String, BTreeMap<String, String>> {
        pairs
            .iter()
            .map(|(key, locales)| {
                (
                    (*key).to_owned(),
                    locales
                        .iter()
                        .map(|(locale, text)| ((*locale).to_owned(), (*text).to_owned()))
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn a_label_falls_back_to_english_and_then_to_nothing() {
        let held = LabelGrid::bounded(grid(&[(
            "data.reservation.party",
            &[("en", "Party size"), ("vi", "Số khách")],
        )]))
        .expect("within bounds");
        assert_eq!(held.label("data.reservation.party", "vi"), Some("Số khách"));
        assert_eq!(
            held.label("data.reservation.party", "ja"),
            Some("Party size")
        );
        assert_eq!(held.label("data.unknown", "en"), None);
    }

    #[test]
    fn a_grid_past_a_bound_is_refused_whole() {
        let long = "x".repeat(LABEL_BYTES_MAX + 1);
        assert!(LabelGrid::bounded(grid(&[("data.a", &[("en", long.as_str())])])).is_none());
        let many: BTreeMap<_, _> = (0..=GRID_KEYS_MAX)
            .map(|n| (format!("k.{n}"), BTreeMap::new()))
            .collect();
        assert!(LabelGrid::bounded(many).is_none());
    }

    #[test]
    fn a_subset_holds_only_the_keys_named() {
        let held = LabelGrid::bounded(grid(&[("a.b", &[("en", "A")]), ("c.d", &[("en", "C")])]))
            .expect("within bounds");
        let subset = held.subset(["a.b", "not.a.key", "a.b"]);
        assert_eq!(subset.keys().collect::<Vec<_>>(), vec!["a.b"]);
    }
}
