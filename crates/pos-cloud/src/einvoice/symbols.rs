// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The ledger of every e-invoice symbol a tenant's stores were given, and allocation from it
//! ([ADR-0240](../../../../docs/adr/0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md)).
//!
//! Each issuing point numbers under a symbol of its own (ADR-0225 decision 2), so two stores of one
//! tenant must never be given the same symbol, and a replaced box's symbol must never be handed out
//! again. Reading the symbols published now cannot promise either, because a replacement overwrites
//! the old one; the ledger remembers every symbol for good. Hand entry records into it, and
//! allocation takes the country rule's next index the ledger does not hold.

use core::future::Future;
use core::pin::Pin;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use pos_country::CountryModule;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;

/// How a symbol entered the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolSource {
    /// Typed by hand in the console or sent to the hand route.
    Manual,
    /// Allocated by the cloud from the country's rule.
    Allocated,
}

impl SymbolSource {
    /// The stored token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "MANUAL",
            Self::Allocated => "ALLOCATED",
        }
    }

    fn parse(token: &str) -> Self {
        if token == "ALLOCATED" {
            Self::Allocated
        } else {
            Self::Manual
        }
    }
}

/// One symbol in the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRecord {
    /// The symbol, such as `1C26MAA`.
    pub symbol: String,
    /// The store it was given to, or `None` for the tenant's correction symbol.
    pub store_id: Option<String>,
    /// The store's lease generation when it was given, where it had one.
    pub lease_generation: Option<i64>,
    /// How it entered the ledger.
    pub source: SymbolSource,
    /// When it was recorded.
    pub recorded_at_ms: i64,
}

impl SymbolRecord {
    /// Whether this record gives the symbol to `store`.
    #[must_use]
    pub fn is_held_by(&self, store: StoreId) -> bool {
        self.store_id.as_deref() == Some(store.to_string().as_str())
    }
}

/// The ledger could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerError(pub String);

/// What a ledger call returns.
pub type LedgerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LedgerError>> + Send + 'a>>;

/// The ledger: a table in `store-postgres`, or [`InMemorySymbolLedger`] in tests.
pub trait SymbolLedger: Send + Sync {
    /// Records `record` unless the tenant already holds its symbol, and returns the record the
    /// ledger holds for the symbol afterwards: `record` when it was recorded, the earlier one if not.
    fn record(&self, tenant: TenantId, record: SymbolRecord) -> LedgerFuture<'_, SymbolRecord>;
    /// The tenant's symbols, oldest first.
    fn list(&self, tenant: TenantId) -> LedgerFuture<'_, Vec<SymbolRecord>>;
}

/// The most symbols [`InMemorySymbolLedger`] holds across tenants.
pub const IN_MEMORY_LIMIT: usize = 10_000;

/// A ledger in memory, bounded at [`IN_MEMORY_LIMIT`]: for tests and a cloud run without a database.
#[derive(Debug, Clone, Default)]
pub struct InMemorySymbolLedger {
    rows: Arc<Mutex<BTreeMap<(String, String), SymbolRecord>>>,
}

impl SymbolLedger for InMemorySymbolLedger {
    fn record(&self, tenant: TenantId, record: SymbolRecord) -> LedgerFuture<'_, SymbolRecord> {
        let answer = self
            .rows
            .lock()
            .map_err(|_| LedgerError("the ledger's lock is poisoned".to_owned()))
            .and_then(|mut rows| {
                let key = (tenant.to_string(), record.symbol.clone());
                if let Some(held) = rows.get(&key) {
                    return Ok(held.clone());
                }
                if rows.len() >= IN_MEMORY_LIMIT {
                    return Err(LedgerError("the in-memory ledger is full".to_owned()));
                }
                rows.insert(key, record.clone());
                Ok(record)
            });
        Box::pin(async move { answer })
    }

    fn list(&self, tenant: TenantId) -> LedgerFuture<'_, Vec<SymbolRecord>> {
        let tenant = tenant.to_string();
        let answer = self
            .rows
            .lock()
            .map_err(|_| LedgerError("the ledger's lock is poisoned".to_owned()))
            .map(|rows| {
                let mut list: Vec<SymbolRecord> = rows
                    .iter()
                    .filter(|((owner, _), _)| *owner == tenant)
                    .map(|(_, record)| record.clone())
                    .collect();
                list.sort_by(|a, b| {
                    (a.recorded_at_ms, &a.symbol).cmp(&(b.recorded_at_ms, &b.symbol))
                });
                list
            });
        Box::pin(async move { answer })
    }
}

