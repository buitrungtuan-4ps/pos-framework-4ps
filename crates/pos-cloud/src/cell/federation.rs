// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! HQ federation of rollups across cells
//! ([ADR-0179](../../../../docs/adr/0179-cells-the-directory-and-the-front-door.md) C10).
//!
//! A company whose stores sit in more than one cell has one tenant per cell, because console
//! accounts and data are per cell. Its head office still wants one view of activity. That view is
//! assembled here, from each other cell's **public `/v1` API**: the same
//! `GET /v1/stores/{store_id}/rollups/daily` an integrator calls, with an API key that cell issued
//! with the `read_rollups` scope. **Only aggregates cross a cell boundary**: a store's count of
//! events per trading day and per event type. No personal data, no `subject_id`, no row about a
//! person and never a raw event. A test pins the rollup's fields, so a field added to it fails until
//! someone decides it may cross a border.
//!
//! `[federation]` in `cloud.toml` names the sources: for each other cell, its id, the key its
//! operator issued to this HQ, and the stores to read. The cell's base URL is never configured
//! here; it comes from the signed directory, so a source can only be a cell the directory key
//! holder listed. The key is a secret, redacted in every `Debug`.
//!
//! This module is the record and the fold; the transport is a [`RollupFetcher`], so the fold is
//! tested without a network, and the HTTPS fetcher and the console route come after it.

use core::fmt;
use core::future::Future;

use pos_cloud_reports::types::DailyRollup;
use serde::Deserialize;

use super::checked_cell_id;
use super::signed::CellDirectory;

/// The most cells one HQ reads from.
const MAX_SOURCES: usize = 64;
/// The most stores read from one cell.
const MAX_STORES_PER_SOURCE: usize = 1_000;
/// The most stores one federated read covers, across every cell: each is one bounded call.
const MAX_STORES_TOTAL: usize = 2_000;

/// `[federation]` in `cloud.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FederationSection {
    /// The other cells this HQ reads rollups from.
    #[serde(default)]
    pub sources: Vec<FederationSource>,
}

/// One other cell, the key it issued, and the stores to read there.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FederationSource {
    /// The cell id, as the signed directory lists it.
    pub cell_id: String,
    /// An API key that cell issued with the `read_rollups` scope.
    pub api_key: SourceKey,
    /// The stores to read in that cell, as 26-character ULIDs.
    pub stores: Vec<String>,
}

/// An API key another cell issued. Never printed.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct SourceKey(String);

impl SourceKey {
    /// Wraps a key.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key itself, for the `Authorization` header and nothing else.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SourceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceKey(redacted)")
    }
}

impl FederationSection {
    /// Checks every source: a well-formed cell id named once, a non-blank key, and 1 to
    /// [`MAX_STORES_PER_SOURCE`] well-formed store ids; at most [`MAX_SOURCES`] sources.
    ///
    /// # Errors
    ///
    /// A message naming the source and the key at fault.
    pub fn validate(&self) -> Result<(), String> {
        if self.sources.len() > MAX_SOURCES {
            return Err(format!(
                "[federation] names {} sources; at most {MAX_SOURCES} are read",
                self.sources.len()
            ));
        }
        let total: usize = self.sources.iter().map(|source| source.stores.len()).sum();
        if total > MAX_STORES_TOTAL {
            return Err(format!(
                "[federation] names {total} stores in all; at most {MAX_STORES_TOTAL} are read"
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for source in &self.sources {
            checked_cell_id(&source.cell_id)
                .map_err(|why| why.replacen("[cell] cell_id", "[federation] cell_id", 1))?;
            if !seen.insert(source.cell_id.as_str()) {
                return Err(format!(
                    "[federation] names the cell {} twice",
                    source.cell_id
                ));
            }
            if source.api_key.expose().trim().is_empty() {
                return Err(format!(
                    "[federation] source {} has no api_key: ask that cell's operator for a key \
                     with the read_rollups scope",
                    source.cell_id
                ));
            }
            if source.stores.is_empty() || source.stores.len() > MAX_STORES_PER_SOURCE {
                return Err(format!(
                    "[federation] source {} names {} stores; it names 1 to {MAX_STORES_PER_SOURCE}",
                    source.cell_id,
                    source.stores.len()
                ));
            }
            if let Some(bad) = source
                .stores
                .iter()
                .find(|store| store.parse::<pos_proto::ulid::Ulid>().is_err())
            {
                return Err(format!(
                    "[federation] source {} names the store {bad:?}, which is not a ULID",
                    source.cell_id
                ));
            }
        }
        Ok(())
    }
}

/// The trading-day window a federated read asks each cell for, passed on as the `/v1` query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollupWindow {
    /// The first business date, `YYYY-MM-DD`, inclusive.
    pub from: Option<String>,
    /// The last business date, `YYYY-MM-DD`, inclusive.
    pub to: Option<String>,
}

/// How a store's rollups are read from another cell.
pub trait RollupFetcher {
    /// The store's daily rollups, read from the cell at `base_url` with `key`.
    ///
    /// # Errors
    ///
    /// Why they could not be read, in words an operator can act on.
    fn fetch(
        &self,
        base_url: &str,
        store_id: &str,
        key: &SourceKey,
        window: &RollupWindow,
    ) -> impl Future<Output = Result<Vec<DailyRollup>, String>> + Send;
}

/// One store's answer in a federated read: its rollups, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FederatedStore {
    /// The cell the store is in.
    pub cell_id: String,
    /// The store.
    pub store_id: String,
    /// Its daily rollups, oldest day first. Null when they could not be read.
    pub rollups: Option<Vec<DailyRollup>>,
    /// Why they could not be read. Null when they were.
    pub error: Option<String>,
}

/// Reads every configured store's rollups, one cell after another. A cell the directory does not
/// list, or a store that cannot be read, is an answer with an error; it never fails the others.
pub async fn federate<F>(
    directory: &CellDirectory,
    federation: &FederationSection,
    fetcher: &F,
    window: &RollupWindow,
) -> Vec<FederatedStore>
where
    F: RollupFetcher + Sync,
{
    let held = directory.held();
    let mut answers = Vec::new();
    for source in &federation.sources {
        let base = held
            .as_ref()
            .and_then(|held| held.cell_url(&source.cell_id))
            .map(str::to_owned);
        for store_id in &source.stores {
            let outcome = match &base {
                None => Err(format!(
                    "the cell directory this cell holds does not list the cell {}",
                    source.cell_id
                )),
                Some(base) => fetcher.fetch(base, store_id, &source.api_key, window).await,
            };
            let (rollups, error) = match outcome {
                Ok(rollups) => (Some(rollups), None),
                Err(why) => (None, Some(why)),
            };
            answers.push(FederatedStore {
                cell_id: source.cell_id.clone(),
                store_id: store_id.clone(),
                rollups,
                error,
            });
        }
    }
    answers
}

#[cfg(test)]
mod tests;
