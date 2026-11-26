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
use pos_proto::weighing::WeightSource;
use pos_proto::{DisplayName, MenuItemId, Quantity, SalesChannel, TaxClassId, WireEnum};

use crate::weighing::{WeighError, check_source};

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
    /// The price of one the cashier typed for an open-price item, within its published bounds
    /// ([ADR-0222](../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
    /// decision 4). Ignored for any other item.
    pub typed_unit_price: Option<Money>,
    /// Where a weighed item's weight came from, its `quantity` then being the net grams
    /// ([ADR-0223](../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md)). A weighed
    /// item needs one, and any other item takes none.
    pub weight_source: Option<WeightSource>,
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
    /// The modifiers this line carries, carried through from the request.
    ///
    /// Their prices are already summed into [`Self::unit_price`], so this is not a second source of
    /// truth for money. It is here because **firing consumes them**: `consumption_for_fire` deducts
    /// the base recipe plus one recipe per modifier, and a priced line that dropped the ids could
    /// only ever be fired as though the guest had ordered the bare item.
    pub modifier_menu_item_ids: Vec<MenuItemId>,
    /// The names those modifiers have in the catalog that priced the line, index-aligned with
    /// [`Self::modifier_menu_item_ids`]
    /// ([ADR-0144](../../../docs/adr/0144-a-line-records-the-names-of-its-modifiers.md)).
    ///
    /// Read in the same lookup that adds each modifier's price, so a name and the price inside
    /// [`Self::unit_price`] come from one price book. The receipt prints these, never today's names.
    pub modifier_display_names: Vec<DisplayName>,
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
    /// An open-price item with no typed price, or one outside its published bounds (ADR-0222).
    OpenPriceOutOfRange {
        /// The most the typed price may be, in minor units.
        maximum_minor: i64,
    },
    /// A weighed item with no weight's source, or a source on an item sold by the piece (ADR-0223).
    Weighing(WeighError),
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
            Self::OpenPriceOutOfRange { maximum_minor } => write!(
                formatter,
                "an open price is typed within its bounds, at most {maximum_minor}"
            ),
            Self::Weighing(error) => write!(formatter, "{error}"),
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
    // A weighed item's price is per kilogram and its quantity net grams (ADR-0223), so the total
    // below is its weight's price, rounded once as any line's.
    check_source(base.codes.sold_by_weight(), line.weight_source)
        .map_err(RepriceError::Weighing)?;

    // A modifier is a catalog item too: choosing it adds its price to the unit price, and an unknown
    // or 86'd modifier refuses the line exactly as an unknown base item does. `checked_add` also
    // rejects a modifier priced in another currency, which folds into `Money`.
    // An open-price item is sold at the price the cashier typed, within its bounds; any other at the
    // catalog's price (ADR-0222 decision 4).
    let mut unit_price = match base.codes.open_price {
        Some(bounds) => line
            .typed_unit_price
            .filter(|typed| bounds.admits(*typed))
            .ok_or(RepriceError::OpenPriceOutOfRange {
                maximum_minor: bounds.maximum.amount_minor,
            })?,
        None => base.unit_price,
    };
    let mut modifier_display_names = Vec::with_capacity(line.modifier_menu_item_ids.len());
    for modifier_id in &line.modifier_menu_item_ids {
        let modifier = catalog
            .get(*modifier_id)
            .ok_or(RepriceError::UnknownItem(*modifier_id))?;
        if !modifier.available {
            return Err(RepriceError::Unavailable(*modifier_id));
        }
        unit_price = unit_price.checked_add(modifier.unit_price)?;
        modifier_display_names.push(modifier.display_name.clone());
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
        modifier_menu_item_ids: line.modifier_menu_item_ids.clone(),
        modifier_display_names,
        repriced,
    })
}

