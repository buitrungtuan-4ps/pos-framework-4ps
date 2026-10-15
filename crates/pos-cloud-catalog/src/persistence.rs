// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The catalog's persistence seams over their `store-postgres` tables: the `CatalogStore`,
//! `FeeRuleStore`, `MediaStore`, `TaxRateStore` and `TranslationStore` impls, moved out of
//! `pos-cloud`'s `persistence.rs` with their traits. A trait can be implemented for another crate's
//! type only in the trait's crate, so the glue lives here; all SQL stays in the adapter. The
//! catalog's, the largest, is in `catalog`: its row conversions there, and the impl in `store`.

use store_postgres::{FeeRuleRow, FeeRuleSlot as FeeRuleRowSlot, FeeRuleWrite, PostgresFeeRules};
use store_postgres::{
    MediaAssetRow, PostgresMedia, PostgresTaxRates, PostgresTranslations, TaxRateRow,
};

use pos_proto::enums::SalesChannel;
use pos_proto::ids::{TaxClassId, TenantId};
use pos_proto::locale::{TaxComponent, TaxRate};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum;

use crate::fees::{FeeRule, FeeRuleSlot, FeeRuleStore, FeeRuleStoreError, FeeScope};
use crate::media::{MediaId, MediaStore, MediaStoreError, MediaSummary, NewMediaAsset, Rendition};
use crate::paging::{Page, PageRequest};
use crate::tax::{TaxRateEntry, TaxRateStore, TaxRateStoreError};
use crate::translations::{TranslationGrid, TranslationStore, TranslationStoreError};
use crate::version::{UpdateOutcome, Version, Versioned};
use pos_cloud_config::persistence::update_outcome;

mod catalog;

impl TranslationStore for PostgresTranslations {
    async fn load(
        &self,
        tenant: TenantId,
    ) -> Result<Option<Versioned<TranslationGrid>>, TranslationStoreError> {
        let row = self
            .load_grid(&tenant.to_string())
            .await
            .map_err(|error| TranslationStoreError::new(error.to_string()))?;
        match row {
            Some((text, version)) => serde_json::from_str(&text)
                .map(|grid| Some(Versioned::new(grid, Version::new(version))))
                .map_err(|error| {
                    TranslationStoreError::new(format!("decoding the stored grid failed: {error}"))
                }),
            None => Ok(None),
        }
    }

    async fn save(
        &self,
        tenant: TenantId,
        grid: &TranslationGrid,
        expected: Option<&Version>,
    ) -> Result<UpdateOutcome, TranslationStoreError> {
        let json = serde_json::to_string(grid).map_err(|error| {
            TranslationStoreError::new(format!("encoding the grid failed: {error}"))
        })?;
        self.save_grid(&tenant.to_string(), &json, expected.map(Version::as_str))
            .await
            .map(update_outcome)
            .map_err(|error| TranslationStoreError::new(error.to_string()))
    }
}

// --- Tax rates (ADR-0074, Track M4) -----------------------------------------------------------

/// Converts one stored `catalog_tax_rates` row into a [`TaxRateEntry`], failing loudly on a malformed
/// class id (store corruption — this cloud wrote a well-formed ULID) but **skipping** an unrecognised
/// channel token: the seam speaks the closed `SalesChannel`, and a token from a future vocabulary is
/// dropped from the authoring view rather than failing the whole read (`None` from `filter_map`).
fn tax_rate_entry(row: &TaxRateRow) -> Result<Option<TaxRateEntry>, TaxRateStoreError> {
    let tax_class_id = row
        .tax_class_id
        .parse::<Ulid>()
        .map(TaxClassId::new)
        .map_err(|_ignored| {
            TaxRateStoreError::new(format!(
                "a tax-rate class id is not a ULID: {}",
                row.tax_class_id
            ))
        })?;
    let Some(sales_channel) = SalesChannel::from_wire(&row.sales_channel) else {
        return Ok(None);
    };
    let rate = TaxRate::from_basis_points(u32::try_from(row.rate_bps).unwrap_or(0));
    // A breakdown that does not decode is dropped rather than failing the read (ADR-0104): the
    // components are how the tax is *printed*, and `rate` — which does decode — is what the guest
    // pays, so a corrupt list can cost an invoice its lines and can never cost a bill its total.
    // The read still says so, because a silent drop on a legal document is not a thing to be quiet
    // about.
    let components: Vec<TaxComponent> = match serde_json::from_str(&row.components_json) {
        Ok(components) => components,
        Err(error) => {
            tracing::warn!(
                tax_class_id = %row.tax_class_id,
                sales_channel = %row.sales_channel,
                %error,
                "a stored tax-rate breakdown did not decode; the row keeps its rate and prints as                  one line"
            );
            Vec::new()
        }
    };
    Ok(Some(TaxRateEntry {
        tax_class_id,
        sales_channel,
        rate,
        components,
    }))
}

