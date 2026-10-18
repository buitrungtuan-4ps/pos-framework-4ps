// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The catalog's rows from `store-postgres` converted to the `CatalogStore` domain, and the
//! `CatalogStore` impl over them (`store`).

use std::collections::BTreeMap;

use store_postgres::{
    CatalogCourseRow, CatalogItemRow, CatalogLayoutButtonRow, CatalogMenuRow,
    CatalogMenuSectionRow, CatalogModifierGroupRow, CatalogPlacementRow, CatalogTaxClassRow,
    CatalogTaxonomyRow,
};

use pos_proto::display::GridPosition;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{CourseId, DisplayCategoryId, DisplaySubcategoryId, MenuItemId, TaxClassId};
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::catalog::{
    CatalogItem, CatalogStoreError, ChannelPrice, Course, DisplayCategory, DisplaySubcategory,
    ItemCategory, ItemCategoryId, ItemSubcategory, ItemSubcategoryId, LayoutButton, Menu, MenuId,
    MenuPlacement, MenuSection, MenuSectionId, ModifierGroup, ModifierGroupId, TaxClass,
};
use crate::media::MediaId;
use crate::registry::EntityStatus;
use crate::version::{Version, Versioned};
use pos_cloud_config::persistence::parse_registry_tenant;

mod store;

// --- catalog (Phase 2a, ADR-0066): the store-postgres rows converted to the CatalogStore domain ---

fn parse_catalog_item_id(text: &str) -> Result<MenuItemId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(MenuItemId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!("a catalog item id is not a ULID: {text}"))
        })
}

fn parse_catalog_menu_id(text: &str) -> Result<MenuId, CatalogStoreError> {
    text.parse::<Ulid>().map(MenuId::new).map_err(|_ignored| {
        CatalogStoreError::new(format!("a catalog menu id is not a ULID: {text}"))
    })
}

fn parse_catalog_menu_section_id(text: &str) -> Result<MenuSectionId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(MenuSectionId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!("a catalog menu section id is not a ULID: {text}"))
        })
}

fn parse_catalog_tax_class(text: &str) -> Result<TaxClassId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(TaxClassId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!("a catalog tax class id is not a ULID: {text}"))
        })
}

fn parse_catalog_category_id(text: &str) -> Result<ItemCategoryId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(ItemCategoryId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!("a catalog item category id is not a ULID: {text}"))
        })
}

fn parse_catalog_subcategory_id(text: &str) -> Result<ItemSubcategoryId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(ItemSubcategoryId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!(
                "a catalog item sub-category id is not a ULID: {text}"
            ))
        })
}

