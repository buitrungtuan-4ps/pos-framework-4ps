// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A menu checked against the store it is published to, the way a fee is
//! ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md)).
//!
//! The menu compiler checks a menu against itself, not against a store. Two mistakes therefore
//! published cleanly and failed later, at the till, during service: a price in a currency other
//! than the one the store bills in, and an item sold on a channel where the store has no tax rate
//! for its class. Each fails every sale of that item, as the same faults fail every bill a fee
//! applies to, so a menu with either one is refused before it is written. The faults are the fee
//! check's own [`StoreFault`]s, read from the same [`StoreFacts`].

use std::collections::BTreeSet;

use pos_proto::MenuCatalog;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::MenuItemId;
use pos_proto::wire_enum::Open;

use crate::fees::{StoreFacts, StoreFault};

/// One item of a menu that the store cannot sell, and why.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MenuFault {
    /// The item, as its placement names it.
    pub menu_item_id: MenuItemId,
    /// The item's name, for the person who has to fix it.
    pub name: String,
    /// What is wrong.
    pub fault: StoreFault,
}

/// What is wrong with the menu `facts` holds at that store, each item and fault once, in item
/// order. Nothing for a store with no menu, and nothing is checked against a fact the store does
/// not have, as for a fee: a store that has no `tax` node yet cannot be short of a rate in it.
#[must_use]
pub fn menu_faults(facts: &StoreFacts) -> Vec<MenuFault> {
    let Some(menu) = facts.menu.as_ref() else {
        return Vec::new();
    };
    let mut faults = BTreeSet::new();
    // Each catalog with the channels it is sold on: its own channel for a channel's row, and for
    // the fallback, every channel the tax table rates that has no row of its own.
    let own: Vec<SalesChannel> = known(menu.channels().iter().map(|row| &row.sales_channel));
    let mut sold: Vec<(&MenuCatalog, Vec<SalesChannel>)> = menu
        .channels()
        .iter()
        .map(|row| (&row.catalog, known([&row.sales_channel])))
        .collect();
    let rated: Vec<SalesChannel> = facts.tax_rates.as_ref().map_or_else(Vec::new, |rates| {
        known(rates.rows().iter().map(|row| &row.sales_channel))
    });
    let fallback_channels = rated.into_iter().filter(|c| !own.contains(c)).collect();
    sold.push((menu.fallback(), fallback_channels));
    for (catalog, channels) in sold {
        for entry in catalog.items() {
            let fault = |fault| MenuFault {
                menu_item_id: entry.menu_item_id,
                name: entry.display_name.as_str().to_owned(),
                fault,
            };
            if facts
                .currency
                .is_some_and(|currency| entry.unit_price.currency_code != currency)
            {
                faults.insert(fault(StoreFault::CurrencyMismatch));
            }
            if let Some(rates) = &facts.tax_rates
                && channels
                    .iter()
                    .any(|channel| rates.rate_for(entry.tax_class_id, *channel).is_none())
            {
                faults.insert(fault(StoreFault::TaxClassNotRated));
            }
        }
    }
    faults.into_iter().collect()
}

