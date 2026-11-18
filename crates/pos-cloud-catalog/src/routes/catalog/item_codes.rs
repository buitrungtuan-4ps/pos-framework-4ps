// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An item's barcodes, PLU and open price, checked at the write
//! ([ADR-0222](../../../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decision 1): each code well formed, and none another active item of the tenant already carries.
//! A duplicate is refused naming both items, so the person authoring knows which one to change.

use axum::response::Response;
use pos_cloud_api::response::{api_error, api_error_with_details};
use pos_proto::ErrorStatus;
use pos_proto::ids::{MenuItemId, TenantId};
use pos_proto::item_codes::ItemCodes;

use super::catalog_error_response;
use crate::catalog::CatalogStore;
use crate::registry::EntityStatus;

/// The codes `item` (no id yet, for a new one; and its name) is to carry: `requested` once it is well formed and
/// unique among the tenant's other active items, or, where the request names none, what the item
/// already carries, so a console that predates codes does not wipe them.
///
/// # Errors
///
/// The refusal to answer with: `400` for a malformed code, `409` naming both items for a duplicate,
/// or the store's own failure.
pub(super) async fn checked_codes<Cat: CatalogStore>(
    catalog: &Cat,
    tenant_id: TenantId,
    item: (Option<MenuItemId>, &str),
    requested: Option<ItemCodes>,
) -> Result<ItemCodes, Response> {
    let (item, name) = item;
    if let Some(fault) = requested.as_ref().and_then(ItemCodes::fault) {
        return Err(api_error(ErrorStatus::InvalidArgument, fault));
    }
    let wants = requested
        .as_ref()
        .is_some_and(|codes| codes.codes().next().is_some());
    if requested.is_some() && !wants {
        return Ok(requested.unwrap_or_default());
    }
    if requested.is_none() && item.is_none() {
        return Ok(ItemCodes::default());
    }
    let items = catalog
        .list_items(tenant_id)
        .await
        .map_err(|error| catalog_error_response(&error))?;
    let Some(codes) = requested else {
        return Ok(items
            .into_iter()
            .find(|stored| Some(stored.record.menu_item_id) == item)
            .map(|stored| stored.record.codes)
            .unwrap_or_default());
    };
    let clash = items.iter().map(|stored| &stored.record).find_map(|other| {
        let shared = other
            .codes
            .codes()
            .find(|code| codes.codes().any(|own| own == *code))?;
        (Some(other.menu_item_id) != item && other.status == EntityStatus::Active)
            .then(|| (other, shared.to_owned()))
    });
    if let Some((other, code)) = clash {
        let other_id = other.menu_item_id.to_string();
        return Err(api_error_with_details(
            ErrorStatus::FailedPrecondition,
            format!(
                "`{code}` is already the code of {}, so {name} cannot carry it too",
                other.name
            ),
            &[("code", &code), ("menu_item_id", &other_id)],
        ));
    }
    Ok(codes)
}
