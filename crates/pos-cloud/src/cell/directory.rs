// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cell directory ([ADR-0179](../../../../docs/adr/0179-cells-the-directory-and-the-front-door.md) C2):
//! one signed, PII-free JSON document that every cell carries, so any cell can say which cell holds
//! a tenant.
//!
//! ```json
//! {
//!   "directory_version": 7,
//!   "cells": { "vn1": "https://vn1.example.com", "jp1": "https://jp1.example.com" },
//!   "countries": { "VN": "vn1", "JP": "jp1", "*": "vn1" },
//!   "tenants": { "pho-hanoi": "vn1" }
//! }
//! ```
//!
//! Three maps and nothing else: `tenant_slug → cell_id`, `country → default cell` with a `*`
//! fallback, and `cell_id → base URL`. A field this build does not know is ignored, so a newer
//! directory still loads in an older cell. Everything else is checked: every id, slug, country and
//! URL is well-formed, every cell a map names is in `cells`, `*` is present, this cell is listed
//! under its own base URL, and every map is within a fixed size.
//!
//! This module checks a document's **content**; [`super::signed`] checks its **signature** and keeps
//! the last good one.

use std::collections::BTreeMap;

use pos_proto::locale::CountryCode;
use serde::Deserialize;

use super::{Cell, checked_base_url, checked_cell_id};

/// The largest directory file read, in bytes. Ample for the tenant counts below.
pub const MAX_DIRECTORY_BYTES: usize = 8 * 1024 * 1024;
/// The most cells one directory may name.
const MAX_CELLS: usize = 1_024;
/// The most country entries, `*` included: every ISO 3166-1 code fits.
const MAX_COUNTRIES: usize = 300;
/// The most tenants one directory may name.
const MAX_TENANTS: usize = 200_000;
/// The longest tenant slug: one DNS label, since a slug may become a hostname (ADR-0023).
const MAX_SLUG_LEN: usize = 63;
/// The key of the fallback country entry.
pub const ANY_COUNTRY: &str = "*";

/// The document as written.
#[derive(Debug, Deserialize)]
struct RawDirectory {
    directory_version: u64,
    cells: BTreeMap<String, String>,
    countries: BTreeMap<String, String>,
    #[serde(default)]
    tenants: BTreeMap<String, String>,
}

/// A directory whose content has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    /// The version the signer gave it. A cell only ever moves to a higher one.
    pub directory_version: u64,
    cells: BTreeMap<String, String>,
    countries: BTreeMap<String, String>,
    tenants: BTreeMap<String, String>,
}

impl Directory {
    /// Parses and checks a directory document for the cell `this` is.
    ///
    /// # Errors
    ///
    /// A message naming the first thing wrong with it.
    pub fn parse(bytes: &[u8], this: &Cell) -> Result<Self, String> {
        if bytes.len() > MAX_DIRECTORY_BYTES {
            return Err(format!(
                "the directory is {} bytes, over the {MAX_DIRECTORY_BYTES} a cell reads",
                bytes.len()
            ));
        }
        let raw: RawDirectory = serde_json::from_slice(bytes)
            .map_err(|error| format!("the directory is not a directory document: {error}"))?;
        check_sizes(&raw)?;
        for (cell_id, base_url) in &raw.cells {
            checked_cell_id(cell_id)?;
            let normal = checked_base_url(base_url)?;
            if &normal != base_url {
                return Err(format!(
                    "the directory's base URL for {cell_id} must be written as {normal}"
                ));
            }
        }
        let known = |cell_id: &String, at: &str| {
            if raw.cells.contains_key(cell_id) {
                Ok(())
            } else {
                Err(format!(
                    "the directory's {at} names the cell {cell_id:?}, which its cells map lacks"
                ))
            }
        };
        for (country, cell_id) in &raw.countries {
            if country != ANY_COUNTRY && checked_country_key(country).is_none() {
                return Err(format!(
                    "the directory's countries map has the key {country:?}: a key is an upper-case \
                     ISO 3166-1 alpha-2 code or \"*\""
                ));
            }
            known(cell_id, "countries map")?;
        }
        if !raw.countries.contains_key(ANY_COUNTRY) {
            return Err("the directory's countries map has no \"*\" fallback".to_owned());
        }
        for (slug, cell_id) in &raw.tenants {
            if !is_slug(slug) {
                return Err(format!(
                    "the directory's tenants map has a key that is not a tenant slug: 1 to \
                     {MAX_SLUG_LEN} lower-case letters, digits or '-', not starting or ending \
                     with '-'"
                ));
            }
            known(cell_id, "tenants map")?;
        }
        check_lists_this_cell(&raw, this)?;
        Ok(Self {
            directory_version: raw.directory_version,
            cells: raw.cells,
            countries: raw.countries,
            tenants: raw.tenants,
        })
    }

    /// The cell a tenant is homed in, when the directory lists it.
    #[must_use]
    pub fn tenant_cell(&self, tenant_slug: &str) -> Option<&str> {
        self.tenants.get(tenant_slug).map(String::as_str)
    }

    /// The default cell for a country: its own entry, or the `*` fallback.
    #[must_use]
    pub fn country_cell(&self, country: CountryCode) -> Option<&str> {
        self.countries
            .get(country.as_str())
            .or_else(|| self.countries.get(ANY_COUNTRY))
            .map(String::as_str)
    }

    /// A cell's base URL.
    #[must_use]
    pub fn cell_url(&self, cell_id: &str) -> Option<&str> {
        self.cells.get(cell_id).map(String::as_str)
    }
}

/// The directory lists this cell, and at this cell's own base URL when one is configured.
fn check_lists_this_cell(raw: &RawDirectory, this: &Cell) -> Result<(), String> {
    match (raw.cells.get(&this.cell_id), &this.base_url) {
        (None, _) => Err(format!(
            "the directory does not list this cell, {}",
            this.cell_id
        )),
        (Some(listed), Some(own)) if listed != own => Err(format!(
            "the directory lists this cell, {}, at {listed}, but its [cell] base_url is {own}",
            this.cell_id
        )),
        _ => Ok(()),
    }
}

fn check_sizes(raw: &RawDirectory) -> Result<(), String> {
    for (name, len, max) in [
        ("cells", raw.cells.len(), MAX_CELLS),
        ("countries", raw.countries.len(), MAX_COUNTRIES),
        ("tenants", raw.tenants.len(), MAX_TENANTS),
    ] {
        if len > max {
            return Err(format!(
                "the directory's {name} map has {len} entries, over the {max} a cell holds"
            ));
        }
    }
    Ok(())
}

/// A country key as the directory writes it: upper-case, so one country has one spelling.
fn checked_country_key(key: &str) -> Option<CountryCode> {
    CountryCode::parse(key)
        .ok()
        .filter(|code| code.as_str() == key)
}

/// A tenant slug: one DNS label, lower-case.
pub(crate) fn is_slug(slug: &str) -> bool {
    (1..=MAX_SLUG_LEN).contains(&slug.len())
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
pub(crate) mod tests;
