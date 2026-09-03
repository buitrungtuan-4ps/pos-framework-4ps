// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The tax-rate authoring seam (Track M4, [ADR-0074](../../../docs/adr/0074-localization-and-tax.md)).
//!
//! The per-`(tax class × sales channel)` rate an operator authors — the values that were missing while
//! `pos_proto::TaxRateTable` and the edge billing that reads it (ADR-0028) waited for a source. This
//! seam stores the rows and hands them back; a publish (a later slice) assembles them with
//! [`to_table`] into the `tax` config node the edge applies to `EdgeSession::tax_rates`.
//!
//! The seam is a trait so it runs against an in-memory fake in tests and a `store-postgres` table in
//! the cloud, tenant-scoped and RLS-isolated like the rest of the catalog. A save **replaces** the
//! tenant's whole table: the operator edits a small `(class × channel)` grid, so a wholesale set is
//! the honest shape.

use core::future::Future;

use pos_proto::enums::SalesChannel;
use pos_proto::ids::{TaxClassId, TenantId};
use pos_proto::locale::{TaxRate, TaxRateTable};

use crate::version::{UpdateOutcome, Version};

/// One authored rate: a tax class on a sales channel resolves to this rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaxRateEntry {
    /// The class the rate applies to — the id an item's `tax_class_id` references.
    pub tax_class_id: TaxClassId,
    /// The channel the rate applies on (the same item may be taxed differently dine-in vs takeaway).
    pub sales_channel: SalesChannel,
    /// The rate in force.
    pub rate: TaxRate,
}

/// Persists and reads a tenant's per-(tax class × channel) tax rates.
///
/// `list_tax_rates` reads the whole tenant table; `set_tax_rates` replaces it wholesale. Both are
/// tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant like every other cloud table.
///
/// # The collection is the entity
///
/// The console loads the whole class × channel grid, an operator edits one cell, and the screen
/// `PUT`s the whole grid back. That is a read-modify-write with a human thinking in the middle, so
/// two operators editing different cells of the same grid lose one of the edits entirely — the
/// condition [ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md) exists to remove,
/// spanning a browser rather than a request.
///
/// There is no *row* to version here, because a save replaces the whole table, so the version
/// belongs to the collection ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
/// Above this seam it is still just an opaque token: minted by the adapter, echoed by the console,
/// compared for equality, never read into.
pub trait TaxRateStore {
    /// Every authored rate for a tenant, and the version the table was read at.
    ///
    /// The version is `None` for a tenant that has never saved rates — a real, checkable state, and
    /// the one a first save asserts by sending no version at all.
    fn list_tax_rates(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<(Vec<TaxRateEntry>, Option<Version>), TaxRateStoreError>> + Send;

    /// Replaces a tenant's whole tax-rate table with `entries`, **only if it is still at
    /// `expected`** — `None` asserting the tenant has saved no rates yet.
    ///
    /// The check belongs here rather than in the caller because a caller can only compare against
    /// the read it already has, and by then the interleave has happened. This precondition is
    /// evaluated by the store at write time, in the same transaction as the replace.
    fn set_tax_rates(
        &self,
        tenant_id: TenantId,
        entries: &[TaxRateEntry],
        expected: Option<&Version>,
    ) -> impl Future<Output = Result<UpdateOutcome, TaxRateStoreError>> + Send;
}

/// Assembles authored entries into the wire [`TaxRateTable`] the edge reprices from.
///
/// The order of the rows does not matter to `rate_for` (it is a keyed lookup), so this preserves the
/// caller's order — the store returns them class-then-channel sorted, which reads cleanly in a diff.
#[must_use]
pub fn to_table(entries: &[TaxRateEntry]) -> TaxRateTable {
    let mut table = TaxRateTable::new();
    for entry in entries {
        table = table.with(entry.tax_class_id, entry.sales_channel, entry.rate);
    }
    table
}

/// A failure of the tax-rate store itself — the database is unreachable, or a stored value could not
/// be decoded.
#[derive(Debug, thiserror::Error)]
#[error("the tax-rate store failed: {0}")]
pub struct TaxRateStoreError(String);

impl TaxRateStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use pos_proto::enums::SalesChannel;
    use pos_proto::ids::{TaxClassId, TenantId};
    use pos_proto::locale::TaxRate;
    use pos_proto::ulid::Ulid;

