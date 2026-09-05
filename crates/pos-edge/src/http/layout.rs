// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The layout read route ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md),
//! production-readiness **C4**).
//!
//! `GET /api/layout` serves how the till should group and order its buttons: the display categories,
//! their sub-categories, and each item's caption and grid slot, from the live
//! [`EdgeSession`](crate::app::EdgeSession) the config-pull rebuilds.
//!
//! # Its own route, because it is its own node
//!
//! The catalog compiles to two artifacts and ADR-0066 delivers them on two configuration nodes for a
//! reason: `menu` is what the domain reprices from and `layout` is what a screen draws, so a price
//! change relays no buttons and a button moving reprices nothing. Folding the plan into
//! `GET /api/menu` would re-entangle at the last hop what the whole design keeps apart.
//!
//! Until this route the `layout` node was authored in the console, validated by the cloud, versioned
//! into the config tree and published to the store — and read by nobody, so every till in the fleet
//! drew the flat price book whatever an operator arranged.
//!
//! # An empty plan is not an empty screen
//!
//! A store that has laid nothing out sends no categories, and the till draws the flat price book —
//! what it drew before this existed. That is the difference between "the console has arranged
//! nothing" and "the console has arranged nothing to show", and only the first is an ordinary state.
//!
//! # Captions, not prices
//!
//! A button carries the label an operator wrote, which may be shorter than the item's catalog name on
//! a crowded grid. It carries no price: the till already holds the price book keyed by
//! `menu_item_id`, and sending a second copy here would create two prices that can disagree.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_ports::event_store::EventStore;
use pos_proto::display::{DisplayButton, DisplayCategory, DisplayPlan};
use pos_proto::ids::{DisplayCategoryId, DisplaySubcategoryId, MenuItemId};
use pos_proto::text::DisplayName;

use crate::app::Edge;

/// The store's presentation plan for this channel, as the in-store UI reads it.
#[derive(Debug, Serialize)]
pub(crate) struct LayoutResponse {
    /// The display categories, in the order the console arranged them. Empty when nothing has been
    /// laid out, which the till reads as "draw the flat price book".
    categories: Vec<CategoryResponse>,
}

/// One display category: a tab or section on the till.
#[derive(Debug, Serialize)]
pub(crate) struct CategoryResponse {
    display_category_id: DisplayCategoryId,
    /// The name to show on the tab.
    name: DisplayName,
    /// Buttons placed directly under the category — a flat category has only these.
    buttons: Vec<ButtonResponse>,
    /// Nested sub-categories, in display order.
    subcategories: Vec<SubcategoryResponse>,
}

/// One display sub-category: a second grouping level with its own buttons.
#[derive(Debug, Serialize)]
pub(crate) struct SubcategoryResponse {
    display_subcategory_id: DisplaySubcategoryId,
    name: DisplayName,
    buttons: Vec<ButtonResponse>,
}

/// One item's button.
#[derive(Debug, Serialize)]
pub(crate) struct ButtonResponse {
    /// The item this button orders — the id the price book from `GET /api/menu` carries. Layout
    /// names the item; the price book prices it; the two meet only here.
    menu_item_id: MenuItemId,
    /// The caption to show, which may be shorter than the item's catalog name.
    label: DisplayName,
    /// The button's zero-based column on a fixed grid, absent for a flowing layout where order alone
    /// places it.
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<u16>,
    /// The button's zero-based row, absent for the same reason as `column`.
    #[serde(skip_serializing_if = "Option::is_none")]
    row: Option<u16>,
}

impl From<&DisplayButton> for ButtonResponse {
    fn from(button: &DisplayButton) -> Self {
        Self {
            menu_item_id: button.menu_item_id,
            label: button.label.clone(),
            column: button.position.map(|position| position.column),
            row: button.position.map(|position| position.row),
        }
    }
}

