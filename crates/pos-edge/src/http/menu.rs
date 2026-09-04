// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The menu read route (roadmap-v3 slice E5,
//! [ADR-0063](../../../docs/adr/0063-store-menu-catalog.md)).
//!
//! `GET /api/menu` serves the store's **own** price book from the live
//! [`EdgeSession`](crate::app::EdgeSession) the config-pull rebuilds — the same catalogue an inbound
//! order is repriced against ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)). Until this route
//! the in-store UI carried a hardcoded list of six pizzas, so publishing a menu from the console
//! changed every channel *except* the till in front of the guest.
//!
//! # The rate comes with the item, and its absence is not a zero
//!
//! A line the till sends carries the tax rate shown to the guest at the moment of the sale, so the
//! route resolves each item's class against the store's rate table for its sales channel and hands
//! the rate over with the price. A class with **no row** yields `None`, never zero:
//! [`TaxRateTable::rate_for`](pos_proto::locale::TaxRateTable::rate_for) is explicit that a missing
//! rate is a configuration error, and quietly charging no tax on an unclassified item is the kind of
//! bug found by an audit rather than a test. The till shows such an item as unsellable instead.
//!
//! # Two store facts ride along with the price book
//!
//! `tips_enabled` and `accepted_tender` are published configuration the till has to obey, and until
//! this route carried them the till obeyed neither. Both were live in the session and read by
//! nobody: the edge refused a tip on a store with the capability off
//! ([`decide_bill`](pos_core::decision::decide_bill)) and refused a method outside
//! `accepted_tender`, but the till had no way to know, so it offered the action and the refusal
//! landed as a `400` in front of the guest. Worse for tips: with no entry field at all, `tip_amount`
//! was zero on every payment a real store took, whatever the capability said.
//!
//! They ride here rather than on a route of their own because they are the same *kind* of fact as a
//! price — published from the console, resolved for this store, refreshed when the price book is —
//! and because the till already reads this route on load, so nothing new has to be called or
//! authorised. `GET /api/session` was the other candidate and is the wrong one: it answers *who is
//! signed in on this device*, which is per-device identity with a per-sign-in lifetime, and hanging
//! store-wide published configuration off it would conflate the two.
//!
//! **Only these two.** The session carries ten capability flags and the till could be handed all of
//! them; nine would arrive with no reader, which is the failure this repository has shipped
//! repeatedly (`docs/roadmap-v3.md` Cadence). A flag joins this response in the change that consumes
//! it, and B5.3 is where the rest arrive with their gates.
//!
//! Empty until the cloud publishes a menu — a store never guesses a price (ADR-0063).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_core::capability::Capability;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{MenuItemId, TaxClassId};
use pos_proto::menu::MenuEntry;
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::{SalesChannel, WireEnum as _, locale::TaxRateTable, text::DisplayName};

use crate::app::Edge;

/// The store's price book, as the in-store UI reads it.
#[derive(Debug, Serialize)]
pub(crate) struct MenuResponse {
    /// The store's currency — every amount below is in it.
    currency: CurrencyCode,
    /// The items, in the order the store published them. Each rate is already resolved for the
    /// channel a walk-in sale arrives on, so the till never picks a channel of its own.
    items: Vec<MenuItemResponse>,
    /// Whether this store takes tips (§10 `Capability::Tips`, authored on the `capabilities` node).
    ///
    /// The till shows no tip entry when this is false, which is the difference between a guest being
    /// offered something the edge will refuse and the action simply not being there.
    tips_enabled: bool,
    /// The payment methods this store accepts, as their wire names, or `None` when the store
    /// restricts nothing and every method is on ([ADR-0080](../../../docs/adr/0080-channels-and-tender.md)).
    ///
    /// `None` rather than "all seven listed" so the till can tell "no restriction published" from "a
    /// restriction that happens to allow everything" — and so a method added to the enum later is
    /// accepted by an unrestricted store without a config change.
    accepted_tender: Option<Vec<&'static str>>,
}