/// Prices a vendor's line at the source its connection chose
/// ([ADR-0248](../../../docs/adr/0248-a-delivery-order-is-priced-paid-and-reconciled-as-the-app-sold-it.md)
/// decision 1), from `line` as [`reprice_line`] priced it at the store's menu.
///
/// With `at_quote`, a line the app `quoted` a price for is sold at that unit price, so the bill is
/// what the guest paid, and taxed as the menu price would be; without it, or with no quote, the
/// menu's price stands. Either way the line is flagged [`PricedLine::repriced`] when the quote
/// differs from the menu's price by more than `tolerance_bps` basis points of the menu's price
/// (`0` flags any difference). A quote in another currency is never honoured, and always flagged.
///
/// # Errors
///
/// [`RepriceError::Money`] if the quoted line's total overflows.
pub fn price_at_source(
    line: PricedLine,
    quoted: Option<Money>,
    at_quote: bool,
    tolerance_bps: u32,
) -> Result<PricedLine, RepriceError> {
    let Some(quoted) = quoted else {
        return Ok(PricedLine {
            repriced: false,
            ..line
        });
    };
    let menu = line.unit_price;
    if quoted.currency_code != menu.currency_code {
        return Ok(PricedLine {
            repriced: true,
            ..line
        });
    }
    let difference = (i128::from(quoted.amount_minor) - i128::from(menu.amount_minor)).abs();
    let allowed = i128::from(menu.amount_minor).abs() * i128::from(tolerance_bps);
    let repriced = difference * 10_000 > allowed;
    if !at_quote {
        return Ok(PricedLine { repriced, ..line });
    }
    let line_total = quoted.mul_quantity(line.quantity, Rounding::HalfUp)?;
    Ok(PricedLine {
        unit_price: quoted,
        line_total,
        repriced,
        ..line
    })
}