impl From<&DisplayCategory> for CategoryResponse {
    fn from(category: &DisplayCategory) -> Self {
        Self {
            display_category_id: category.display_category_id,
            name: category.name.clone(),
            buttons: category.buttons.iter().map(ButtonResponse::from).collect(),
            subcategories: category
                .subcategories
                .iter()
                .map(|subcategory| SubcategoryResponse {
                    display_subcategory_id: subcategory.display_subcategory_id,
                    name: subcategory.name.clone(),
                    buttons: subcategory
                        .buttons
                        .iter()
                        .map(ButtonResponse::from)
                        .collect(),
                })
                .collect(),
        }
    }
}

/// Renders a plan for the wire.
fn respond_with(plan: &DisplayPlan) -> LayoutResponse {
    LayoutResponse {
        categories: plan
            .categories()
            .iter()
            .map(CategoryResponse::from)
            .collect(),
    }
}

/// `GET /api/layout` — the store's published presentation plan, read from the live session.
pub(crate) async fn plan<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    (StatusCode::OK, Json(respond_with(&edge.session().layout))).into_response()
}

#[cfg(test)]
mod tests {
    use super::respond_with;
    use pos_proto::display::{
        DisplayButton, DisplayCategory, DisplayPlan, DisplaySubcategory, GridPosition,
    };
    use pos_proto::ids::{DisplayCategoryId, DisplaySubcategoryId, MenuItemId};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;

    fn button(seed: u128, position: Option<GridPosition>) -> DisplayButton {
        DisplayButton {
            menu_item_id: MenuItemId::new(Ulid::from_u128(seed)),
            label: DisplayName::new("Margherita"),
            position,
        }
    }

    #[test]
    fn a_store_that_has_laid_nothing_out_sends_no_categories() {
        // Not an error and not an empty screen: the till draws the flat price book, which is what it
        // drew before this route existed (C4).
        let response = respond_with(&DisplayPlan::new());
        assert!(response.categories.is_empty());
    }

    #[test]
    fn a_categorys_buttons_and_subcategories_keep_the_order_the_console_arranged() {
        let plan = DisplayPlan::new().with(DisplayCategory {
            display_category_id: DisplayCategoryId::new(Ulid::from_u128(1)),
            name: DisplayName::new("Pizza"),
            buttons: vec![button(10, None), button(11, None)],
            subcategories: vec![DisplaySubcategory {
                display_subcategory_id: DisplaySubcategoryId::new(Ulid::from_u128(2)),
                name: DisplayName::new("Classics"),
                buttons: vec![button(12, None)],
            }],
        });
        let response = respond_with(&plan);
        let category = response.categories.first().expect("one category");
        assert_eq!(category.name.as_str(), "Pizza");
        let ids: Vec<String> = category
            .buttons
            .iter()
            .map(|button| button.menu_item_id.to_string())
            .collect();
        assert_eq!(ids.len(), 2, "both buttons, in the arranged order");
        assert_eq!(ids[0], MenuItemId::new(Ulid::from_u128(10)).to_string());
        assert_eq!(
            category
                .subcategories
                .first()
                .expect("one sub-category")
                .buttons
                .len(),
            1
        );
    }

    #[test]
    fn a_grid_slot_survives_the_wire_and_a_flowing_button_carries_none() {
        // A terminal places by column and row; a tablet places by order. Sending `0, 0` for a flowing
        // button would pin every one of them to the top-left.
        let plan = DisplayPlan::new().with(DisplayCategory {
            display_category_id: DisplayCategoryId::new(Ulid::from_u128(1)),
            name: DisplayName::new("Pizza"),
            buttons: vec![
                button(10, Some(GridPosition { column: 2, row: 3 })),
                button(11, None),
            ],
            subcategories: Vec::new(),
        });
        let response = respond_with(&plan);
        let buttons = &response.categories.first().expect("a category").buttons;
        assert_eq!(buttons[0].column, Some(2));
        assert_eq!(buttons[0].row, Some(3));
        assert_eq!(buttons[1].column, None);
        assert_eq!(buttons[1].row, None);
    }
}
