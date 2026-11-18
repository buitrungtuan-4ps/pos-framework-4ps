// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A scanned code becomes a line
//! ([ADR-0222](../../../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decisions 3 and 5).
//!
//! A variable-measure label is read by the store's layout (`scan.variable_measure_layout`) through
//! [`pos_core::variable_measure::read`]; its item reference names an item by its PLU. The edge stays
//! the only pricer: a price label is taken only for an open-price item, as a typed price that the
//! line's own check holds to the item's bounds, a fixed-price item refuses one, and a weight label
//! waits for items sold by weight (ADR-0223). Any other code is looked up as it is, by barcode or
//! PLU, and sells one at the menu's price.

use pos_core::decision::Actor;
use pos_core::variable_measure::{self, Measure};
use pos_ports::event_store::EventStore;
use pos_proto::ids::{MenuItemId, OrderId};
use pos_proto::menu::MenuEntry;
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;

use crate::app::{AppError, Edge, LineView, OrderLineChoice};

impl<S: EventStore> Edge<S> {
    /// Adds the line `code` names to `order_id`, priced by the edge.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownCode`] for a code no item carries,
    /// [`AppError::PriceLabelNotOpenPrice`] for a price label on a fixed-price item,
    /// [`AppError::WeightLabelNotSold`] for a weight label, or what adding the line refuses.
    pub async fn add_scanned_line(
        &self,
        actor: Actor,
        order_id: OrderId,
        code: &str,
    ) -> Result<LineView, AppError> {
        let choice = self.scanned_choice(code)?;
        self.add_noted_line_to_order(actor, order_id, choice, None)
            .await
    }

    fn scanned_choice(&self, code: &str) -> Result<OrderLineChoice, AppError> {
        let session = self.session();
        let items = session.menu.items();
        if let Some(label) = variable_measure::read(code, session.scan.variable_measure_layout()) {
            let entry = items
                .iter()
                .find(|entry| {
                    entry
                        .codes
                        .plu
                        .as_deref()
                        .is_some_and(|plu| label.names_plu(plu))
                })
                .ok_or(AppError::UnknownCode)?;
            return match label.measure {
                Measure::PriceMinor(minor) if entry.codes.open_price.is_some() => Ok(one(
                    entry.menu_item_id,
                    Some(Money::new(entry.unit_price.currency_code, minor)),
                )),
                Measure::PriceMinor(_) => Err(AppError::PriceLabelNotOpenPrice),
                Measure::Grams(_) => Err(AppError::WeightLabelNotSold),
            };
        }
        items
            .iter()
            .find(|entry| carries(entry, code))
            .map(|entry| one(entry.menu_item_id, None))
            .ok_or(AppError::UnknownCode)
    }
}

/// Whether `entry` carries `code` as a barcode or as its PLU.
fn carries(entry: &MenuEntry, code: &str) -> bool {
    entry.codes.barcodes.iter().any(|barcode| barcode == code)
        || entry.codes.plu.as_deref() == Some(code)
}

/// One of `menu_item_id`, at `typed_unit_price` where a label gave one.
fn one(menu_item_id: MenuItemId, typed_unit_price: Option<Money>) -> OrderLineChoice {
    OrderLineChoice {
        menu_item_id,
        quantity: Quantity::from_milli(1_000),
        modifier_menu_item_ids: Vec::new(),
        seat: None,
        course_id: None,
        note_present: false,
        typed_unit_price,
    }
}