/// One sellable item, priced and taxed as this store sells it.
#[derive(Debug, Serialize)]
pub(crate) struct MenuItemResponse {
    /// The item's identifier, which a line names.
    menu_item_id: MenuItemId,
    /// The name to show the guest, already in the store's display language.
    display_name: DisplayName,
    /// The store's price per unit.
    unit_price: Money,
    /// The tax class, carried onto the line.
    tax_class_id: TaxClassId,
    /// The rate for that class on this channel, or `None` when the store's table has no row —
    /// a configuration error the till surfaces rather than papers over with zero.
    tax_rate: Option<Ratio>,
    /// Whether the item can be sold right now. An item present but 86'd is shown and refused, not
    /// hidden, so staff can see why it cannot be ordered.
    available: bool,
}

impl MenuItemResponse {
    /// Prices one catalogue entry for `channel` against the store's rate table.
    fn from_entry(entry: &MenuEntry, rates: &TaxRateTable, channel: SalesChannel) -> Self {
        let tax_rate = rates
            .rate_for(entry.tax_class_id, channel)
            .map(pos_proto::locale::TaxRate::as_ratio);
        Self {
            menu_item_id: entry.menu_item_id,
            display_name: entry.display_name.clone(),
            unit_price: entry.unit_price,
            tax_class_id: entry.tax_class_id,
            // An item whose class carries no rate cannot be quoted to a guest, so it is not
            // sellable however the catalogue flags it.
            available: entry.available && tax_rate.is_some(),
            tax_rate,
        }
    }
}

/// `GET /api/menu` — the store's published price book, read from the live session.
pub(crate) async fn catalog<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let session = edge.session();
    let channel = session.sales_channel;
    let items = session
        .menu
        .items()
        .iter()
        .map(|entry| MenuItemResponse::from_entry(entry, &session.tax_rates, channel))
        .collect();
    (
        StatusCode::OK,
        Json(MenuResponse {
            currency: session.currency,
            items,
            tips_enabled: session.capabilities.enabled(Capability::Tips),
            accepted_tender: session
                .accepted_tender
                .as_ref()
                .map(|methods| methods.iter().map(|method| method.as_wire()).collect()),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use pos_proto::SalesChannel;
    use pos_proto::ids::{MenuItemId, TaxClassId};
    use pos_proto::locale::{TaxRate, TaxRateTable};
    use pos_proto::menu::MenuEntry;
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;

    use super::MenuItemResponse;

    fn class() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(1))
    }

    fn entry(available: bool) -> MenuEntry {
        MenuEntry {
            menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
            display_name: DisplayName::new("Margherita"),
            display_name_translations: std::collections::BTreeMap::new(),
            unit_price: Money::new(CurrencyCode::VND, 150_000),
            tax_class_id: class(),
            available,
        }
    }

    #[test]
    fn an_items_rate_comes_from_the_stores_table_for_its_channel() {
        let rates =
            TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(10));
        let priced = MenuItemResponse::from_entry(&entry(true), &rates, SalesChannel::DineIn);
        assert!(priced.available);
        assert!(
            priced.tax_rate.is_some(),
            "a classified item carries its rate"
        );
    }

    #[test]
    fn an_unclassified_item_is_not_sellable_rather_than_taxed_at_zero() {
        // The rule this guards: a class with no row is a configuration error, and quoting the guest
        // zero tax on it is the kind of bug an audit finds. The till shows the item and refuses it.
        let priced =
            MenuItemResponse::from_entry(&entry(true), &TaxRateTable::new(), SalesChannel::DineIn);
        assert!(priced.tax_rate.is_none());
        assert!(!priced.available);
    }

    #[test]
    fn a_rate_published_for_another_channel_does_not_price_this_one() {
        let rates = TaxRateTable::new().with(class(), SalesChannel::Qr, TaxRate::from_percent(10));
        let priced = MenuItemResponse::from_entry(&entry(true), &rates, SalesChannel::DineIn);
        assert!(priced.tax_rate.is_none());
        assert!(!priced.available);
    }

    #[test]
    fn an_86d_item_stays_unavailable_even_with_a_rate() {
        let rates =
            TaxRateTable::new().with(class(), SalesChannel::DineIn, TaxRate::from_percent(10));
        let priced = MenuItemResponse::from_entry(&entry(false), &rates, SalesChannel::DineIn);
        assert!(priced.tax_rate.is_some(), "the rate is still reported");
        assert!(!priced.available, "an 86'd item is not sellable");
    }
}
