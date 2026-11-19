// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An open-price line on the table path
//! ([ADR-0222](../../../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decision 4): the device sends the price the cashier typed, and the edge prices the line from it
//! only within the item's published bounds, with `sales.line.add` alone. Outside them is a price
//! override (ADR-0219). Every other item keeps the table path's device-copied price.

use pos_core::menu::{RequestedLine, reprice_line};

use crate::app::{AppError, EdgeSession, LineDraft};

/// `draft` priced by the edge where its item is open-price: the typed price of one is the device's
/// unit price less its modifiers', and must lie within the item's bounds.
///
/// # Errors
///
/// [`AppError::Domain`] with `PRICE_OUT_OF_RANGE` for a price outside the bounds, or the
/// repricing's own refusals.
pub(in crate::app) fn priced_open(
    session: &EdgeSession,
    mut draft: LineDraft,
) -> Result<LineDraft, AppError> {
    let open = session
        .menu
        .get(draft.menu_item_id)
        .is_some_and(|entry| entry.codes.open_price.is_some());
    if !open {
        return Ok(draft);
    }
    let mut typed = draft.unit_price;
    for modifier in draft
        .modifier_menu_item_ids
        .iter()
        .filter_map(|id| session.menu.get(*id))
    {
        typed = typed
            .checked_sub(modifier.unit_price)
            .map_err(|error| AppError::Domain(error.into()))?;
    }
    let requested = RequestedLine {
        menu_item_id: draft.menu_item_id,
        quantity: draft.quantity,
        modifier_menu_item_ids: draft.modifier_menu_item_ids.clone(),
        quoted_unit_price: None,
        typed_unit_price: Some(typed),
        weight_source: None,
    };
    let priced = reprice_line(
        &session.menu,
        &session.tax_rates,
        session.sales_channel,
        &requested,
    )
    .map_err(AppError::from_reprice)?;
    draft.unit_price = priced.unit_price;
    draft.line_total = priced.line_total;
    Ok(draft)
}