    use super::{TaxRateEntry, TaxRateStore, TaxRateStoreError, to_table};
    use crate::version::{UpdateOutcome, Version};

    /// An in-memory `TaxRateStore` for the seam's tests. Tenant-scoped exactly like the real thing;
    /// `set` replaces the tenant's rows and leaves other tenants alone, so a test can prove isolation.
    ///
    /// `versions` mirrors the `catalog_tax_rate_versions` row: one token per tenant, moved by every
    /// applied save. The token's shape is this fake's business, exactly as `xmin` is the adapter's.
    #[derive(Default)]
    struct FakeTaxRates {
        rows: Mutex<Vec<(TenantId, TaxRateEntry)>>,
        versions: Mutex<BTreeMap<TenantId, Version>>,
        minted: Mutex<u64>,
    }

    impl FakeTaxRates {
        fn mint(&self) -> Version {
            let mut minted = self.minted.lock().expect("lock");
            *minted += 1;
            Version::new(format!("v{minted}"))
        }
    }

    impl TaxRateStore for FakeTaxRates {
        async fn list_tax_rates(
            &self,
            tenant_id: TenantId,
        ) -> Result<(Vec<TaxRateEntry>, Option<Version>), TaxRateStoreError> {
            let entries = self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _entry)| *owner == tenant_id)
                .map(|(_owner, entry)| *entry)
                .collect();
            let version = self.versions.lock().expect("lock").get(&tenant_id).cloned();
            Ok((entries, version))
        }

        async fn set_tax_rates(
            &self,
            tenant_id: TenantId,
            entries: &[TaxRateEntry],
            expected: Option<&Version>,
        ) -> Result<UpdateOutcome, TaxRateStoreError> {
            let version = self.mint();
            let mut versions = self.versions.lock().expect("lock");
            // The same four answers `store-postgres` gives, so a test passing here is not passing on
            // a laxer store.
            let refusal = match (versions.get(&tenant_id), expected) {
                (None, None) => None,
                (None, Some(_)) => Some(UpdateOutcome::NotFound),
                (Some(_), None) => Some(UpdateOutcome::VersionMismatch),
                (Some(stored), Some(expected)) => {
                    (stored != expected).then_some(UpdateOutcome::VersionMismatch)
                }
            };
            if let Some(refusal) = refusal {
                return Ok(refusal);
            }
            versions.insert(tenant_id, version.clone());
            let mut rows = self.rows.lock().expect("lock");
            rows.retain(|(owner, _entry)| *owner != tenant_id);
            rows.extend(entries.iter().map(|entry| (tenant_id, *entry)));
            Ok(UpdateOutcome::Updated(version))
        }
    }

    /// The version an applied save left the table at, or a panic naming what came back instead.
    fn applied(outcome: UpdateOutcome) -> Version {
        match outcome {
            UpdateOutcome::Updated(version) => version,
            other @ (UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound) => {
                panic!("expected the save to apply, got {other:?}")
            }
        }
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn other_tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(2))
    }

    fn tax_class() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(10))
    }

    #[tokio::test]
    async fn set_replaces_the_table_and_stays_tenant_scoped() {
        let store = FakeTaxRates::default();
        let standard = TaxRateEntry {
            tax_class_id: tax_class(),
            sales_channel: SalesChannel::DineIn,
            rate: TaxRate::from_percent(10),
        };
        let takeaway = TaxRateEntry {
            tax_class_id: tax_class(),
            sales_channel: SalesChannel::Takeaway,
            rate: TaxRate::from_percent(8),
        };

        // A neighbour's table must survive our writes — including its version, which is per tenant.
        applied(
            store
                .set_tax_rates(
                    other_tenant(),
                    &[TaxRateEntry {
                        tax_class_id: tax_class(),
                        sales_channel: SalesChannel::DineIn,
                        rate: TaxRate::from_percent(5),
                    }],
                    None,
                )
                .await
                .expect("set neighbour"),
        );

        applied(
            store
                .set_tax_rates(tenant(), &[standard, takeaway], None)
                .await
                .expect("set ours"),
        );
        let (listed, version) = store.list_tax_rates(tenant()).await.expect("list ours");
        assert_eq!(listed.len(), 2);
        let version = version.expect("a saved table has a version");

        // A second set at that version replaces rather than appends.
        applied(
            store
                .set_tax_rates(tenant(), &[standard], Some(&version))
                .await
                .expect("replace"),
        );
        let (replaced, moved) = store.list_tax_rates(tenant()).await.expect("list replaced");
        assert_eq!(replaced, vec![standard]);
        assert_ne!(
            moved.expect("still versioned"),
            version,
            "an applied save must move the version, or the next write would be unguarded"
        );

        // The neighbour is untouched.
        let (neighbour, _) = store
            .list_tax_rates(other_tenant())
            .await
            .expect("list neighbour");
        assert_eq!(neighbour.len(), 1);
    }

    /// The three ways a save is refused, and the proof that none of them changed the table.
    #[tokio::test]
    async fn a_save_against_a_version_the_table_no_longer_holds_is_refused() {
        let store = FakeTaxRates::default();
        let standard = TaxRateEntry {
            tax_class_id: tax_class(),
            sales_channel: SalesChannel::DineIn,
            rate: TaxRate::from_percent(10),
        };
        let clobber = TaxRateEntry {
            tax_class_id: tax_class(),
            sales_channel: SalesChannel::DineIn,
            rate: TaxRate::from_percent(99),
        };

        // Naming a version for a tenant that has never saved is a `NotFound`, not a conflict: there
        // is no other edit to have lost, so telling the operator to reload would be a lie.
        assert_eq!(
            store
                .set_tax_rates(tenant(), &[clobber], Some(&Version::new("v0")))
                .await
                .expect("the comparison must not raise"),
            UpdateOutcome::NotFound
        );

        let first = applied(
            store
                .set_tax_rates(tenant(), &[standard], None)
                .await
                .expect("the first save"),
        );

        // Claiming "nothing saved yet" about a table that has been saved: refused, not upserted.
        assert_eq!(
            store
                .set_tax_rates(tenant(), &[clobber], None)
                .await
                .expect("the create path"),
            UpdateOutcome::VersionMismatch
        );

        // Replaying a version the table has moved past is the lost update this exists to refuse.
        applied(
            store
                .set_tax_rates(tenant(), &[standard], Some(&first))
                .await
                .expect("a second save"),
        );
        assert_eq!(
            store
                .set_tax_rates(tenant(), &[clobber], Some(&first))
                .await
                .expect("the stale save"),
            UpdateOutcome::VersionMismatch
        );

        let (rows, _) = store.list_tax_rates(tenant()).await.expect("list");
        assert_eq!(rows, vec![standard], "no refused save changed the table");
    }

    #[test]
    fn to_table_resolves_each_class_and_channel() {
        let standard = TaxRateEntry {
            tax_class_id: tax_class(),
            sales_channel: SalesChannel::DineIn,
            rate: TaxRate::from_percent(10),
        };
        let takeaway = TaxRateEntry {
            tax_class_id: tax_class(),
            sales_channel: SalesChannel::Takeaway,
            rate: TaxRate::from_percent(8),
        };
        let table = to_table(&[standard, takeaway]);
        assert_eq!(
            table.rate_for(tax_class(), SalesChannel::DineIn),
            Some(TaxRate::from_percent(10))
        );
        assert_eq!(
            table.rate_for(tax_class(), SalesChannel::Takeaway),
            Some(TaxRate::from_percent(8))
        );
        // A channel nobody priced stays a visible None, never a silent zero.
        assert_eq!(table.rate_for(tax_class(), SalesChannel::Delivery), None);
    }
}
