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
    /// The per-locale names as a JSON object (locale code → name), the `name_translations` jsonb
    /// column verbatim; `{}` when the item has none (ADR-0074).
    pub name_translations: String,
    /// The tax class id (a ULID string).
    pub tax_class_id: String,
    /// The operational category id (a ULID string), or `None` if unclassified.
    pub item_category_id: Option<String>,
    /// The operational sub-category id (a ULID string), or `None`.
    pub item_subcategory_id: Option<String>,
    /// The item's image (a media id ULID string), or `None` (ADR-0075).
    pub image_ref: Option<String>,
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

/// A layout button as listed — one item's button in a per-channel layout.
#[derive(Clone, Debug)]
pub struct CatalogLayoutButtonRow {
    /// The owning tenant.
    pub tenant_id: String,
    /// The channel wire token this button lays out.
    pub sales_channel: String,
    /// The display category (a ULID string) the button sits under.
    pub display_category_id: String,
    /// The display sub-category (a ULID string), or `None` for a button directly under the category.
    pub display_subcategory_id: Option<String>,
    /// The item the button orders (a ULID string).
    pub menu_item_id: String,
    /// The caption to show.
    pub label: String,
    /// The grid column, or `None` for a flowing layout.
    pub grid_column: Option<i32>,
    /// The grid row, or `None` for a flowing layout.
    pub grid_row: Option<i32>,
    /// The display order within its group.
    pub sort: i32,
}

