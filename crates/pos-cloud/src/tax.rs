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
pub trait TaxRateStore {
    /// Every authored rate for a tenant.
    fn list_tax_rates(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<TaxRateEntry>, TaxRateStoreError>> + Send;

    /// Replaces a tenant's whole tax-rate table with `entries`.
    fn set_tax_rates(
        &self,
        tenant_id: TenantId,
        entries: &[TaxRateEntry],
    ) -> impl Future<Output = Result<(), TaxRateStoreError>> + Send;
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
    use std::sync::Mutex;

    use pos_proto::enums::SalesChannel;
    use pos_proto::ids::{TaxClassId, TenantId};
    use pos_proto::locale::TaxRate;
    use pos_proto::ulid::Ulid;

    use super::{TaxRateEntry, TaxRateStore, TaxRateStoreError, to_table};

    /// An in-memory `TaxRateStore` for the seam's tests. Tenant-scoped exactly like the real thing;
    /// `set` replaces the tenant's rows and leaves other tenants alone, so a test can prove isolation.
    #[derive(Default)]
    struct FakeTaxRates {
        rows: Mutex<Vec<(TenantId, TaxRateEntry)>>,
    }

    impl TaxRateStore for FakeTaxRates {
        async fn list_tax_rates(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<TaxRateEntry>, TaxRateStoreError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _entry)| *owner == tenant_id)
                .map(|(_owner, entry)| *entry)
                .collect())
        }

        async fn set_tax_rates(
            &self,
            tenant_id: TenantId,
            entries: &[TaxRateEntry],
        ) -> Result<(), TaxRateStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            rows.retain(|(owner, _entry)| *owner != tenant_id);
            rows.extend(entries.iter().map(|entry| (tenant_id, *entry)));
            Ok(())
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

        // A neighbour's table must survive our writes.
        store
            .set_tax_rates(
                other_tenant(),
                &[TaxRateEntry {
                    tax_class_id: tax_class(),
                    sales_channel: SalesChannel::DineIn,
                    rate: TaxRate::from_percent(5),
                }],
            )
            .await
            .expect("set neighbour");

        store
            .set_tax_rates(tenant(), &[standard, takeaway])
            .await
            .expect("set ours");
        let listed = store.list_tax_rates(tenant()).await.expect("list ours");
        assert_eq!(listed.len(), 2);

        // A second set replaces rather than appends.
        store
            .set_tax_rates(tenant(), &[standard])
            .await
            .expect("replace");
        let replaced = store.list_tax_rates(tenant()).await.expect("list replaced");
        assert_eq!(replaced, vec![standard]);

        // The neighbour is untouched.
        let neighbour = store
            .list_tax_rates(other_tenant())
            .await
            .expect("list neighbour");
        assert_eq!(neighbour.len(), 1);
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