fn catalog_item_record(row: CatalogItemRow) -> Result<Versioned<CatalogItem>, CatalogStoreError> {
    let item_category_id = match row.item_category_id {
        Some(text) => Some(parse_catalog_category_id(&text)?),
        None => None,
    };
    let item_subcategory_id = match row.item_subcategory_id {
        Some(text) => Some(parse_catalog_subcategory_id(&text)?),
        None => None,
    };
    // The `name_translations` jsonb is a locale→name object we wrote ourselves; a value that does not
    // parse as such falls back to no translations (the item's `name` remains its caption) rather than
    // failing the whole list — a malformed blob must not take a store's menu away.
    let name_translations: BTreeMap<String, String> =
        serde_json::from_str(&row.name_translations).unwrap_or_default();
    // A malformed `course_id` degrades to "on no course" rather than failing the list, the same
    // never-blank posture the image below takes: a bad id must not take a store's menu away, and an
    // item on no course is a real and correct state (ADR-0130).
    let course_id = row
        .course_id
        .as_deref()
        .and_then(|text| text.parse::<Ulid>().ok())
        .map(CourseId::new);
    // A malformed `image_ref` (not a ULID) degrades to "no image" rather than failing the list — the
    // never-blank / placeholder posture (ADR-0075), the same as a media asset that was later deleted.
    let image_ref = row
        .image_ref
        .as_deref()
        .and_then(|text| text.parse::<Ulid>().ok())
        .map(MediaId::new);
    let version = Version::new(row.version);
    Ok(Versioned::new(
        CatalogItem {
            menu_item_id: parse_catalog_item_id(&row.menu_item_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            name_translations,
            tax_class_id: parse_catalog_tax_class(&row.tax_class_id)?,
            item_category_id,
            item_subcategory_id,
            course_id,
            image_ref,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_category_record(
    row: CatalogTaxonomyRow,
) -> Result<Versioned<ItemCategory>, CatalogStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        ItemCategory {
            item_category_id: parse_catalog_category_id(&row.id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_subcategory_record(
    row: CatalogTaxonomyRow,
) -> Result<Versioned<ItemSubcategory>, CatalogStoreError> {
    let parent = row.parent_id.ok_or_else(|| {
        CatalogStoreError::new("an item sub-category row is missing its parent category")
    })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        ItemSubcategory {
            item_subcategory_id: parse_catalog_subcategory_id(&row.id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            item_category_id: parse_catalog_category_id(&parent)?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_tax_class_record(
    row: CatalogTaxClassRow,
) -> Result<Versioned<TaxClass>, CatalogStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        TaxClass {
            tax_class_id: parse_catalog_tax_class(&row.tax_class_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn parse_catalog_display_category(text: &str) -> Result<DisplayCategoryId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(DisplayCategoryId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!(
                "a catalog display category id is not a ULID: {text}"
            ))
        })
}

fn parse_catalog_display_subcategory(
    text: &str,
) -> Result<DisplaySubcategoryId, CatalogStoreError> {
    text.parse::<Ulid>()
        .map(DisplaySubcategoryId::new)
        .map_err(|_ignored| {
            CatalogStoreError::new(format!(
                "a catalog display sub-category id is not a ULID: {text}"
            ))
        })
}

fn catalog_display_category_record(
    row: CatalogTaxonomyRow,
) -> Result<Versioned<DisplayCategory>, CatalogStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        DisplayCategory {
            display_category_id: parse_catalog_display_category(&row.id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_display_subcategory_record(
    row: CatalogTaxonomyRow,
) -> Result<Versioned<DisplaySubcategory>, CatalogStoreError> {
    let parent = row.parent_id.ok_or_else(|| {
        CatalogStoreError::new("a display sub-category row is missing its parent category")
    })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        DisplaySubcategory {
            display_subcategory_id: parse_catalog_display_subcategory(&row.id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            display_category_id: parse_catalog_display_category(&parent)?,
            name: row.name,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_layout_button_record(
    row: CatalogLayoutButtonRow,
) -> Result<Versioned<LayoutButton>, CatalogStoreError> {
    let display_subcategory_id = match row.display_subcategory_id {
        Some(text) => Some(parse_catalog_display_subcategory(&text)?),
        None => None,
    };
    // A grid slot exists only when both column and row are stored; a flowing button has neither.
    let position = match (row.grid_column, row.grid_row) {
        (Some(column), Some(grid_row)) => {
            let column = u16::try_from(column).map_err(|_ignored| {
                CatalogStoreError::new("a layout button's grid column is out of range")
            })?;
            let row_index = u16::try_from(grid_row).map_err(|_ignored| {
                CatalogStoreError::new("a layout button's grid row is out of range")
            })?;
            Some(GridPosition {
                column,
                row: row_index,
            })
        }
        _ => None,
    };
    let version = Version::new(row.version);
    Ok(Versioned::new(
        LayoutButton {
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            sales_channel: Open::<SalesChannel>::parse(&row.sales_channel),
            display_category_id: parse_catalog_display_category(&row.display_category_id)?,
            display_subcategory_id,
            menu_item_id: parse_catalog_item_id(&row.menu_item_id)?,
            label: row.label,
            position,
            sort: row.sort,
        },
        version,
    ))
}

fn item_id_list_json(ids: &[MenuItemId]) -> Result<String, CatalogStoreError> {
    let raw: Vec<String> = ids.iter().map(ToString::to_string).collect();
    serde_json::to_string(&raw).map_err(|error| {
        CatalogStoreError::new(format!("could not serialise an item id list: {error}"))
    })
}

fn parse_catalog_item_id_list(json: &str) -> Result<Vec<MenuItemId>, CatalogStoreError> {
    let raw: Vec<String> = serde_json::from_str(json).map_err(|error| {
        CatalogStoreError::new(format!(
            "a modifier group's item id list is not valid JSON: {error}"
        ))
    })?;
    raw.iter().map(|text| parse_catalog_item_id(text)).collect()
}

/// Converts a stored course row into the domain [`Course`]
/// ([ADR-0130](../../../docs/adr/0130-a-course-is-something-the-catalog-names.md)).
fn catalog_course_record(row: CatalogCourseRow) -> Result<Versioned<Course>, CatalogStoreError> {
    let course_id = row
        .course_id
        .parse::<Ulid>()
        .map(CourseId::new)
        .map_err(|error| {
            CatalogStoreError::new(format!("stored course id is not a ULID: {error}"))
        })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        Course {
            course_id,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            sort: row.sort,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_modifier_group_record(
    row: CatalogModifierGroupRow,
) -> Result<Versioned<ModifierGroup>, CatalogStoreError> {
    let min_select = u16::try_from(row.min_select).map_err(|_ignored| {
        CatalogStoreError::new("a modifier group's min_select is out of range")
    })?;
    let max_select = u16::try_from(row.max_select).map_err(|_ignored| {
        CatalogStoreError::new("a modifier group's max_select is out of range")
    })?;
    let version = Version::new(row.version);
    Ok(Versioned::new(
        ModifierGroup {
            modifier_group_id: row
                .modifier_group_id
                .parse::<Ulid>()
                .map(ModifierGroupId::new)
                .map_err(|_ignored| {
                    CatalogStoreError::new(format!(
                        "a modifier group id is not a ULID: {}",
                        row.modifier_group_id
                    ))
                })?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            min_select,
            max_select,
            member_item_ids: parse_catalog_item_id_list(&row.member_item_ids_json)?,
            attached_item_ids: parse_catalog_item_id_list(&row.attached_item_ids_json)?,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_menu_record(row: CatalogMenuRow) -> Result<Versioned<Menu>, CatalogStoreError> {
    let parent_menu_id = match row.parent_menu_id {
        Some(text) => Some(parse_catalog_menu_id(&text)?),
        None => None,
    };
    let version = Version::new(row.version);
    Ok(Versioned::new(
        Menu {
            menu_id: parse_catalog_menu_id(&row.menu_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            name: row.name,
            parent_menu_id,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_menu_section_record(
    row: CatalogMenuSectionRow,
) -> Result<Versioned<MenuSection>, CatalogStoreError> {
    let version = Version::new(row.version);
    Ok(Versioned::new(
        MenuSection {
            menu_section_id: parse_catalog_menu_section_id(&row.menu_section_id)?,
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            menu_id: parse_catalog_menu_id(&row.menu_id)?,
            name: row.name,
            sort: row.sort,
            status: EntityStatus::from_db(&row.status),
        },
        version,
    ))
}

fn catalog_placement_record(
    row: &CatalogPlacementRow,
) -> Result<Versioned<MenuPlacement>, CatalogStoreError> {
    let prices: Vec<ChannelPrice> = serde_json::from_str(&row.prices_json).map_err(|error| {
        CatalogStoreError::new(format!(
            "a placement's stored prices are not valid JSON: {error}"
        ))
    })?;
    let menu_section_id = match &row.menu_section_id {
        Some(text) => Some(parse_catalog_menu_section_id(text)?),
        None => None,
    };
    Ok(Versioned::new(
        MenuPlacement {
            tenant_id: parse_registry_tenant(&row.tenant_id)
                .map_err(|error| CatalogStoreError::new(error.to_string()))?,
            menu_id: parse_catalog_menu_id(&row.menu_id)?,
            menu_item_id: parse_catalog_item_id(&row.menu_item_id)?,
            menu_section_id,
            prices,
            available: row.available,
        },
        Version::new(row.version.clone()),
    ))
}
