// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Repricing an inbound order line from the store's menu catalog
//! ([ADR-0063](../../../docs/adr/0063-store-menu-catalog.md)).
//!
//! The [`MenuCatalog`] itself is a serializable config shape in `pos-proto` (it crosses the wire).
//! This module is the *logic* that consumes it: turning `(menu_item_id, quantity, modifiers)` into a
//! priced line the way a device does for a dine-in order, so the store can accept an **inbound** order
//! — a marketplace order, `POST /v1/orders`, a QR guest — that arrives with identifiers and nothing
//! else.
//!
//! It is what makes the `OrderIn` contract's two hard rules honourable at the store
//! ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §5):
//!
//! * **The store's price wins.** The price comes from the catalog; the caller's quote is compared and
//!   reported in [`PricedLine::repriced`], never charged.
//! * **An unknown item is refused, not substituted.** No fuzzy match, no nearest neighbour — an item
//!   the catalog does not carry is a [`RepriceError::UnknownItem`], which the caller turns into an
//!   `invalid_argument`.
//!
//! Pure and sans-I/O like the rest of the domain: no clock, no store, no `pos-ports`
//! ([ADR-0013](../../../docs/adr/0013-async-strategy.md)). The edge maps a [`RepriceError`] to a
//! `PortError`; this module never names one.

use pos_proto::locale::TaxRateTable;
use pos_proto::menu::MenuCatalog;
use pos_proto::money::{Money, MoneyError, Ratio, Rounding};
use pos_proto::{DisplayName, MenuItemId, Quantity, SalesChannel, TaxClassId, WireEnum};

/// One line an inbound order asks for, before the store has priced it.
///
/// Channel-agnostic and free of any menu amount: the caller names the item, how many, and which
/// modifiers, and may attach the price it *believes* applies — but that quote is advisory (see
/// [`PricedLine::repriced`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestedLine {
    /// The item ordered — an identifier the catalog must recognise.
    pub menu_item_id: MenuItemId,
    /// How many, in thousandths, so a half portion is expressible.
    pub quantity: Quantity,
    /// Chosen modifiers, each itself a catalog item whose price adds to the line.
    pub modifier_menu_item_ids: Vec<MenuItemId>,
    /// The unit price the caller believes applies. Advisory: compared against the store's price and
    /// reported, never honoured.
    pub quoted_unit_price: Option<Money>,
}

/// A line the store has priced from its own catalog — exactly the amounts a `sales.order_line.added`
/// event captures, so the caller can record it the way a device-priced line is recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PricedLine {
    /// The item, for reporting.
    pub menu_item_id: MenuItemId,
    /// The name to show, captured now so the line never re-reads the live menu.
    pub display_name: DisplayName,
    /// How many.
    pub quantity: Quantity,
    /// The store's unit price: the base item plus every chosen modifier.
    pub unit_price: Money,
    /// The extended total: `unit_price × quantity`, rounded half-up.
    pub line_total: Money,
    /// The tax class, which keys the line into a rate on the bill.
    pub tax_class_id: TaxClassId,
    /// The tax rate in force for this class on this channel, as a ratio for money arithmetic.
    pub tax_rate: Ratio,
    /// Whether the caller's quoted unit price differed from the store's. Reported, not refused: a
    /// stale quote loses a sale if refused and loses margin if honoured, so it is only surfaced.
    pub repriced: bool,
}

/// Why a line could not be priced from the catalog.
///
/// Each variant is a distinct refusal the caller maps to a distinct error class, so a marketplace
/// learns *why* — an unknown item is its bug to fix, an unavailable item is a transient store state,
/// a missing rate is the operator's misconfiguration.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepriceError {
    /// The item (or one of its modifiers) is not in the store's catalog. Refused, never substituted
    /// ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §5 rule 3) — maps to `invalid_argument`.
    UnknownItem(MenuItemId),
    /// The item is in the catalog but 86'd right now. The store will not promise a dish it cannot
    /// make — maps to `failed_precondition`.
    Unavailable(MenuItemId),
    /// No tax rate is configured for the item's class on this channel. Reuses
    /// [`TaxRateTable`]'s deliberate "a missing rate is a configuration error, not a silent zero"
    /// rule — maps to `failed_precondition`.
    MissingRate {
        /// The class with no rate on this channel.
        tax_class_id: TaxClassId,
        /// The channel it was looked up on.
        sales_channel: SalesChannel,
    },
    /// Money arithmetic overflowed, or a modifier was priced in a different currency than the base
    /// item (a catalog a store should never publish). Wraps the primitive's own error.
    Money(MoneyError),
}

