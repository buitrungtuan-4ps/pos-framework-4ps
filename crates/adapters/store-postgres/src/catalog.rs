// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud catalog authoring model over PostgreSQL (Phase 2a, [ADR-0066](../../../docs/adr/0066-cloud-catalog.md)).
//!
//! Items, menus (with an inheritance edge) and menu placements (an item in a menu with its per-channel
//! prices). This adapter keeps only the SQL and returns plain rows; `pos-cloud` implements its
//! `CatalogStore` seam over this type and compiles the model into a `MenuBook`. Tenant scoping is an
//! explicit `WHERE tenant_id = $1` filter (the cloud connects as the trusted pool owner, which bypasses
//! RLS; the migration's policy is the belt-and-suspenders second line), exactly as the registry does.
//! Its methods use distinct verbs from the seam (`insert`/`fetch`/`set`/`upsert`/`delete`) so the seam
//! impl calls the SQL and never itself.
//!
//! `prices` is a `jsonb` column read and written as one opaque document via the `text::jsonb` cast the
//! config-tree adapter uses; the `pos-cloud` seam serialises the price list into it and back.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// An item as listed — the product master.
#[derive(Clone, Debug)]
pub struct CatalogItemRow {
    /// The item id (a ULID string), shared with the compiled menu entry.
    pub menu_item_id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The human name.
    pub name: String,
    /// The tax class id (a ULID string).
    pub tax_class_id: String,
    /// The operational category id (a ULID string), or `None` if unclassified.
    pub item_category_id: Option<String>,
    /// The operational sub-category id (a ULID string), or `None`.
    pub item_subcategory_id: Option<String>,
    /// `active` or `archived`.
    pub status: String,
}

/// An item category or sub-category as listed. A sub-category carries its parent category id; a
/// top-level category leaves `parent_id` `None`. One row type serves both tables.
#[derive(Clone, Debug)]
pub struct CatalogTaxonomyRow {
    /// The category or sub-category id (a ULID string).
    pub id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The parent category id for a sub-category, else `None`.
    pub parent_id: Option<String>,
    /// The human name.
    pub name: String,
    /// `active` or `archived`.
    pub status: String,
}

/// A tax class as listed — a named bucket an item belongs to.
#[derive(Clone, Debug)]
pub struct CatalogTaxClassRow {
    /// The tax-class id (a ULID string), the id an item's `tax_class_id` references.
    pub tax_class_id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The human name.
    pub name: String,
    /// `active` or `archived`.
    pub status: String,
}

/// A menu as listed — a named set that may inherit from a parent.
#[derive(Clone, Debug)]
pub struct CatalogMenuRow {
    /// The menu id (a ULID string).
    pub menu_id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The human name.
    pub name: String,
    /// The parent menu id, or `None` for a root menu.
    pub parent_menu_id: Option<String>,
    /// `active` or `archived`.
    pub status: String,
}

/// A placement as listed — an item in a menu, with its per-channel prices as a JSON document.
#[derive(Clone, Debug)]
pub struct CatalogPlacementRow {
    /// The owning tenant.
    pub tenant_id: String,
    /// The menu this placement belongs to.
    pub menu_id: String,
    /// The item placed.
    pub menu_item_id: String,
    /// The per-channel prices, as the JSON text stored in the `jsonb` column.
    pub prices_json: String,
    /// Whether the item is for sale in this menu right now (the published-availability floor).
    pub available: bool,
}

/// The catalog authoring store over a shared pool. Built by
/// [`PostgresStore::catalog`](crate::PostgresStore::catalog).
#[derive(Clone, Debug)]
pub struct PostgresCatalog {
    pool: Pool,
}

impl PostgresCatalog {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    // --- items (tenant-scoped) ---

