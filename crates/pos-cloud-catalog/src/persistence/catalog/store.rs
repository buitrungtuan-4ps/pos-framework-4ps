// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `CatalogStore` impl for the `store-postgres` catalog tables.

use store_postgres::{ItemOrder, PostgresCatalog};

use pos_proto::enums::SalesChannel;
use pos_proto::ids::{MenuItemId, TenantId};
use pos_proto::wire_enum::Open;

use crate::catalog::{
    CatalogItem, CatalogStore, CatalogStoreError, Course, DisplayCategory, DisplaySubcategory,
    ItemCategory, ItemListFilter, ItemSort, ItemSubcategory, LayoutButton, Menu, MenuId,
    MenuPlacement, MenuSection, ModifierGroup, TaxClass,
};
use crate::paging::{Page, PageRequest};
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};
use pos_cloud_config::persistence::{create_outcome, update_outcome};

use super::{
    catalog_category_record, catalog_course_record, catalog_display_category_record,
    catalog_display_subcategory_record, catalog_item_record, catalog_layout_button_record,
    catalog_menu_record, catalog_menu_section_record, catalog_modifier_group_record,
    catalog_placement_record, catalog_subcategory_record, catalog_tax_class_record,
    item_id_list_json,
};

impl CatalogStore for PostgresCatalog {
    async fn create_item(&self, item: &CatalogItem) -> Result<Version, CatalogStoreError> {
        let category = item.item_category_id.map(|id| id.to_string());
        let subcategory = item.item_subcategory_id.map(|id| id.to_string());
        let name_translations = serde_json::to_string(&item.name_translations)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        let course = item.course_id.map(|id| id.to_string());
        let image_ref = item.image_ref.map(|id| id.to_string());
        self.insert_item(
            &item.menu_item_id.to_string(),
            &item.tenant_id.to_string(),
            &item.name,
            &name_translations,
            &item.tax_class_id.to_string(),
            category.as_deref(),
            subcategory.as_deref(),
            course.as_deref(),
            image_ref.as_deref(),
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_items(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<CatalogItem>>, CatalogStoreError> {
        let rows = self
            .fetch_items(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_item_record).collect()
    }

    async fn list_items_page(
        &self,
        tenant_id: TenantId,
        page: PageRequest,
        filter: &ItemListFilter,
    ) -> Result<Page<Versioned<CatalogItem>>, CatalogStoreError> {
        // The one place the domain's sort vocabulary meets the adapter's. Exhaustive by necessity:
        // `clippy::wildcard_enum_match_arm` is denied, so a new `ItemSort` variant fails to compile
        // here rather than quietly falling back to the newest order.
        let order = match filter.sort {
            ItemSort::Newest => ItemOrder::Newest,
            ItemSort::Name => ItemOrder::Name,
            ItemSort::Status => ItemOrder::Status,
        };
        let (rows, total) = self
            .fetch_items_page(
                &tenant_id.to_string(),
                filter.search.as_deref(),
                order,
                filter.descending,
                i64::from(page.limit()),
                i64::from(page.offset()),
            )
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        let items: Vec<Versioned<CatalogItem>> = rows
            .into_iter()
            .map(catalog_item_record)
            .collect::<Result<_, _>>()?;
        Ok(Page::new(items, u32::try_from(total).unwrap_or(u32::MAX)))
    }

    async fn update_item(
        &self,
        item: &CatalogItem,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let category = item.item_category_id.map(|id| id.to_string());
        let subcategory = item.item_subcategory_id.map(|id| id.to_string());
        let name_translations = serde_json::to_string(&item.name_translations)
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        let course = item.course_id.map(|id| id.to_string());
        let image_ref = item.image_ref.map(|id| id.to_string());
        self.set_item(
            &item.tenant_id.to_string(),
            &item.menu_item_id.to_string(),
            &item.name,
            &name_translations,
            &item.tax_class_id.to_string(),
            category.as_deref(),
            subcategory.as_deref(),
            course.as_deref(),
            image_ref.as_deref(),
            item.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_tax_class(&self, tax_class: &TaxClass) -> Result<Version, CatalogStoreError> {
        self.insert_tax_class(
            &tax_class.tax_class_id.to_string(),
            &tax_class.tenant_id.to_string(),
            &tax_class.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_tax_classes(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<TaxClass>>, CatalogStoreError> {
        let rows = self
            .fetch_tax_classes(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_tax_class_record).collect()
    }

    async fn update_tax_class(
        &self,
        tax_class: &TaxClass,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_tax_class(
            &tax_class.tenant_id.to_string(),
            &tax_class.tax_class_id.to_string(),
            &tax_class.name,
            tax_class.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_item_category(
        &self,
        category: &ItemCategory,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_item_category(
            &category.item_category_id.to_string(),
            &category.tenant_id.to_string(),
            &category.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_item_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<ItemCategory>>, CatalogStoreError> {
        let rows = self
            .fetch_item_categories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_category_record).collect()
    }

    async fn update_item_category(
        &self,
        category: &ItemCategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_item_category(
            &category.tenant_id.to_string(),
            &category.item_category_id.to_string(),
            &category.name,
            category.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_item_subcategory(
            &subcategory.item_subcategory_id.to_string(),
            &subcategory.tenant_id.to_string(),
            &subcategory.item_category_id.to_string(),
            &subcategory.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_item_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<ItemSubcategory>>, CatalogStoreError> {
        let rows = self
            .fetch_item_subcategories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_subcategory_record).collect()
    }

    async fn update_item_subcategory(
        &self,
        subcategory: &ItemSubcategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_item_subcategory(
            &subcategory.tenant_id.to_string(),
            &subcategory.item_subcategory_id.to_string(),
            &subcategory.item_category_id.to_string(),
            &subcategory.name,
            subcategory.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_display_category(
        &self,
        category: &DisplayCategory,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_display_category(
            &category.display_category_id.to_string(),
            &category.tenant_id.to_string(),
            &category.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_display_categories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<DisplayCategory>>, CatalogStoreError> {
        let rows = self
            .fetch_display_categories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter()
            .map(catalog_display_category_record)
            .collect()
    }

    async fn update_display_category(
        &self,
        category: &DisplayCategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_display_category(
            &category.tenant_id.to_string(),
            &category.display_category_id.to_string(),
            &category.name,
            category.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_display_subcategory(
            &subcategory.display_subcategory_id.to_string(),
            &subcategory.tenant_id.to_string(),
            &subcategory.display_category_id.to_string(),
            &subcategory.name,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_display_subcategories(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<DisplaySubcategory>>, CatalogStoreError> {
        let rows = self
            .fetch_display_subcategories(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter()
            .map(catalog_display_subcategory_record)
            .collect()
    }

    async fn update_display_subcategory(
        &self,
        subcategory: &DisplaySubcategory,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_display_subcategory(
            &subcategory.tenant_id.to_string(),
            &subcategory.display_subcategory_id.to_string(),
            &subcategory.display_category_id.to_string(),
            &subcategory.name,
            subcategory.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_layout_button(
        &self,
        button: &LayoutButton,
    ) -> Result<CreateOutcome, CatalogStoreError> {
        let subcategory = button.display_subcategory_id.map(|id| id.to_string());
        self.insert_layout_button(
            &button.tenant_id.to_string(),
            button.sales_channel.as_wire(),
            &button.display_category_id.to_string(),
            subcategory.as_deref(),
            &button.menu_item_id.to_string(),
            &button.label,
            button.position.map(|p| i32::from(p.column)),
            button.position.map(|p| i32::from(p.row)),
            button.sort,
        )
        .await
        .map(create_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn update_layout_button(
        &self,
        button: &LayoutButton,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let subcategory = button.display_subcategory_id.map(|id| id.to_string());
        self.update_layout_button_at(
            &button.tenant_id.to_string(),
            button.sales_channel.as_wire(),
            &button.display_category_id.to_string(),
            subcategory.as_deref(),
            &button.menu_item_id.to_string(),
            &button.label,
            button.position.map(|p| i32::from(p.column)),
            button.position.map(|p| i32::from(p.row)),
            button.sort,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_layout_buttons(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<LayoutButton>>, CatalogStoreError> {
        let rows = self
            .fetch_layout_buttons(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_layout_button_record).collect()
    }

    async fn remove_layout_button(
        &self,
        tenant_id: TenantId,
        sales_channel: Open<SalesChannel>,
        menu_item_id: MenuItemId,
    ) -> Result<bool, CatalogStoreError> {
        self.delete_layout_button(
            &tenant_id.to_string(),
            sales_channel.as_wire(),
            &menu_item_id.to_string(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_modifier_group(
        &self,
        group: &ModifierGroup,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_modifier_group(
            &group.modifier_group_id.to_string(),
            &group.tenant_id.to_string(),
            &group.name,
            i32::from(group.min_select),
            i32::from(group.max_select),
            &item_id_list_json(&group.member_item_ids)?,
            &item_id_list_json(&group.attached_item_ids)?,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_modifier_groups(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<ModifierGroup>>, CatalogStoreError> {
        let rows = self
            .fetch_modifier_groups(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter()
            .map(catalog_modifier_group_record)
            .collect()
    }

    async fn update_modifier_group(
        &self,
        group: &ModifierGroup,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_modifier_group(
            &group.tenant_id.to_string(),
            &group.modifier_group_id.to_string(),
            &group.name,
            i32::from(group.min_select),
            i32::from(group.max_select),
            &item_id_list_json(&group.member_item_ids)?,
            &item_id_list_json(&group.attached_item_ids)?,
            group.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_course(&self, course: &Course) -> Result<Version, CatalogStoreError> {
        self.insert_course(
            &course.course_id.to_string(),
            &course.tenant_id.to_string(),
            &course.name,
            course.sort,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_courses(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<Course>>, CatalogStoreError> {
        let rows = self
            .fetch_courses(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_course_record).collect()
    }

    async fn update_course(
        &self,
        course: &Course,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_course(
            &course.tenant_id.to_string(),
            &course.course_id.to_string(),
            &course.name,
            course.sort,
            course.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_menu(&self, menu: &Menu) -> Result<Version, CatalogStoreError> {
        let parent = menu.parent_menu_id.map(|id| id.to_string());
        self.insert_menu(
            &menu.menu_id.to_string(),
            &menu.tenant_id.to_string(),
            &menu.name,
            parent.as_deref(),
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_menus(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<Versioned<Menu>>, CatalogStoreError> {
        let rows = self
            .fetch_menus(&tenant_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_menu_record).collect()
    }

    async fn update_menu(
        &self,
        menu: &Menu,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let parent = menu.parent_menu_id.map(|id| id.to_string());
        self.set_menu(
            &menu.tenant_id.to_string(),
            &menu.menu_id.to_string(),
            &menu.name,
            parent.as_deref(),
            menu.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_menu_section(
        &self,
        section: &MenuSection,
    ) -> Result<Version, CatalogStoreError> {
        self.insert_menu_section(
            &section.menu_section_id.to_string(),
            &section.tenant_id.to_string(),
            &section.menu_id.to_string(),
            &section.name,
            section.sort,
        )
        .await
        .map(Version::new)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_menu_sections(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<Versioned<MenuSection>>, CatalogStoreError> {
        let rows = self
            .fetch_menu_sections(&tenant_id.to_string(), &menu_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.into_iter().map(catalog_menu_section_record).collect()
    }

    async fn update_menu_section(
        &self,
        section: &MenuSection,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        self.set_menu_section(
            &section.tenant_id.to_string(),
            &section.menu_section_id.to_string(),
            &section.name,
            section.sort,
            section.status.as_str(),
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn create_placement(
        &self,
        placement: &MenuPlacement,
    ) -> Result<CreateOutcome, CatalogStoreError> {
        let prices_json = serde_json::to_string(&placement.prices).map_err(|error| {
            CatalogStoreError::new(format!("could not serialise placement prices: {error}"))
        })?;
        let section = placement.menu_section_id.map(|id| id.to_string());
        self.insert_placement(
            &placement.tenant_id.to_string(),
            &placement.menu_id.to_string(),
            &placement.menu_item_id.to_string(),
            section.as_deref(),
            &prices_json,
            placement.available,
        )
        .await
        .map(create_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn update_placement(
        &self,
        placement: &MenuPlacement,
        expected: &Version,
    ) -> Result<UpdateOutcome, CatalogStoreError> {
        let prices_json = serde_json::to_string(&placement.prices).map_err(|error| {
            CatalogStoreError::new(format!("could not serialise placement prices: {error}"))
        })?;
        let section = placement.menu_section_id.map(|id| id.to_string());
        self.update_placement_at(
            &placement.tenant_id.to_string(),
            &placement.menu_id.to_string(),
            &placement.menu_item_id.to_string(),
            section.as_deref(),
            &prices_json,
            placement.available,
            expected.as_str(),
        )
        .await
        .map(update_outcome)
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }

    async fn list_placements(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> Result<Vec<Versioned<MenuPlacement>>, CatalogStoreError> {
        let rows = self
            .fetch_placements(&tenant_id.to_string(), &menu_id.to_string())
            .await
            .map_err(|error| CatalogStoreError::new(error.to_string()))?;
        rows.iter().map(catalog_placement_record).collect()
    }

    async fn remove_placement(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
        menu_item_id: MenuItemId,
    ) -> Result<bool, CatalogStoreError> {
        self.delete_placement(
            &tenant_id.to_string(),
            &menu_id.to_string(),
            &menu_item_id.to_string(),
        )
        .await
        .map_err(|error| CatalogStoreError::new(error.to_string()))
    }
}