impl core::fmt::Display for RepriceError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownItem(id) => write!(formatter, "the store does not sell menu item {id}"),
            Self::Unavailable(id) => write!(formatter, "menu item {id} is not available right now"),
            Self::MissingRate {
                tax_class_id,
                sales_channel,
            } => write!(
                formatter,
                "no tax rate is configured for class {tax_class_id} on channel {}",
                sales_channel.as_wire()
            ),
            Self::Money(error) => write!(formatter, "the line could not be priced: {error}"),
        }
    }
}

impl core::error::Error for RepriceError {}

impl From<MoneyError> for RepriceError {
    fn from(error: MoneyError) -> Self {
        Self::Money(error)
    }
}

/// Prices one requested line from the store's catalog and its channel-keyed rate table.
///
/// The base item's price, plus each chosen modifier's, is the unit price; the extended total is that
/// times the quantity; the tax rate is the base item's class looked up on the order's channel. An
/// unknown or 86'd item — base or modifier — refuses the whole line.
///
/// # Errors
///
/// [`RepriceError`] — the item or a modifier is unknown or unavailable, no rate is configured for the
/// class on this channel, or the money arithmetic overflowed.
pub fn reprice_line(
    catalog: &MenuCatalog,
    rates: &TaxRateTable,
    sales_channel: SalesChannel,
    line: &RequestedLine,
) -> Result<PricedLine, RepriceError> {
    let base = catalog
        .get(line.menu_item_id)
        .ok_or(RepriceError::UnknownItem(line.menu_item_id))?;
    if !base.available {
        return Err(RepriceError::Unavailable(base.menu_item_id));
    }

    // A modifier is a catalog item too: choosing it adds its price to the unit price, and an unknown
    // or 86'd modifier refuses the line exactly as an unknown base item does. `checked_add` also
    // rejects a modifier priced in another currency, which folds into `Money`.
    let mut unit_price = base.unit_price;
    for modifier_id in &line.modifier_menu_item_ids {
        let modifier = catalog
            .get(*modifier_id)
            .ok_or(RepriceError::UnknownItem(*modifier_id))?;
        if !modifier.available {
            return Err(RepriceError::Unavailable(*modifier_id));
        }
        unit_price = unit_price.checked_add(modifier.unit_price)?;
    }

    let line_total = unit_price.mul_quantity(line.quantity, Rounding::HalfUp)?;

    let tax_rate =
        rates
            .rate_for(base.tax_class_id, sales_channel)
            .ok_or(RepriceError::MissingRate {
                tax_class_id: base.tax_class_id,
                sales_channel,
            })?;

    let repriced = line
        .quoted_unit_price
        .is_some_and(|quoted| quoted != unit_price);

    Ok(PricedLine {
        menu_item_id: base.menu_item_id,
        display_name: base.display_name.clone(),
        quantity: line.quantity,
        unit_price,
        line_total,
        tax_class_id: base.tax_class_id,
        tax_rate: tax_rate.as_ratio(),
        repriced,
    })
}

#[cfg(test)]
mod tests {
    use super::{RepriceError, RequestedLine, reprice_line};
    use pos_proto::locale::{TaxRate, TaxRateTable};
    use pos_proto::menu::{MenuCatalog, MenuEntry};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::{DisplayName, MenuItemId, Quantity, SalesChannel, TaxClassId, Ulid};

    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn vnd(minor: i64) -> Money {
        Money::new(CurrencyCode::VND, minor)
    }

