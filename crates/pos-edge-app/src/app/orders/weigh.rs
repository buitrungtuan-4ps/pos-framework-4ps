// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A weighed item's line from the scale beside the till or a weight typed by hand
//! ([ADR-0223](../../../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md) decision 2).
//!
//! The edge reads the scale through the `Scale` port, because a browser cannot reach a serial or
//! USB scale on every device class. A settled reading less the item's tare is the line's net
//! grams; a moving, overloaded or below-zero reading, or a scale that does not answer, is refused
//! and never read as a number. A typed weight is net already, and is taken only where the store's
//! `weighing.manual_entry`, seeded by its country pack, allows it and the pack does not forbid it. A reading is never edited: the
//! line records where its weight came from.

use std::future::Future;
use std::pin::Pin;

use pos_core::decision::Actor;
use pos_core::weighing::{WeighError, net_quantity};
use pos_ports::event_store::EventStore;
use pos_ports::scale::{Scale, ScaleReading};
use pos_ports::{PortError, PortName};
use pos_proto::ids::{MenuItemId, OrderId};
use pos_proto::weighing::WeightSource;

use crate::app::{AppError, Edge, LineView, OrderLineChoice};

/// Where a weighed line's weight comes from, as the till asks for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeighFrom {
    /// The scale beside the till, read now.
    Scale,
    /// The net grams a person typed.
    Manual {
        /// The net weight typed, in grams.
        net_grams: u32,
    },
}

/// The scale an edge reads, behind a box so the edge needs no type parameter for it.
pub(crate) type BoxedReading<'a> =
    Pin<Box<dyn Future<Output = Result<ScaleReading, PortError>> + Send + 'a>>;

/// A [`Scale`] the edge can hold as a trait object: the port returns `impl Future`, which a
/// trait object cannot.
pub(crate) trait DynScale: Send + Sync + std::fmt::Debug {
    /// [`Scale::read`], boxed.
    fn read_boxed(&self) -> BoxedReading<'_>;
}

impl<T: Scale + std::fmt::Debug> DynScale for T {
    fn read_boxed(&self) -> BoxedReading<'_> {
        Box::pin(self.read())
    }
}

/// The scale of an edge composed with none: every read answers that no scale is connected, so a
/// line is never priced at nothing (the port's contract, rule 4).
#[derive(Debug)]
pub(crate) struct NoScale;

impl Scale for NoScale {
    async fn read(&self) -> Result<ScaleReading, PortError> {
        Err(PortError::unavailable(
            PortName::Scale,
            "no scale is connected to this store",
        ))
    }
}

impl<S: EventStore> Edge<S> {
    /// What the scale beside the till says now, for the till to show before it weighs.
    ///
    /// # Errors
    ///
    /// [`AppError::ScaleAbsent`] if no scale answers.
    pub async fn read_scale(&self) -> Result<ScaleReading, AppError> {
        self.scale.read_boxed().await.map_err(|error| {
            tracing::warn!(%error, "the scale did not answer");
            AppError::ScaleAbsent
        })
    }

    /// Adds a line of the weighed item `menu_item_id` to `order_id`, weighed `from` the scale or
    /// a hand, priced by the edge at its price per kilogram.
    ///
    /// # Errors
    ///
    /// [`AppError::Weighing`] for an item sold by the piece or a net weight out of bounds,
    /// [`AppError::ManualWeightNotAllowed`] for a typed weight where the store refuses one,
    /// [`AppError::ScaleNotSettled`] for a reading that is moving, overloaded or below zero,
    /// [`AppError::ScaleAbsent`] if no scale answers, or what adding the line refuses.
    pub async fn add_weighed_line(
        &self,
        actor: Actor,
        order_id: OrderId,
        menu_item_id: MenuItemId,
        from: WeighFrom,
    ) -> Result<LineView, AppError> {
        let session = self.session();
        let tare_grams = match session.menu.get(menu_item_id) {
            Some(entry) if entry.codes.sold_by_weight() => entry.codes.tare_grams,
            Some(_) => return Err(AppError::Weighing(WeighError::NotSoldByWeight)),
            None => return Err(AppError::ItemNotSellable),
        };
        let min_grams = session.weighing.min_grams();
        let (quantity, source) = match from {
            WeighFrom::Manual { net_grams } => {
                if !self.manual_weight_allowed() {
                    return Err(AppError::ManualWeightNotAllowed);
                }
                let quantity =
                    net_quantity(net_grams, None, min_grams).map_err(AppError::Weighing)?;
                (quantity, WeightSource::Manual)
            }
            WeighFrom::Scale => match self.read_scale().await? {
                ScaleReading::Stable { net_grams } => {
                    let quantity = net_quantity(net_grams, tare_grams, min_grams)
                        .map_err(AppError::Weighing)?;
                    (quantity, WeightSource::Scale)
                }
                _ => return Err(AppError::ScaleNotSettled),
            },
        };
        let choice = OrderLineChoice {
            menu_item_id,
            quantity,
            modifier_menu_item_ids: Vec::new(),
            seat: None,
            course_id: None,
            note_present: false,
            typed_unit_price: None,
            weight_source: Some(source),
        };
        self.add_noted_line_to_order(actor, order_id, choice, None)
            .await
    }
}