impl TaxRateStore for PostgresTaxRates {
    async fn list_tax_rates(
        &self,
        tenant_id: TenantId,
    ) -> Result<(Vec<TaxRateEntry>, Option<Version>), TaxRateStoreError> {
        let (rows, version) = self
            .fetch(&tenant_id.to_string())
            .await
            .map_err(|error| TaxRateStoreError::new(error.to_string()))?;
        let entries: Vec<TaxRateEntry> = rows
            .iter()
            .filter_map(|row| tax_rate_entry(row).transpose())
            .collect::<Result<_, _>>()?;
        Ok((entries, version.map(Version::new)))
    }

    async fn set_tax_rates(
        &self,
        tenant_id: TenantId,
        entries: &[TaxRateEntry],
        expected: Option<&Version>,
    ) -> Result<UpdateOutcome, TaxRateStoreError> {
        let rows: Vec<TaxRateRow> = entries
            .iter()
            .map(|entry| TaxRateRow {
                tax_class_id: entry.tax_class_id.to_string(),
                sales_channel: entry.sales_channel.as_wire().to_string(),
                rate_bps: i32::try_from(entry.rate.basis_points()).unwrap_or(i32::MAX),
                // A list of name/rate pairs cannot fail to serialise; `[]` on the impossible branch
                // rather than an `expect`, because the backbone forbids the panic and "no breakdown"
                // is the correct fallback anyway.
                components_json: serde_json::to_string(&entry.components)
                    .unwrap_or_else(|_ignored| "[]".to_owned()),
            })
            .collect();
        self.replace(&tenant_id.to_string(), &rows, expected.map(Version::as_str))
            .await
            .map(update_outcome)
            .map_err(|error| TaxRateStoreError::new(error.to_string()))
    }
}

impl FeeRuleStore for PostgresFeeRules {
    async fn list(&self, tenant_id: TenantId) -> Result<Vec<FeeRule>, FeeRuleStoreError> {
        let rows = self
            .fetch(&tenant_id.to_string())
            .await
            .map_err(|error| FeeRuleStoreError::new(error.to_string()))?;
        rows.iter().map(fee_rule_from_row).collect()
    }

    async fn put(&self, tenant_id: TenantId, rule: &FeeRule) -> Result<(), FeeRuleStoreError> {
        rule.check_storable()?;
        let doc_json = crate::fees::authored_json(rule)
            .map(|document| document.to_string())
            .map_err(|error| {
                FeeRuleStoreError::new(format!("could not serialize a fee rule: {error}"))
            })?;
        let fee_id = rule.rule.fee_id.to_string();
        let slot = FeeRuleRowSlot {
            scope: rule.scope.as_wire(),
            scope_id: &rule.scope_id,
            fee_id: &fee_id,
        };
        let write = FeeRuleWrite {
            doc_json: &doc_json,
            update_time_ms: rule.update_time.as_milliseconds_since_epoch(),
            updated_by: &rule.updated_by,
        };
        self.upsert(&tenant_id.to_string(), slot, write)
            .await
            .map_err(|error| FeeRuleStoreError::new(error.to_string()))
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        slot: FeeRuleSlot<'_>,
    ) -> Result<bool, FeeRuleStoreError> {
        let fee_id = slot.fee_id.to_string();
        let row_slot = FeeRuleRowSlot {
            scope: slot.scope.as_wire(),
            scope_id: slot.scope_id,
            fee_id: &fee_id,
        };
        PostgresFeeRules::delete(self, &tenant_id.to_string(), row_slot)
            .await
            .map_err(|error| FeeRuleStoreError::new(error.to_string()))
    }
}