    fn food() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(1))
    }

    /// A store carrying a 150,000₫ Margherita (item 500) and a 20,000₫ "extra cheese" modifier
    /// (item 600), both food class.
    fn catalog() -> MenuCatalog {
        MenuCatalog::new()
            .with(MenuEntry::new(
                item(500),
                DisplayName::new("Margherita"),
                vnd(150_000),
                food(),
            ))
            .with(MenuEntry::new(
                item(600),
                DisplayName::new("Extra cheese"),
                vnd(20_000),
                food(),
            ))
    }

    /// Food is 10% dine-in.
    fn rates() -> TaxRateTable {
        TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10))
    }

    fn line(menu_item_id: MenuItemId, quantity: Quantity) -> RequestedLine {
        RequestedLine {
            menu_item_id,
            quantity,
            modifier_menu_item_ids: Vec::new(),
            quoted_unit_price: None,
        }
    }

    #[test]
    fn a_known_item_prices_at_the_store_price_extended_by_quantity() {
        let priced = reprice_line(
            &catalog(),
            &rates(),
            SalesChannel::DineIn,
            &line(item(500), Quantity::from_whole(2).expect("valid")),
        )
        .expect("prices");

        assert_eq!(priced.unit_price, vnd(150_000));
        assert_eq!(priced.line_total, vnd(300_000), "two at 150,000");
        assert_eq!(priced.tax_class_id, food());
        assert_eq!(priced.display_name.as_str(), "Margherita");
        assert!(
            !priced.repriced,
            "no quote was given, so nothing to reprice against"
        );
    }

    #[test]
    fn an_unknown_item_is_refused_never_substituted() {
        let error = reprice_line(
            &catalog(),
            &rates(),
            SalesChannel::DineIn,
            &line(item(999), Quantity::ONE),
        )
        .expect_err("the store does not sell it");
        assert_eq!(error, RepriceError::UnknownItem(item(999)));
    }

    #[test]
    fn an_eighty_sixed_item_is_refused() {
        let catalog = MenuCatalog::new().with(
            MenuEntry::new(
                item(500),
                DisplayName::new("Margherita"),
                vnd(150_000),
                food(),
            )
            .out_of_stock(),
        );
        let error = reprice_line(
            &catalog,
            &rates(),
            SalesChannel::DineIn,
            &line(item(500), Quantity::ONE),
        )
        .expect_err("out of stock");
        assert_eq!(error, RepriceError::Unavailable(item(500)));
    }

    #[test]
    fn a_modifier_adds_its_price_to_the_unit() {
        let mut requested = line(item(500), Quantity::ONE);
        requested.modifier_menu_item_ids = vec![item(600)];
        let priced =
            reprice_line(&catalog(), &rates(), SalesChannel::DineIn, &requested).expect("prices");
        assert_eq!(
            priced.unit_price,
            vnd(170_000),
            "150,000 base + 20,000 cheese"
        );
        assert_eq!(priced.line_total, vnd(170_000));
    }

    #[test]
    fn an_unknown_modifier_refuses_the_whole_line() {
        let mut requested = line(item(500), Quantity::ONE);
        requested.modifier_menu_item_ids = vec![item(999)];
        let error = reprice_line(&catalog(), &rates(), SalesChannel::DineIn, &requested)
            .expect_err("the modifier is not sold");
        assert_eq!(error, RepriceError::UnknownItem(item(999)));
    }

    #[test]
    fn a_differing_quote_is_reported_not_honoured() {
        let mut requested = line(item(500), Quantity::ONE);
        requested.quoted_unit_price = Some(vnd(140_000)); // marketplace's stale cached price
        let priced =
            reprice_line(&catalog(), &rates(), SalesChannel::DineIn, &requested).expect("prices");
        assert_eq!(priced.unit_price, vnd(150_000), "the store's price wins");
        assert!(priced.repriced, "the differing quote is flagged");

        requested.quoted_unit_price = Some(vnd(150_000)); // a quote that matches
        let priced =
            reprice_line(&catalog(), &rates(), SalesChannel::DineIn, &requested).expect("prices");
        assert!(!priced.repriced, "a matching quote is not a repricing");
    }

    #[test]
    fn a_missing_rate_on_the_channel_is_a_configuration_error_not_a_silent_zero() {
        // The catalog has the item, but the rate table has no row for its class on Takeaway.
        let error = reprice_line(
            &catalog(),
            &rates(),
            SalesChannel::Takeaway,
            &line(item(500), Quantity::ONE),
        )
        .expect_err("no takeaway rate configured");
        assert_eq!(
            error,
            RepriceError::MissingRate {
                tax_class_id: food(),
                sales_channel: SalesChannel::Takeaway,
            }
        );
    }

    #[test]
    fn the_channel_selects_the_rate() {
        // Same item, same catalog: dine-in has a rate, takeaway does not. The channel keys the
        // lookup, which is the whole reason the rate table has a channel dimension.
        assert!(
            reprice_line(
                &catalog(),
                &rates(),
                SalesChannel::DineIn,
                &line(item(500), Quantity::ONE)
            )
            .is_ok()
        );
        assert!(
            reprice_line(
                &catalog(),
                &rates(),
                SalesChannel::Takeaway,
                &line(item(500), Quantity::ONE)
            )
            .is_err()
        );
    }

    #[test]
    fn a_fractional_quantity_scales_the_line_total() {
        let priced = reprice_line(
            &catalog(),
            &rates(),
            SalesChannel::DineIn,
            &line(item(500), Quantity::HALF),
        )
        .expect("prices");
        assert_eq!(priced.line_total, vnd(75_000), "half of 150,000");
    }
}