/// A modifier group as listed — a selection rule plus its members and attachments as JSON arrays.
#[derive(Clone, Debug)]
pub struct CatalogModifierGroupRow {
    /// The modifier-group id (a ULID string).
    pub modifier_group_id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The human name.
    pub name: String,
    /// The minimum number of choices.
    pub min_select: i32,
    /// The maximum number of choices.
    pub max_select: i32,
    /// The member item ids (the modifiers offered), as the JSON text stored in the `jsonb` column.
    pub member_item_ids_json: String,
    /// The attached item ids (the items this group modifies), as JSON text.
    pub attached_item_ids_json: String,
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

/// A menu section as listed — an authoring grouping within a menu.
#[derive(Clone, Debug)]
pub struct CatalogMenuSectionRow {
    /// The section id (a ULID string).
    pub menu_section_id: String,
    /// The owning tenant.
    pub tenant_id: String,
    /// The menu this section belongs to.
    pub menu_id: String,
    /// The human name.
    pub name: String,
    /// Where the section sorts within its menu, ascending.
    pub sort: i32,
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
    /// The section this placement sits under, or `None` for an unsectioned placement.
    pub menu_section_id: Option<String>,
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
    #[expect(
        clippy::too_many_arguments,
        reason = "one INSERT binding the item's flat columns by name; the names (ADR-0074) and image \
                  (ADR-0075) columns take it over the limit, and a params struct for a single call \
                  site would obscure more than it clarifies"
    )]
    pub async fn insert_item(
        &self,
        menu_item_id: &str,
        tenant_id: &str,
        name: &str,
        name_translations: &str,
        tax_class_id: &str,
        item_category_id: Option<&str>,
        item_subcategory_id: Option<&str>,
        image_ref: Option<&str>,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_items \
                 (menu_item_id, tenant_id, name, name_translations, tax_class_id, item_category_id, \
                 item_subcategory_id, image_ref) \
                 VALUES ($1, $2, $3, $4::jsonb, $5, $6, $7, $8)",
                &[
                    &menu_item_id,
                    &tenant_id,
                    &name,
                    &name_translations,
                    &tax_class_id,
                    &item_category_id,
                    &item_subcategory_id,
                    &image_ref,
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
                "SELECT menu_item_id, tenant_id, name, name_translations::text, tax_class_id, \
                 item_category_id, item_subcategory_id, image_ref, status FROM catalog_items \
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
                name_translations: row.get(3),
                tax_class_id: row.get(4),
                item_category_id: row.get(5),
                item_subcategory_id: row.get(6),
                image_ref: row.get(7),
                status: row.get(8),
            })
            .collect())
    }

    /// Renames an item, sets its tax class and status, within its tenant. Returns whether a row
    /// changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    #[expect(
        clippy::too_many_arguments,
        reason = "one UPDATE binding the item's flat columns by name; the per-locale names column \
                  (ADR-0074) takes it one over the limit, and a params struct for a single call site \
                  would obscure more than it clarifies"
    )]
    pub async fn set_item(
        &self,
        tenant_id: &str,
        menu_item_id: &str,
        name: &str,
        name_translations: &str,
        tax_class_id: &str,
        item_category_id: Option<&str>,
        item_subcategory_id: Option<&str>,
        image_ref: Option<&str>,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_items SET name = $3, name_translations = $4::jsonb, \
                 tax_class_id = $5, item_category_id = $6, item_subcategory_id = $7, \
                 image_ref = $8, status = $9, updated_at = now() \
                 WHERE tenant_id = $1 AND menu_item_id = $2",
                &[
                    &tenant_id,
                    &menu_item_id,
                    &name,
                    &name_translations,
                    &tax_class_id,
                    &item_category_id,
                    &item_subcategory_id,
                    &image_ref,
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

    // --- display taxonomy: categories and sub-categories (tenant-scoped) ---

    /// Inserts a display category.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_display_category(
        &self,
        display_category_id: &str,
        tenant_id: &str,
        name: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_display_categories (display_category_id, tenant_id, name) \
                 VALUES ($1, $2, $3)",
                &[&display_category_id, &tenant_id, &name],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's display categories, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_display_categories(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<CatalogTaxonomyRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT display_category_id, tenant_id, name, status FROM catalog_display_categories \
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

    /// Renames a display category and sets its status. Returns whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_display_category(
        &self,
        tenant_id: &str,
        display_category_id: &str,
        name: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_display_categories SET name = $3, status = $4, updated_at = now() \
                 WHERE tenant_id = $1 AND display_category_id = $2",
                &[&tenant_id, &display_category_id, &name, &status],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    /// Inserts a display sub-category under a parent display category.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_display_subcategory(
        &self,
        display_subcategory_id: &str,
        tenant_id: &str,
        display_category_id: &str,
        name: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_display_subcategories \
                 (display_subcategory_id, tenant_id, display_category_id, name) \
                 VALUES ($1, $2, $3, $4)",
                &[
                    &display_subcategory_id,
                    &tenant_id,
                    &display_category_id,
                    &name,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's display sub-categories, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_display_subcategories(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<CatalogTaxonomyRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT display_subcategory_id, tenant_id, display_category_id, name, status \
                 FROM catalog_display_subcategories WHERE tenant_id = $1 ORDER BY created_at DESC",
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

    /// Renames a display sub-category, (re)parents it and sets its status. Returns whether a row
    /// changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_display_subcategory(
        &self,
        tenant_id: &str,
        display_subcategory_id: &str,
        display_category_id: &str,
        name: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_display_subcategories SET display_category_id = $3, name = $4, \
                 status = $5, updated_at = now() \
                 WHERE tenant_id = $1 AND display_subcategory_id = $2",
                &[
                    &tenant_id,
                    &display_subcategory_id,
                    &display_category_id,
                    &name,
                    &status,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    // --- layout buttons (tenant-scoped, keyed by (tenant, sales_channel, menu_item_id)) ---

    /// Inserts or replaces a layout button by its `(sales_channel, menu_item_id)` pair within a tenant.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the write fails.
    #[expect(
        clippy::too_many_arguments,
        reason = "a layout button is a flat row of presentation columns (channel, category, \
                  optional sub-category, item, label, optional grid column/row, sort); a params \
                  struct would only move the list up one level"
    )]
    pub async fn upsert_layout_button(
        &self,
        tenant_id: &str,
        sales_channel: &str,
        display_category_id: &str,
        display_subcategory_id: Option<&str>,
        menu_item_id: &str,
        label: &str,
        grid_column: Option<i32>,
        grid_row: Option<i32>,
        sort: i32,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_layout_buttons \
                 (tenant_id, sales_channel, display_category_id, display_subcategory_id, \
                  menu_item_id, label, grid_column, grid_row, sort) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
                 ON CONFLICT (tenant_id, sales_channel, menu_item_id) DO UPDATE SET \
                  display_category_id = $3, display_subcategory_id = $4, label = $6, \
                  grid_column = $7, grid_row = $8, sort = $9, updated_at = now()",
                &[
                    &tenant_id,
                    &sales_channel,
                    &display_category_id,
                    &display_subcategory_id,
                    &menu_item_id,
                    &label,
                    &grid_column,
                    &grid_row,
                    &sort,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's layout buttons across all channels, by channel then display order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_layout_buttons(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<CatalogLayoutButtonRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT tenant_id, sales_channel, display_category_id, display_subcategory_id, \
                 menu_item_id, label, grid_column, grid_row, sort \
                 FROM catalog_layout_buttons WHERE tenant_id = $1 \
                 ORDER BY sales_channel, sort",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CatalogLayoutButtonRow {
                tenant_id: row.get(0),
                sales_channel: row.get(1),
                display_category_id: row.get(2),
                display_subcategory_id: row.get(3),
                menu_item_id: row.get(4),
                label: row.get(5),
                grid_column: row.get(6),
                grid_row: row.get(7),
                sort: row.get(8),
            })
            .collect())
    }

    /// Removes a layout button by its `(sales_channel, menu_item_id)` pair within a tenant. Returns
    /// whether a row was found and removed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn delete_layout_button(
        &self,
        tenant_id: &str,
        sales_channel: &str,
        menu_item_id: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "DELETE FROM catalog_layout_buttons \
                 WHERE tenant_id = $1 AND sales_channel = $2 AND menu_item_id = $3",
                &[&tenant_id, &sales_channel, &menu_item_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(changed == 1)
    }

    // --- modifier groups (tenant-scoped) ---

    /// Inserts a modifier group. `member_item_ids_json` / `attached_item_ids_json` are JSON arrays of
    /// ULID strings, cast into `jsonb` columns.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_modifier_group(
        &self,
        modifier_group_id: &str,
        tenant_id: &str,
        name: &str,
        min_select: i32,
        max_select: i32,
        member_item_ids_json: &str,
        attached_item_ids_json: &str,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_modifier_groups \
                 (modifier_group_id, tenant_id, name, min_select, max_select, member_item_ids, \
                  attached_item_ids) \
                 VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7::text::jsonb)",
                &[
                    &modifier_group_id,
                    &tenant_id,
                    &name,
                    &min_select,
                    &max_select,
                    &member_item_ids_json,
                    &attached_item_ids_json,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a tenant's modifier groups, newest first.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_modifier_groups(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<CatalogModifierGroupRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT modifier_group_id, tenant_id, name, min_select, max_select, \
                 member_item_ids::text, attached_item_ids::text, status \
                 FROM catalog_modifier_groups WHERE tenant_id = $1 ORDER BY created_at DESC",
                &[&tenant_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CatalogModifierGroupRow {
                modifier_group_id: row.get(0),
                tenant_id: row.get(1),
                name: row.get(2),
                min_select: row.get(3),
                max_select: row.get(4),
                member_item_ids_json: row.get(5),
                attached_item_ids_json: row.get(6),
                status: row.get(7),
            })
            .collect())
    }

    /// Renames a modifier group, sets its selection rule, members, attachments and status. Returns
    /// whether a row changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    #[expect(
        clippy::too_many_arguments,
        reason = "an update sets each authored column explicitly; a params struct would only move \
                  the list into the seam that calls this"
    )]
    pub async fn set_modifier_group(
        &self,
        tenant_id: &str,
        modifier_group_id: &str,
        name: &str,
        min_select: i32,
        max_select: i32,
        member_item_ids_json: &str,
        attached_item_ids_json: &str,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_modifier_groups SET name = $3, min_select = $4, max_select = $5, \
                 member_item_ids = $6::text::jsonb, attached_item_ids = $7::text::jsonb, \
                 status = $8, updated_at = now() \
                 WHERE tenant_id = $1 AND modifier_group_id = $2",
                &[
                    &tenant_id,
                    &modifier_group_id,
                    &name,
                    &min_select,
                    &max_select,
                    &member_item_ids_json,
                    &attached_item_ids_json,
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

    // --- menu sections (tenant-scoped, within a menu) ---

    /// Inserts a menu section.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or the insert fails.
    pub async fn insert_menu_section(
        &self,
        menu_section_id: &str,
        tenant_id: &str,
        menu_id: &str,
        name: &str,
        sort: i32,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_menu_sections \
                 (menu_section_id, tenant_id, menu_id, name, sort) VALUES ($1, $2, $3, $4, $5)",
                &[&menu_section_id, &tenant_id, &menu_id, &name, &sort],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Lists a menu's sections within a tenant, by sort then id.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn fetch_menu_sections(
        &self,
        tenant_id: &str,
        menu_id: &str,
    ) -> Result<Vec<CatalogMenuSectionRow>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT menu_section_id, tenant_id, menu_id, name, sort, status \
                 FROM catalog_menu_sections WHERE tenant_id = $1 AND menu_id = $2 \
                 ORDER BY sort ASC, menu_section_id ASC",
                &[&tenant_id, &menu_id],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| CatalogMenuSectionRow {
                menu_section_id: row.get(0),
                tenant_id: row.get(1),
                menu_id: row.get(2),
                name: row.get(3),
                sort: row.get(4),
                status: row.get(5),
            })
            .collect())
    }

    /// Renames a menu section, sets its sort and status, within its tenant. Returns whether a row
    /// changed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn set_menu_section(
        &self,
        tenant_id: &str,
        menu_section_id: &str,
        name: &str,
        sort: i32,
        status: &str,
    ) -> Result<bool, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let changed = connection
            .execute(
                "UPDATE catalog_menu_sections SET name = $3, sort = $4, status = $5, \
                 updated_at = now() WHERE tenant_id = $1 AND menu_section_id = $2",
                &[&tenant_id, &menu_section_id, &name, &sort, &status],
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
        menu_section_id: Option<&str>,
        prices_json: &str,
        available: bool,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO catalog_placements \
                 (tenant_id, menu_id, menu_item_id, menu_section_id, prices, available) \
                 VALUES ($1, $2, $3, $4, $5::text::jsonb, $6) \
                 ON CONFLICT (menu_id, menu_item_id) \
                 DO UPDATE SET menu_section_id = $4, prices = $5::text::jsonb, available = $6, \
                 updated_at = now()",
                &[
                    &tenant_id,
                    &menu_id,
                    &menu_item_id,
                    &menu_section_id,
                    &prices_json,
                    &available,
                ],
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
                "SELECT tenant_id, menu_id, menu_item_id, menu_section_id, prices::text, available \
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
                menu_section_id: row.get(3),
                prices_json: row.get(4),
                available: row.get(5),
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