/// One stored fee rule, read back into the cloud's shape. The rule is parsed from the row's JSON
/// text ([`crate::fees::rule_from_authored`]: a money amount reads only from text). A row whose
/// scope, rule or time does not read back, or whose rule names another fee than the row's key, is
/// an error rather than a rule quietly dropped from a store's list.
fn fee_rule_from_row(row: &FeeRuleRow) -> Result<FeeRule, FeeRuleStoreError> {
    let scope = FeeScope::from_wire(&row.scope)
        .filter(|scope| FeeScope::WRITABLE.contains(scope))
        .ok_or_else(|| {
            FeeRuleStoreError::new(format!(
                "a stored fee rule has an unknown scope `{}`",
                row.scope
            ))
        })?;
    let (rule, item_category_ids) =
        crate::fees::rule_from_authored(&row.doc_json).map_err(|error| {
            FeeRuleStoreError::new(format!("a stored fee rule could not be decoded: {error}"))
        })?;
    if rule.fee_id.to_string() != row.fee_id {
        return Err(FeeRuleStoreError::new(
            "a stored fee rule's id is not the id it is kept under",
        ));
    }
    let update_time =
        Timestamp::from_milliseconds_since_epoch(row.update_time_ms).map_err(|error| {
            FeeRuleStoreError::new(format!(
                "a stored fee rule has an invalid update time: {error}"
            ))
        })?;
    Ok(FeeRule {
        scope,
        scope_id: row.scope_id.clone(),
        rule,
        item_category_ids,
        update_time,
        updated_by: row.updated_by.clone(),
    })
}

// --- Media renditions (ADR-0075) --------------------------------------------------------------

/// Converts one stored `media_assets` summary row into the seam's [`MediaSummary`], failing loudly on
/// a media id that is not a ULID — that is store corruption (this cloud minted it), not an absence.
fn media_summary(row: MediaAssetRow) -> Result<MediaSummary, MediaStoreError> {
    let media_id = row
        .media_id
        .parse::<Ulid>()
        .map(MediaId::new)
        .map_err(|_ignored| {
            MediaStoreError::new(format!("a media id is not a ULID: {}", row.media_id))
        })?;
    Ok(MediaSummary {
        media_id,
        content_type: row.content_type,
        detail_bytes: usize::try_from(row.detail_bytes).unwrap_or(0),
        created_at_ms: row.created_at_ms,
    })
}

impl MediaStore for PostgresMedia {
    async fn put(&self, asset: &NewMediaAsset) -> Result<(), MediaStoreError> {
        let detail_bytes = i32::try_from(asset.detail.len()).unwrap_or(i32::MAX);
        self.insert(
            &asset.media_id.to_string(),
            &asset.tenant_id.to_string(),
            &asset.content_type,
            &asset.thumbnail,
            &asset.detail,
            detail_bytes,
        )
        .await
        .map_err(|error| MediaStoreError::new(error.to_string()))
    }

    async fn get(
        &self,
        tenant_id: TenantId,
        media_id: MediaId,
        rendition: Rendition,
    ) -> Result<Option<Vec<u8>>, MediaStoreError> {
        self.fetch_rendition(
            &tenant_id.to_string(),
            &media_id.to_string(),
            matches!(rendition, Rendition::Detail),
        )
        .await
        .map_err(|error| MediaStoreError::new(error.to_string()))
    }

    async fn list(&self, tenant_id: TenantId) -> Result<Vec<MediaSummary>, MediaStoreError> {
        let rows = self
            .fetch_summaries(&tenant_id.to_string())
            .await
            .map_err(|error| MediaStoreError::new(error.to_string()))?;
        rows.into_iter().map(media_summary).collect()
    }

    async fn list_page(
        &self,
        tenant_id: TenantId,
        page: PageRequest,
    ) -> Result<Page<MediaSummary>, MediaStoreError> {
        // `PageRequest` already range-checked these, so widening `u32` into the `i64` the SQL binds
        // cannot lose or sign-flip anything — which is why the adapter takes bare integers.
        let (rows, total) = self
            .fetch_summaries_page(
                &tenant_id.to_string(),
                i64::from(page.limit()),
                i64::from(page.offset()),
            )
            .await
            .map_err(|error| MediaStoreError::new(error.to_string()))?;
        let items = rows
            .into_iter()
            .map(media_summary)
            .collect::<Result<Vec<MediaSummary>, MediaStoreError>>()?;
        Ok(Page::new(items, u32::try_from(total).unwrap_or(u32::MAX)))
    }

    async fn delete(
        &self,
        tenant_id: TenantId,
        media_id: MediaId,
    ) -> Result<bool, MediaStoreError> {
        self.remove(&tenant_id.to_string(), &media_id.to_string())
            .await
            .map_err(|error| MediaStoreError::new(error.to_string()))
    }
}