/// The channels a sale is made on among `channels`: not `UNSPECIFIED`, and not a token this build
/// does not know, de-duplicated.
fn known<'a>(channels: impl IntoIterator<Item = &'a Open<SalesChannel>>) -> Vec<SalesChannel> {
    let set: BTreeSet<SalesChannel> = channels
        .into_iter()
        .filter(|channel| !channel.is_unrecognised())
        .map(Open::known)
        .filter(|channel| *channel != SalesChannel::Unspecified)
        .collect();
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use pos_proto::enums::SalesChannel;
    use pos_proto::ids::{MenuItemId, TaxClassId};
    use pos_proto::locale::{TaxRate, TaxRateTable};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::{MenuBook, MenuCatalog, MenuEntry};

    use super::{MenuFault, menu_faults};
    use crate::fees::{StoreFacts, StoreFault};

    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn class(n: u128) -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(0x7A0 + n))
    }

    fn entry(n: u128, name: &str, currency: &str, tax_class: u128) -> MenuEntry {
        let currency = CurrencyCode::parse(currency).expect("a currency");
        MenuEntry::new(
            item(n),
            DisplayName::new(name),
            Money::new(currency, 100_000),
            class(tax_class),
        )
    }

    /// A VND store that rates class 1 on dine-in and takeaway, and class 2 on dine-in only.
    fn facts(menu: &MenuBook) -> StoreFacts {
        let rate = TaxRate::from_basis_points(800);
        let tax = TaxRateTable::new()
            .with(class(1), SalesChannel::DineIn, rate)
            .with(class(1), SalesChannel::Takeaway, rate)
            .with(class(2), SalesChannel::DineIn, rate);
        StoreFacts::from_document(&serde_json::json!({
            "locale": { "currency_code": "VND", "timezone": "Asia/Ho_Chi_Minh" },
            "tax": serde_json::to_value(&tax).expect("encode the table"),
            "menu": serde_json::to_value(menu).expect("encode the menu"),
        }))
    }

    fn fault(n: u128, name: &str, fault: StoreFault) -> MenuFault {
        MenuFault {
            menu_item_id: item(n),
            name: name.to_owned(),
            fault,
        }
    }

    #[test]
    fn a_menu_the_store_can_sell_has_no_fault() {
        let menu = MenuBook::new()
            .with(
                SalesChannel::DineIn,
                MenuCatalog::new()
                    .with(entry(1, "Margherita", "VND", 1))
                    .with(entry(2, "Wine", "VND", 2)),
            )
            .with(
                SalesChannel::Takeaway,
                MenuCatalog::new().with(entry(1, "Margherita", "VND", 1)),
            );
        assert_eq!(menu_faults(&facts(&menu)), Vec::new());
    }

    #[test]
    fn a_price_in_another_currency_is_a_fault() {
        let menu = MenuBook::new().with(
            SalesChannel::DineIn,
            MenuCatalog::new().with(entry(1, "Margherita", "USD", 1)),
        );
        assert_eq!(
            menu_faults(&facts(&menu)),
            vec![fault(1, "Margherita", StoreFault::CurrencyMismatch)]
        );
    }

    #[test]
    fn an_item_sold_where_its_class_has_no_rate_is_a_fault() {
        // Wine is rated on dine-in only, so selling it for takeaway fails every such sale.
        let menu = MenuBook::new().with(
            SalesChannel::Takeaway,
            MenuCatalog::new().with(entry(2, "Wine", "VND", 2)),
        );
        assert_eq!(
            menu_faults(&facts(&menu)),
            vec![fault(2, "Wine", StoreFault::TaxClassNotRated)]
        );
    }

    #[test]
    fn the_fallback_is_checked_on_each_rated_channel_without_its_own_row() {
        let menu = MenuBook::new()
            .with(
                SalesChannel::DineIn,
                MenuCatalog::new().with(entry(1, "Margherita", "VND", 1)),
            )
            .with_fallback(MenuCatalog::new().with(entry(2, "Wine", "VND", 2)));
        assert_eq!(
            menu_faults(&facts(&menu)),
            vec![fault(2, "Wine", StoreFault::TaxClassNotRated)]
        );
    }

    #[test]
    fn nothing_is_checked_against_a_fact_the_store_does_not_have() {
        let menu = MenuBook::new().with(
            SalesChannel::Takeaway,
            MenuCatalog::new().with(entry(2, "Wine", "USD", 2)),
        );
        let facts = StoreFacts::from_document(&serde_json::json!({
            "menu": serde_json::to_value(&menu).expect("encode the menu"),
        }));
        assert_eq!(menu_faults(&facts), Vec::new());
    }
}
