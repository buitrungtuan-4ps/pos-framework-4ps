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
//! **Only what has a reader.** The session carries ten capability flags and the till could be handed
//! all of them; the rest would arrive with no reader, which is the failure this repository has
//! shipped repeatedly (`docs/roadmap-v3.md` Cadence). A flag joins this response in the change that
//! consumes it, and B5.3 is where the rest arrive with their gates.
//!
//! `seats_enabled` is the third, and arrived under that rule rather than around it: the seat picker
//! that reads it ships in the same change.
//!
//! `tables_enabled` and `kds_enabled` are the fourth and fifth, and arrive under it too. They are
//! the two flags that decide what the till *is*: `docs/ui-ux.md` §3 says the store profile decides
//! the starting screen and the flow — *"same components, different assembly, not three
//! applications"* — and until these rode here the till had no way to be anything but a
//! table-service application. It landed every store on a floor plan and offered every store a
//! kitchen board, including the ones that have neither.
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
use pos_proto::ids::{MenuItemId, ModifierGroupId, TaxClassId};
use pos_proto::menu::{MenuEntry, MenuModifierGroup};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::{SalesChannel, WireEnum as _, locale::TaxRateTable, text::DisplayName};

use crate::app::Edge;

/// The store's price book, as the in-store UI reads it.
///
/// The capability flags are flat and stay flat. `struct_excessive_bools` is a good lint about
/// *arguments* — four positional bools at a call site is a bug waiting to happen — and this struct
/// is never constructed positionally: it is serialized, and each flag is named on the wire by a
/// field a till reads by name. Grouping them under a `capabilities` object is the shape B5.3 will
/// want when the rest arrive, and it is **not available now**: `tips_enabled` and `seats_enabled`
/// are already published, and `AGENTS.md` §2 forbids removing or renaming a published field —
/// nesting them would do exactly that to every till that has not updated yet (ADR-0111's
/// one-directional version rule is what makes an old device safe, and only while this is additive).
/// When a sixth flag needs a reader, the answer is a *new* nested object beside these, with these
/// deprecated rather than moved.
#[expect(
    clippy::struct_excessive_bools,
    reason = "a serialized response whose fields are named on the wire, and whose published flags \
              cannot be regrouped without removing them — see above"
)]
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
    /// Whether this store assigns items to seats (§10 `Capability::Seats`, authored on the
    /// `capabilities` node, and **off by default** — most counters have no seats to speak of).
    ///
    /// Joins this response in the change that consumes it, which is the rule this module's header
    /// sets: `seat` has ridden `sales.order_line.added` and `LineDraft` since they were written, the
    /// add route has accepted it all along, and nothing has ever set it — because the till had no way
    /// to know whether the store wanted to be asked.
    seats_enabled: bool,
    /// Whether this store runs table service (§10 `Capability::Tables`, **on** by default).
    ///
    /// The one flag that decides what the till's home screen is. A counter cafe has no floor plan,
    /// and `Capability::PayFirst` is declared incompatible with this one in the capability model's
    /// own validity rules — so a store with it off being shown a room full of tables it does not
    /// have is not a cosmetic problem, it is the till describing a different shop.
    tables_enabled: bool,
    /// Whether fired lines reach a kitchen display (§10 `Capability::Kds`, on by default).
    ///
    /// Rides with `tables_enabled` because it answers the same question about the same screen: a
    /// destination in the status bar that leads to a board this store does not run is an offer the
    /// store cannot honour, which is the same failure `tips_enabled` was added to stop.
    kds_enabled: bool,
    /// The payment methods this store accepts, as their wire names, or `None` when the store
    /// restricts nothing and every method is on ([ADR-0080](../../../docs/adr/0080-channels-and-tender.md)).
    ///
    /// `None` rather than "all seven listed" so the till can tell "no restriction published" from "a
    /// restriction that happens to allow everything" — and so a method added to the enum later is
    /// accepted by an unrestricted store without a config change.
    accepted_tender: Option<Vec<&'static str>>,
    /// Every modifier group any item above attaches, listed once
    /// ([ADR-0127](../../../docs/adr/0127-modifier-groups-reach-the-edge.md)).
    ///
    /// Empty on a store that has published none, which is every store until the console attaches
    /// one — and the till then asks nothing, exactly as it did before this field existed.
    modifier_groups: Vec<ModifierGroupResponse>,
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
    /// The modifier groups the till must ask about before this item is sold
    /// ([ADR-0127](../../../docs/adr/0127-modifier-groups-reach-the-edge.md)).
    ///
    /// Ids, not copies: the groups themselves are listed once on the response below, because one
    /// "Size" group attached to forty pizzas would otherwise be forty copies of the same rule on
    /// every menu read.
    modifier_group_ids: Vec<ModifierGroupId>,
    /// Whether the item can be sold right now. An item present but 86'd is shown and refused, not
    /// hidden, so staff can see why it cannot be ordered.
    available: bool,
}

/// One modifier group, as the till asks it.
#[derive(Debug, Serialize)]
pub(crate) struct ModifierGroupResponse {
    /// The group's identifier, which an item above names.
    modifier_group_id: ModifierGroupId,
    /// The name to show, already in the store's display language — the same resolution the item's
    /// own name gets ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md)).
    display_name: DisplayName,
    /// How many choices a guest must make. **Zero means optional**; the till has no separate
    /// `required` flag to disagree with this number, and neither does the book.
    min_select: u16,
    /// How many choices a guest may make at most.
    max_select: u16,
    /// The items offered as choices. Each is an ordinary item in `items` above, with its own price
    /// — which is how a large pizza costs more than a small one without a second pricing concept.
    member_menu_item_ids: Vec<MenuItemId>,
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
            modifier_group_ids: entry.modifier_group_ids.clone(),
            available: entry.available && tax_rate.is_some(),
            tax_rate,
        }
    }
}

impl ModifierGroupResponse {
    /// One published group, with its name resolved for the store's display language.
    fn from_group(group: &MenuModifierGroup, locale: &str) -> Self {
        Self {
            modifier_group_id: group.modifier_group_id,
            display_name: group.localized_name(locale).clone(),
            min_select: group.min_select,
            max_select: group.max_select,
            member_menu_item_ids: group.member_menu_item_ids.clone(),
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
    let language = session.display_language.clone().unwrap_or_default();
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
            seats_enabled: session.capabilities.enabled(Capability::Seats),
            tables_enabled: session.capabilities.enabled(Capability::Tables),
            kds_enabled: session.capabilities.enabled(Capability::Kds),
            accepted_tender: session
                .accepted_tender
                .as_ref()
                .map(|methods| methods.iter().map(|method| method.as_wire()).collect()),
            // The same resolution the reason-code and QR routes use: the store's language, or the
            // empty string, which every `localized_name` treats as "no translation, use the base".
            modifier_groups: session
                .menu
                .modifier_groups()
                .iter()
                .map(|group| ModifierGroupResponse::from_group(group, &language))
                .collect(),
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
            modifier_group_ids: Vec::new(),
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