#[cfg(test)]
mod tests {
    use super::{RepriceError, RequestedLine, WeighError, price_at_source, reprice_line};
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
            typed_unit_price: None,
            weight_source: None,
        }
    }

    /// ADR-0248 decision 1: a vendor's line sells at the app's quote or the menu's price, as its
    /// connection says, and is flagged only past the tolerance.
    #[test]
    fn a_vendor_line_sells_at_its_source_and_is_flagged_past_the_tolerance() {
        let two = Quantity::from_milli(2_000);
        let menu = reprice_line(
            &catalog(),
            &rates(),
            SalesChannel::DineIn,
            &line(item(500), two),
        )
        .expect("priced");
        // The app sold it at 140,000₫, 6.67% under the menu's 150,000₫.
        let quote = Some(vnd(140_000));
        let app = price_at_source(menu.clone(), quote, true, 0).expect("app");
        assert_eq!(
            (app.unit_price, app.line_total, app.repriced),
            (vnd(140_000), vnd(280_000), true)
        );
        let tolerated = price_at_source(menu.clone(), quote, true, 700).expect("within 7%");
        assert_eq!(
            (tolerated.line_total, tolerated.repriced),
            (vnd(280_000), false)
        );
        let at_menu = price_at_source(menu.clone(), quote, false, 500).expect("menu");
        assert_eq!((at_menu.line_total, at_menu.repriced), (vnd(300_000), true));
        let unquoted = price_at_source(menu.clone(), None, true, 0).expect("no quote");
        assert_eq!(
            (unquoted.line_total, unquoted.repriced),
            (vnd(300_000), false)
        );
        let foreign = Money::new(CurrencyCode::USD, 600);
        let foreign = price_at_source(menu, Some(foreign), true, 10_000).expect("foreign");
        assert_eq!((foreign.line_total, foreign.repriced), (vnd(300_000), true));
    }

    #[test]
    fn an_open_price_item_sells_at_the_typed_price_within_its_bounds_and_no_other() {
        let open = MenuEntry {
            codes: pos_proto::item_codes::ItemCodes {
                open_price: Some(pos_proto::item_codes::OpenPrice {
                    minimum: vnd(10_000),
                    maximum: vnd(90_000),
                }),
                ..Default::default()
            },
            ..MenuEntry::new(
                item(700),
                DisplayName::new("Cake by weight"),
                vnd(1),
                food(),
            )
        };
        let catalog = catalog().with(open);
        let typed = |price: Option<i64>| RequestedLine {
            typed_unit_price: price.map(vnd),
            ..line(item(700), Quantity::ONE)
        };
        let priced = reprice_line(
            &catalog,
            &rates(),
            SalesChannel::DineIn,
            &typed(Some(45_000)),
        )
        .expect("within its bounds");
        assert_eq!(
            (priced.unit_price, priced.line_total),
            (vnd(45_000), vnd(45_000))
        );
        for refused in [None, Some(9_999), Some(90_001)] {
            assert_eq!(
                reprice_line(&catalog, &rates(), SalesChannel::DineIn, &typed(refused)).err(),
                Some(RepriceError::OpenPriceOutOfRange {
                    maximum_minor: 90_000
                }),
                "{refused:?}"
            );
        }
        // Any other item ignores a typed price.
        let menu_priced = RequestedLine {
            typed_unit_price: Some(vnd(1)),
            ..line(item(500), Quantity::ONE)
        };
        let priced =
            reprice_line(&catalog, &rates(), SalesChannel::DineIn, &menu_priced).expect("prices");
        assert_eq!(priced.unit_price, vnd(150_000));
    }

    #[test]
    fn a_weighed_item_is_priced_per_kilogram_by_its_grams_rounded_once() {
        // ADR-0223: 123 g of a 99,999 per kilogram item is 12,299.877, which is 12,300.
        let weighed = MenuEntry {
            codes: pos_proto::item_codes::ItemCodes {
                sold_by: pos_proto::weighing::SoldBy::Weight.into(),
                ..Default::default()
            },
            ..MenuEntry::new(item(800), DisplayName::new("Rye"), vnd(99_999), food())
        };
        let catalog = catalog().with(weighed);
        let weighed_line = |source| RequestedLine {
            weight_source: source,
            ..line(item(800), Quantity::from_milli(123))
        };
        let source = Some(pos_proto::weighing::WeightSource::Scale);
        let priced = reprice_line(
            &catalog,
            &rates(),
            SalesChannel::DineIn,
            &weighed_line(source),
        )
        .expect("weighed");
        assert_eq!(priced.line_total, vnd(12_300));
        assert_eq!(
            reprice_line(
                &catalog,
                &rates(),
                SalesChannel::DineIn,
                &weighed_line(None)
            )
            .err(),
            Some(RepriceError::Weighing(WeighError::Required))
        );
        let piece = RequestedLine {
            weight_source: source,
            ..line(item(500), Quantity::ONE)
        };
        assert_eq!(
            reprice_line(&catalog, &rates(), SalesChannel::DineIn, &piece).err(),
            Some(RepriceError::Weighing(WeighError::NotSoldByWeight))
        );
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
    fn a_priced_line_names_its_modifiers_from_the_catalog_that_priced_it() {
        // ADR-0144: the receipt prints these, so they come from the lookup that added the price —
        // one per modifier, in the order the modifiers were chosen, repeats included.
        let mut requested = line(item(500), Quantity::ONE);
        requested.modifier_menu_item_ids = vec![item(600), item(600)];
        let priced =
            reprice_line(&catalog(), &rates(), SalesChannel::DineIn, &requested).expect("prices");
        let names: Vec<&str> = priced
            .modifier_display_names
            .iter()
            .map(DisplayName::as_str)
            .collect();
        assert_eq!(names, ["Extra cheese", "Extra cheese"]);
        assert_eq!(priced.unit_price, vnd(190_000), "each one is charged");

        let bare = reprice_line(
            &catalog(),
            &rates(),
            SalesChannel::DineIn,
            &line(item(500), Quantity::ONE),
        )
        .expect("prices");
        assert!(bare.modifier_display_names.is_empty());
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
