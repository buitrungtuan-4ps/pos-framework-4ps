// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a fee write resolves against: where the tenant's stores sit, and the catalog they price.

use std::collections::BTreeSet;

use axum::response::Response;
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::fees::PublishedFee;
use pos_proto::ids::{MenuItemId, StoreId, TaxClassId, TenantId};
use pos_proto::{ErrorStatus, WireEnum};

use crate::catalog::{CatalogStore, ItemCategoryId};
use crate::fees::{CategoryItems, FeePlacement, FeeScope};
use crate::http::{
    FieldRefusal, catalog_error_response, enum_refusal, not_found, registry_error_response,
};
use crate::registry::{BrandId, RegistryStore, StoreRecord};
use crate::version::records;

/// Where a tenant's stores sit for their fees — each store and its brand — read once per request,
/// so one write resolves every store it reaches against the same picture.
pub(super) struct FeeLayout {
    pub(super) tenant_id: TenantId,
    pub(super) stores: Vec<StoreRecord>,
    brand_ids: Vec<BrandId>,
}

impl FeeLayout {
    /// Reads the tenant's stores and brands.
    pub(in crate::http) async fn load<Rg: RegistryStore>(
        registry: &Rg,
        tenant_id: TenantId,
    ) -> Result<Self, Response> {
        let stores = records(
            registry
                .list_stores(tenant_id)
                .await
                .map_err(|error| registry_error_response(&error))?,
        );
        let brand_ids = registry
            .list_brands(tenant_id)
            .await
            .map_err(|error| registry_error_response(&error))?
            .into_iter()
            .map(|brand| brand.record.brand_id)
            .collect();
        Ok(Self {
            tenant_id,
            stores,
            brand_ids,
        })
    }

    /// Where one store sits, or `None` if the tenant has no such store.
    pub(super) fn placement(&self, store_id: StoreId) -> Option<FeePlacement> {
        let store = self
            .stores
            .iter()
            .find(|store| store.store_id == store_id)?;
        Some(FeePlacement {
            tenant_id: self.tenant_id.to_string(),
            brand_id: store.brand_id.map(|brand| brand.to_string()),
            store_id: store_id.to_string(),
        })
    }

    /// The canonical id a rule at `scope` is written for, refusing one the tenant does not have.
    #[expect(
        clippy::result_large_err,
        reason = "the Err is an axum Response by design — it is the refusal the caller returns"
    )]
    pub(super) fn scope_target(&self, scope: FeeScope, scope_id: &str) -> Result<String, Response> {
        let id = match parse_ulid_fields([("scope_id", scope_id)]) {
            Ok([id]) => id,
            Err(refusal) => return Err(refusal),
        };
        match scope {
            FeeScope::Tenant if id == self.tenant_id.as_ulid() => Ok(id.to_string()),
            FeeScope::Tenant => Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a tenant-scope rule is written for the tenant itself",
                &[("scope_id", "MUST_BE_THE_TENANT")],
            )),
            FeeScope::Brand if self.brand_ids.contains(&BrandId::new(id)) => Ok(id.to_string()),
            FeeScope::Brand => Err(not_found("brand")),
            FeeScope::Store if self.placement(StoreId::new(id)).is_some() => Ok(id.to_string()),
            FeeScope::Store => Err(not_found("store")),
            FeeScope::Unspecified => Err(enum_refusal(
                "scope",
                FeeScope::WRITABLE.iter().map(|scope| scope.as_wire()),
            )),
        }
    }

    /// The stores a rule at `scope` reaches, in the registry's order.
    pub(super) fn reached_by(&self, scope: FeeScope, scope_id: &str) -> Vec<StoreId> {
        let tenant_id = self.tenant_id.to_string();
        self.stores
            .iter()
            .filter(|store| match scope {
                FeeScope::Tenant => scope_id == tenant_id,
                FeeScope::Brand => store
                    .brand_id
                    .is_some_and(|brand| brand.to_string() == scope_id),
                FeeScope::Store => store.store_id.to_string() == scope_id,
                FeeScope::Unspecified => false,
            })
            .map(|store| store.store_id)
            .collect()
    }
}

/// What a fee rule is checked and compiled against in the tenant's catalog.
pub(super) struct FeeCatalog {
    item_ids: BTreeSet<MenuItemId>,
    category_ids: BTreeSet<ItemCategoryId>,
    tax_class_ids: BTreeSet<TaxClassId>,
    pub(super) categories: CategoryItems,
}

impl FeeCatalog {
    /// Reads the tenant's items, item categories and tax classes.
    pub(in crate::http) async fn load<Cat: CatalogStore>(
        catalog: &Cat,
        tenant_id: TenantId,
    ) -> Result<Self, Response> {
        let items = records(
            catalog
                .list_items(tenant_id)
                .await
                .map_err(|error| catalog_error_response(&error))?,
        );
        let category_ids = catalog
            .list_item_categories(tenant_id)
            .await
            .map_err(|error| catalog_error_response(&error))?
            .into_iter()
            .map(|category| category.record.item_category_id)
            .collect();
        let tax_class_ids = catalog
            .list_tax_classes(tenant_id)
            .await
            .map_err(|error| catalog_error_response(&error))?
            .into_iter()
            .map(|tax_class| tax_class.record.tax_class_id)
            .collect();
        Ok(Self {
            item_ids: items.iter().map(|item| item.menu_item_id).collect(),
            category_ids,
            tax_class_ids,
            categories: CategoryItems::from_catalog(&items),
        })
    }

    /// Refuses a rule that names an item, an item category or a tax class the catalog does not
    /// have: a list naming an item nobody can sell is a typing mistake, and a tax class no store
    /// can have a rate for fails every bill the fee applies to.
    #[expect(
        clippy::result_large_err,
        reason = "the Err is an axum Response by design — it is the refusal the caller returns"
    )]
    pub(super) fn check_references(
        &self,
        rule: &PublishedFee,
        item_category_ids: &[ItemCategoryId],
    ) -> Result<(), Response> {
        let refusal = |message: &'static str, field: &'static str| {
            Err(FieldRefusal::new(message, field, "UNKNOWN_REFERENCE").into_response())
        };
        if rule
            .menu_item_ids
            .iter()
            .any(|id| !self.item_ids.contains(id))
        {
            return refusal(
                "the fee names an item the catalog does not have",
                "rule.menu_item_ids",
            );
        }
        if item_category_ids
            .iter()
            .any(|id| !self.category_ids.contains(id))
        {
            return refusal(
                "the fee names an item category the catalog does not have",
                "rule.item_category_ids",
            );
        }
        if rule
            .tax_class_id
            .is_some_and(|id| !self.tax_class_ids.contains(&id))
        {
            return refusal(
                "the fee names a tax class the catalog does not have",
                "rule.tax_class_id",
            );
        }
        Ok(())
    }
}