    /// Inserts an item.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_item(
        &self,
        menu_item_id: &str,
        tenant_id: &str,
        name: &str,
        tax_class_id: &str,
        item_category_id: Option<&str>,
        item_subcategory_id: Option<&str>,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_items \
                 (menu_item_id, tenant_id, name, tax_class_id, item_category_id, item_subcategory_id) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &menu_item_id,
                    &tenant_id,
                    &name,
                    &tax_class_id,
                    &item_category_id,
                    &item_subcategory_id,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's items, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_items(&self, tenant_id: &str) -> Result<Vec<CatalogItemRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT menu_item_id, tenant_id, name, tax_class_id, item_category_id, \
                 item_subcategory_id, status FROM catalog_items \
                 WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CatalogItemRow {
                menu_item_id: row.get(0),
                tenant_id: row.get(1),
                name: row.get(2),
                tax_class_id: row.get(3),
                item_category_id: row.get(4),
                item_subcategory_id: row.get(5),
                status: row.get(6),
            })
            .collect())
    }

    /// Renames an item, sets its tax class and status, within its tenant. Returns whether a row
    /// changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_item(
        &self,
        tenant_id: &str,
        menu_item_id: &str,
        name: &str,
        tax_class_id: &str,
        item_category_id: Option<&str>,
        item_subcategory_id: Option<&str>,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_items SET name = $3, tax_class_id = $4, item_category_id = $5, \
                 item_subcategory_id = $6, status = $7, updated_at = now() \
                 WHERE tenant_id = $1 AND menu_item_id = $2",
                &[
                    &tenant_id,
                    &menu_item_id,
                    &name,
                    &tax_class_id,
                    &item_category_id,
                    &item_subcategory_id,
                    &status,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    // --- tax classes (tenant-scoped) ---

    /// Inserts a tax class.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_tax_class(
        &self,
        tax_class_id: &str,
        tenant_id: &str,
        name: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_tax_classes (tax_class_id, tenant_id, name) \
                 VALUES ($1, $2, $3)",
                &[&tax_class_id, &tenant_id, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's tax classes, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_tax_classes(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<CatalogTaxClassRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT tax_class_id, tenant_id, name, status FROM catalog_tax_classes \
                 WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CatalogTaxClassRow {
                tax_class_id: row.get(0),
                tenant_id: row.get(1),
                name: row.get(2),
                status: row.get(3),
            })
            .collect())
    }

    /// Renames a tax class and sets its status, within its tenant. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_tax_class(
        &self,
        tenant_id: &str,
        tax_class_id: &str,
        name: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_tax_classes SET name = $3, status = $4, updated_at = now() \
                 WHERE tenant_id = $1 AND tax_class_id = $2",
                &[&tenant_id, &tax_class_id, &name, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    // --- item taxonomy: categories and sub-categories (tenant-scoped) ---

    /// Inserts an item category.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_item_category(
        &self,
        item_category_id: &str,
        tenant_id: &str,
        name: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_item_categories (item_category_id, tenant_id, name) \
                 VALUES ($1, $2, $3)",
                &[&item_category_id, &tenant_id, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's item categories, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_item_categories(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<CatalogTaxonomyRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT item_category_id, tenant_id, name, status FROM catalog_item_categories \
                 WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CatalogTaxonomyRow {
                id: row.get(0),
                tenant_id: row.get(1),
                parent_id: None,
                name: row.get(2),
                status: row.get(3),
            })
            .collect())
    }

    /// Renames an item category and sets its status, within its tenant. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_item_category(
        &self,
        tenant_id: &str,
        item_category_id: &str,
        name: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_item_categories SET name = $3, status = $4, updated_at = now() \
                 WHERE tenant_id = $1 AND item_category_id = $2",
                &[&tenant_id, &item_category_id, &name, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    /// Inserts an item sub-category under a parent category.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_item_subcategory(
        &self,
        item_subcategory_id: &str,
        tenant_id: &str,
        item_category_id: &str,
        name: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_item_subcategories \
                 (item_subcategory_id, tenant_id, item_category_id, name) VALUES ($1, $2, $3, $4)",
                &[&item_subcategory_id, &tenant_id, &item_category_id, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's item sub-categories, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_item_subcategories(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<CatalogTaxonomyRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT item_subcategory_id, tenant_id, item_category_id, name, status \
                 FROM catalog_item_subcategories WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CatalogTaxonomyRow {
                id: row.get(0),
                tenant_id: row.get(1),
                parent_id: Some(row.get(2)),
                name: row.get(3),
                status: row.get(4),
            })
            .collect())
    }

    /// Renames an item sub-category, (re)parents it and sets its status. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_item_subcategory(
        &self,
        tenant_id: &str,
        item_subcategory_id: &str,
        item_category_id: &str,
        name: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_item_subcategories SET item_category_id = $3, name = $4, \
                 status = $5, updated_at = now() \
                 WHERE tenant_id = $1 AND item_subcategory_id = $2",
                &[
                    &tenant_id,
                    &item_subcategory_id,
                    &item_category_id,
                    &name,
                    &status,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    // --- menus (tenant-scoped) ---

    /// Inserts a menu, with an optional parent.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_menu(
        &self,
        menu_id: &str,
        tenant_id: &str,
        name: &str,
        parent_menu_id: Option<&str>,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_menus (menu_id, tenant_id, name, parent_menu_id) \
                 VALUES ($1, $2, $3, $4)",
                &[&menu_id, &tenant_id, &name, &parent_menu_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's menus, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_menus(&self, tenant_id: &str) -> Result<Vec<CatalogMenuRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT menu_id, tenant_id, name, parent_menu_id, status FROM catalog_menus \
                 WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CatalogMenuRow {
                menu_id: row.get(0),
                tenant_id: row.get(1),
                name: row.get(2),
                parent_menu_id: row.get(3),
                status: row.get(4),
            })
            .collect())
    }

    /// Renames a menu, (re)sets its parent and status, within its tenant. Returns whether a row
    /// changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_menu(
        &self,
        tenant_id: &str,
        menu_id: &str,
        name: &str,
        parent_menu_id: Option<&str>,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_menus SET name = $3, parent_menu_id = $4, status = $5, updated_at = now() \
                 WHERE tenant_id = $1 AND menu_id = $2",
                &[&tenant_id, &menu_id, &name, &parent_menu_id, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    // --- placements (tenant-scoped, keyed by (menu_id, menu_item_id)) ---

    /// Inserts or replaces a placement by its `(menu_id, menu_item_id)` pair. `prices_json` is the
    /// price list as JSON text, cast into the `jsonb` column.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    pub async fn upsert_placement(
        &self,
        tenant_id: &str,
        menu_id: &str,
        menu_item_id: &str,
        prices_json: &str,
        available: bool,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_placements (tenant_id, menu_id, menu_item_id, prices, available) \
                 VALUES ($1, $2, $3, $4::text::jsonb, $5) \
                 ON CONFLICT (menu_id, menu_item_id) \
                 DO UPDATE SET prices = $4::text::jsonb, available = $5, updated_at = now()",
                &[&tenant_id, &menu_id, &menu_item_id, &prices_json, &available],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a menu's placements within a tenant, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_placements(
        &self,
        tenant_id: &str,
        menu_id: &str,
    ) -> Result<Vec<CatalogPlacementRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT tenant_id, menu_id, menu_item_id, prices::text, available \
                 FROM catalog_placements WHERE tenant_id = $1 AND menu_id = $2 \
                 ORDER BY created_at DESC",
                &[&tenant_id, &menu_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CatalogPlacementRow {
                tenant_id: row.get(0),
                menu_id: row.get(1),
                menu_item_id: row.get(2),
                prices_json: row.get(3),
                available: row.get(4),
            })
            .collect())
    }

    /// Removes an item from a menu, within its tenant. Returns whether a row was found and removed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete_placement(
        &self,
        tenant_id: &str,
        menu_id: &str,
        menu_item_id: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "DELETE FROM catalog_placements \
                 WHERE tenant_id = $1 AND menu_id = $2 AND menu_item_id = $3",
                &[&tenant_id, &menu_id, &menu_item_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }
}