impl SymbolLedger for store_postgres::PostgresEInvoiceSymbols {
    fn record(&self, tenant: TenantId, record: SymbolRecord) -> LedgerFuture<'_, SymbolRecord> {
        Box::pin(async move {
            let row = store_postgres::EInvoiceSymbolRow {
                symbol: record.symbol,
                store_id: record.store_id,
                lease_generation: record.lease_generation,
                source: record.source.as_str().to_owned(),
                recorded_at_ms: record.recorded_at_ms,
            };
            let held = Self::record(self, &tenant.to_string(), &row)
                .await
                .map_err(|error| LedgerError(error.to_string()))?;
            Ok(from_row(held))
        })
    }

    fn list(&self, tenant: TenantId) -> LedgerFuture<'_, Vec<SymbolRecord>> {
        Box::pin(async move {
            let rows = Self::list(self, &tenant.to_string())
                .await
                .map_err(|error| LedgerError(error.to_string()))?;
            Ok(rows.into_iter().map(from_row).collect())
        })
    }
}

fn from_row(row: store_postgres::EInvoiceSymbolRow) -> SymbolRecord {
    SymbolRecord {
        symbol: row.symbol,
        store_id: row.store_id,
        lease_generation: row.lease_generation,
        source: SymbolSource::parse(&row.source),
        recorded_at_ms: row.recorded_at_ms,
    }
}

/// Why no symbol was allocated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocateError {
    /// The store's country has no allocation rule, or no country is published: type it by hand.
    NoRule,
    /// Every index of the year's rule is already in the ledger.
    Exhausted,
    /// The ledger could not be read or written.
    Ledger(LedgerError),
}

/// The most indexes one allocation tries: Vietnam's rule has 676 a year.
pub const MAX_INDEX: u32 = 10_000;

/// Allocates `store` (or, with `None`, the tenant's correction symbol) the first symbol of `year`
/// under `country`'s rule that the tenant's ledger does not hold, and records it. A record lost to a
/// concurrent allocation is skipped, so two allocations never share a symbol.
///
/// # Errors
///
/// [`AllocateError`] as its variants say.
pub async fn allocate(
    ledger: &dyn SymbolLedger,
    country: Option<&dyn CountryModule>,
    tenant: TenantId,
    store: Option<StoreId>,
    lease_generation: Option<i64>,
    year: i16,
    now: Timestamp,
) -> Result<SymbolRecord, AllocateError> {
    let country = country.ok_or(AllocateError::NoRule)?;
    if country.allocate_invoice_symbol(year, 0).is_none() {
        return Err(AllocateError::NoRule);
    }
    let held: BTreeSet<String> = ledger
        .list(tenant)
        .await
        .map_err(AllocateError::Ledger)?
        .into_iter()
        .map(|record| record.symbol)
        .collect();
    for index in 0..MAX_INDEX {
        let Some(symbol) = country.allocate_invoice_symbol(year, index) else {
            break;
        };
        if held.contains(&symbol) {
            continue;
        }
        let wanted = SymbolRecord {
            symbol,
            store_id: store.map(|store| store.to_string()),
            lease_generation,
            source: SymbolSource::Allocated,
            recorded_at_ms: now.as_milliseconds_since_epoch(),
        };
        let recorded = ledger
            .record(tenant, wanted.clone())
            .await
            .map_err(AllocateError::Ledger)?;
        if recorded == wanted {
            return Ok(recorded);
        }
    }
    Err(AllocateError::Exhausted)
}

#[cfg(test)]
mod tests;
