// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The request bodies the catalog authoring routes take.

use serde::Deserialize;

use crate::catalog::ChannelPrice;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateItemRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    /// Per-locale names keyed by locale code (ADR-0074). Optional and additive; `name` is the
    /// always-present fallback.
    #[serde(default)]
    pub(super) name_translations: std::collections::BTreeMap<String, String>,
    pub(super) tax_class_id: String,
    #[serde(default)]
    pub(super) item_category_id: Option<String>,
    #[serde(default)]
    pub(super) item_subcategory_id: Option<String>,
    /// The course this item goes out on (ADR-0130). Absent leaves it on none, which is every item
    /// in every store today.
    #[serde(default)]
    pub(super) course_id: Option<String>,
    /// The item's photo — a media id (ADR-0075), or absent/empty for none.
    #[serde(default)]
    pub(super) image_ref: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateItemRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    /// Per-locale names keyed by locale code (ADR-0074). Optional and additive; `name` is the
    /// always-present fallback.
    #[serde(default)]
    pub(super) name_translations: std::collections::BTreeMap<String, String>,
    pub(super) tax_class_id: String,
    #[serde(default)]
    pub(super) item_category_id: Option<String>,
    #[serde(default)]
    pub(super) item_subcategory_id: Option<String>,
    /// The course this item goes out on (ADR-0130). Absent leaves it on none, which is every item
    /// in every store today.
    #[serde(default)]
    pub(super) course_id: Option<String>,
    /// The item's photo — a media id (ADR-0075), or absent/empty for none.
    #[serde(default)]
    pub(super) image_ref: Option<String>,
    pub(super) status: String,
}

/// Drops empty-key or empty-value entries from an authored per-locale name map and trims both sides,
/// so a blank row a form left behind never becomes a `""` translation the edge would show in place of
/// the default name (ADR-0074).
#[must_use]
pub fn clean_name_translations(
    raw: std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    raw.into_iter()
        .filter_map(|(locale, name)| {
            let locale = locale.trim().to_owned();
            let name = name.trim().to_owned();
            (!locale.is_empty() && !name.is_empty()).then_some((locale, name))
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateTaxClassRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateTaxClassRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    pub(super) status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateItemCategoryRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateItemCategoryRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    pub(super) status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateItemSubcategoryRequest {
    pub(super) tenant_id: String,
    pub(super) item_category_id: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateItemSubcategoryRequest {
    pub(super) tenant_id: String,
    pub(super) item_category_id: String,
    pub(super) name: String,
    pub(super) status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateDisplayCategoryRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateDisplayCategoryRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    pub(super) status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateDisplaySubcategoryRequest {
    pub(super) tenant_id: String,
    pub(super) display_category_id: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateDisplaySubcategoryRequest {
    pub(super) tenant_id: String,
    pub(super) display_category_id: String,
    pub(super) name: String,
    pub(super) status: String,
}

/// A `POST /admin/catalog/layout-buttons` body: a button's presentation plus the `(channel, item)`
/// slot it goes in, which the collection URI has no room for and the `PUT` takes from its path.
///
/// The identity fields are spelled out here rather than `#[serde(flatten)]`-ing the update body:
/// flatten pulls in serde's buffering `Content` enum, whose `f32`/`f64` variants `clippy.toml` bans
/// outright (`docs/adr/0013`: money is never a float). `into_write` keeps the two shapes from
/// drifting instead.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateLayoutButtonRequest {
    pub(super) sales_channel: String,
    pub(super) menu_item_id: String,
    tenant_id: String,
    display_category_id: String,
    #[serde(default)]
    display_subcategory_id: Option<String>,
    label: String,
    #[serde(default)]
    grid_column: Option<u16>,
    #[serde(default)]
    grid_row: Option<u16>,
    #[serde(default)]
    sort: i32,
}

impl CreateLayoutButtonRequest {
    /// The presentation half, as the shared builder takes it.
    pub(super) fn into_write(self) -> SetLayoutButtonRequest {
        SetLayoutButtonRequest {
            tenant_id: self.tenant_id,
            display_category_id: self.display_category_id,
            display_subcategory_id: self.display_subcategory_id,
            label: self.label,
            grid_column: self.grid_column,
            grid_row: self.grid_row,
            sort: self.sort,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SetLayoutButtonRequest {
    pub(super) tenant_id: String,
    pub(super) display_category_id: String,
    #[serde(default)]
    pub(super) display_subcategory_id: Option<String>,
    pub(super) label: String,
    #[serde(default)]
    pub(super) grid_column: Option<u16>,
    #[serde(default)]
    pub(super) grid_row: Option<u16>,
    #[serde(default)]
    pub(super) sort: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateModifierGroupRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) min_select: u16,
    #[serde(default)]
    pub(super) max_select: u16,
    #[serde(default)]
    pub(super) member_item_ids: Vec<String>,
    #[serde(default)]
    pub(super) attached_item_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateModifierGroupRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) min_select: u16,
    #[serde(default)]
    pub(super) max_select: u16,
    #[serde(default)]
    pub(super) member_item_ids: Vec<String>,
    #[serde(default)]
    pub(super) attached_item_ids: Vec<String>,
    pub(super) status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateMenuRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    pub(super) parent_menu_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateMenuRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    pub(super) parent_menu_id: Option<String>,
    pub(super) status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreateMenuSectionRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) sort: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct UpdateMenuSectionRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) sort: i32,
    pub(super) status: String,
}

/// A `POST /admin/catalog/menus/{menu_id}/placements` body: a placement plus the `(menu, item)` pair
/// it is keyed by. The menu is also in the path; both are read from the body so this route and the
/// `PUT` share one builder.
///
/// Flat rather than `#[serde(flatten)]` for the same reason as [`CreateLayoutButtonRequest`].
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CreatePlacementRequest {
    pub(super) menu_id: String,
    pub(super) menu_item_id: String,
    tenant_id: String,
    #[serde(default)]
    menu_section_id: Option<String>,
    prices: Vec<ChannelPrice>,
    available: bool,
}

impl CreatePlacementRequest {
    /// The placement half, as the shared builder takes it.
    pub(super) fn into_write(self) -> SetPlacementRequest {
        SetPlacementRequest {
            tenant_id: self.tenant_id,
            menu_section_id: self.menu_section_id,
            prices: self.prices,
            available: self.available,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SetPlacementRequest {
    pub(super) tenant_id: String,
    #[serde(default)]
    pub(super) menu_section_id: Option<String>,
    pub(super) prices: Vec<ChannelPrice>,
    pub(super) available: bool,
}
