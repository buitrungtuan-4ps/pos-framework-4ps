// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which cell this cloud is ([ADR-0179](../../../docs/adr/0179-cells-the-directory-and-the-front-door.md), C1).
//!
//! A **cell** is one complete cloud deployment, named by a cell id (`vn1`, `jp1`) and never by a
//! country. `cloud.toml` may carry a `[cell]` section saying which cell this process is: its id, an
//! optional country alias (`vn` → `vn1`), the country its tenants are homed by, the URL it answers
//! on, and the country its servers run in.
//!
//! **Absent is level 0.** A cloud with no `[cell]` section is the one and only cell, under the id
//! [`DEFAULT_CELL_ID`], with nothing else known about it. That is today's single deployment, and a
//! fork that never adds a second cell never writes the section.
//!
//! Present, every field but the alias is required and checked at boot, so a typo stops the process
//! rather than becoming the id a directory later redirects tenants to. The section holds no
//! personal data and no secret: it is safe to show to any console role that may read data.

use pos_proto::locale::CountryCode;
use serde::Deserialize;

/// The cell id of a cloud whose `cloud.toml` has no `[cell]` section.
pub const DEFAULT_CELL_ID: &str = "default";

/// The shortest cell id. Three, so a cell id can never be mistaken for a two-letter country code:
/// a cell is named by an id, a country is at most its alias (ADR-0179 D-R3).
const MIN_CELL_ID_LEN: usize = 3;

/// The longest cell id. A cell id may become a hostname label, and this leaves room in the 63
/// a DNS label allows for whatever a fork puts beside it.
const MAX_CELL_ID_LEN: usize = 32;

/// The `[cell]` section of `cloud.toml`, as written.
///
/// Unknown keys are refused: a misspelt `base_ulr` would otherwise parse as "no base URL" and boot.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellSection {
    /// The cell id: lower-case ASCII letters, digits and `-`, starting with a letter, 3 to 32
    /// characters (`vn1`).
    pub cell_id: String,
    /// An optional country alias that names this cell where a country is written instead of a cell
    /// id (`vn`). Two ASCII letters.
    #[serde(default)]
    pub country_alias: Option<String>,
    /// The country this cell's tenants are homed by, as an ISO 3166-1 alpha-2 code (`VN`).
    pub home_country: String,
    /// The origin this cell answers on (`https://vn1.example.com`): `https`, no path, no query.
    /// Plain `http` is accepted only for `localhost` and loopback addresses, for a developer's box.
    pub base_url: String,
    /// The country this cell's servers run in, as an ISO 3166-1 alpha-2 code. Usually the home
    /// country; where it is not, the boot says so and carries on.
    pub runs_in: String,
}

/// Which cell this cloud is, checked: what the console shows and the directory will name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The cell id.
    pub cell_id: String,
    /// The country alias, lower-case, when one is configured.
    pub country_alias: Option<String>,
    /// The home country. `None` only when no `[cell]` section is configured.
    pub home_country: Option<CountryCode>,
    /// The origin this cell answers on, without a trailing `/`. `None` only when no `[cell]`
    /// section is configured.
    pub base_url: Option<String>,
    /// The country the servers run in. `None` only when no `[cell]` section is configured.
    pub runs_in: Option<CountryCode>,
    /// Whether `cloud.toml` carries a `[cell]` section at all.
    pub configured: bool,
}

impl Cell {
    /// The level-0 cell: the only one, under [`DEFAULT_CELL_ID`], with nothing else known.
    #[must_use]
    pub fn single() -> Self {
        Self {
            cell_id: DEFAULT_CELL_ID.to_owned(),
            country_alias: None,
            home_country: None,
            base_url: None,
            runs_in: None,
            configured: false,
        }
    }

    /// Whether the servers run in a different country from the one the tenants are homed by. Never
    /// a refusal (ADR-0179): the boot logs it, and an operator decides.
    #[must_use]
    pub fn runs_abroad(&self) -> bool {
        match (self.home_country, self.runs_in) {
            (Some(home), Some(runs_in)) => home != runs_in,
            _ => false,
        }
    }
}

impl CellSection {
    /// Checks every field and returns the cell it describes.
    ///
    /// # Errors
    ///
    /// A message naming the `[cell]` key that cannot be honoured, and what it must be.
    pub fn resolve(&self) -> Result<Cell, String> {
        let cell_id = checked_cell_id(&self.cell_id)?;
        let country_alias = self
            .country_alias
            .as_deref()
            .map(checked_alias)
            .transpose()?;
        let home_country = checked_country("home_country", &self.home_country)?;
        let runs_in = checked_country("runs_in", &self.runs_in)?;
        let base_url = checked_base_url(&self.base_url)?;
        Ok(Cell {
            cell_id,
            country_alias,
            home_country: Some(home_country),
            base_url: Some(base_url),
            runs_in: Some(runs_in),
            configured: true,
        })
    }
}

/// The cell `[cell]` describes, or the level-0 cell when there is no section.
///
/// # Errors
///
/// As [`CellSection::resolve`].
pub fn resolve(section: Option<&CellSection>) -> Result<Cell, String> {
    section.map_or_else(|| Ok(Cell::single()), CellSection::resolve)
}

fn checked_cell_id(raw: &str) -> Result<String, String> {
    let well_formed = (MIN_CELL_ID_LEN..=MAX_CELL_ID_LEN).contains(&raw.len())
        && raw.starts_with(|c: char| c.is_ascii_lowercase())
        && !raw.ends_with('-')
        && raw
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if well_formed {
        Ok(raw.to_owned())
    } else {
        Err(format!(
            "[cell] cell_id {raw:?} must be {MIN_CELL_ID_LEN} to {MAX_CELL_ID_LEN} lower-case \
             letters, digits or '-', starting with a letter and not ending with '-' (`vn1`). A \
             cell is named by an id, never by a country; a country goes in country_alias \
             (ADR-0179)"
        ))
    }
}

fn checked_alias(raw: &str) -> Result<String, String> {
    if raw.len() == 2 && raw.chars().all(|c| c.is_ascii_alphabetic()) {
        Ok(raw.to_ascii_lowercase())
    } else {
        Err(format!(
            "[cell] country_alias {raw:?} must be two letters (`vn`), or be left out (ADR-0179)"
        ))
    }
}

fn checked_country(key: &str, raw: &str) -> Result<CountryCode, String> {
    CountryCode::parse(raw).map_err(|error| {
        format!("[cell] {key} {raw:?} must be an ISO 3166-1 alpha-2 country code (`VN`): {error}")
    })
}

fn checked_base_url(raw: &str) -> Result<String, String> {
    let refuse = |why: &str| {
        Err(format!(
            "[cell] base_url {raw:?} {why}. It is the origin this cell answers on, such as \
             `https://vn1.example.com` (ADR-0179)"
        ))
    };
    let Ok(url) = url::Url::parse(raw) else {
        return refuse("is not a URL");
    };
    let loopback = match url.host() {
        Some(url::Host::Domain(name)) => name == "localhost",
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return refuse("must use https (plain http only for localhost)"),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return refuse("must not carry a user name or password");
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return refuse("must be an origin, with no path, query or fragment");
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests;
