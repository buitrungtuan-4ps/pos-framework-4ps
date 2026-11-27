// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The two axes a sale is reported on: where its order came from, and how it is served
//! ([ADR-0259](../../../docs/adr/0259-a-sale-is-reported-by-its-channel-and-its-order-type.md),
//! [ADR-0260](../../../docs/adr/0260-order-types-and-sale-sources-are-published-lists.md)).
//!
//! Both are tokens: a built-in one, or one HQ published. [`sale_channel`] reads them off
//! `sales.order.opened`: the tokens it recorded where it recorded any, and otherwise what ADR-0259
//! derives from its channel, table and app, so an order on any log, an old one included, reads the
//! same on the edge and in the cloud, which both call this one function.

use std::borrow::Cow;

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::enums::SalesChannel;
use crate::ids::{TableId, VendorId};
use crate::wire_enum;
use crate::wire_enum::WireEnum;

wire_enum! {
    /// Where an order came from (ADR-0259 decision 2). A delivery app is
    /// [`App`](Self::App) with the order's vendor, named by its connection, never by a list here.
    SaleSource, prefix = "SALE_SOURCE";
    /// The store's own till: at a table, at the counter, or a phone order staff took.
    Pos = "POS",
    /// A guest ordering from a table's QR page.
    Qr = "QR",
    /// A kiosk: an order a kiosk opened records it (ADR-0260 decision 4); older ones read as `POS`.
    Kiosk = "KIOSK",
    /// The public API.
    Api = "API",
    /// A delivery app, through one of the store's vendor connections.
    App = "APP",
}

wire_enum! {
    /// How an order is served, by how the food leaves the store: the built-in types, and the base
    /// kind every published type maps onto (ADR-0260 decision 1).
    OrderType, prefix = "ORDER_TYPE";
    /// Eaten here.
    DineIn = "DINE_IN",
    /// Carried away by the guest.
    Takeaway = "TAKEAWAY",
    /// Carried to the guest.
    Delivery = "DELIVERY",
}

/// The longest token an order type or a sale source may have (ADR-0260 decision 2).
pub const MAX_TOKEN_LEN: usize = 48;

/// An order type's or a sale source's token: one of the built-ins above, or one HQ published in the
/// `order_channels` node
/// ([ADR-0260](../../../docs/adr/0260-order-types-and-sale-sources-are-published-lists.md)
/// decision 2).
///
/// `ORDER_TYPE_` or `SALE_SOURCE_`, then upper-case ASCII letters, digits and `_`, at most
/// [`MAX_TOKEN_LEN`] characters. Built only through [`Self::order_type`] and [`Self::source`], and
/// refused on deserialise when it fails both. The personal-data barrier admits it to an event
/// payload ([`crate::pii`]): with no space, `@`, lower case or punctuation but `_` it cannot carry a
/// person's name, an e-mail or a phone number, and it comes from the list an operator authors.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(transparent)]
pub struct SaleToken(Cow<'static, str>);

/// Why a text is not a [`SaleToken`] of the axis asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaleTokenError;

impl fmt::Display for SaleTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "an order type or sale source token is ORDER_TYPE_ or SALE_SOURCE_ then upper-case \
             letters, digits and underscores, at most 48 characters",
        )
    }
}

impl core::error::Error for SaleTokenError {}

/// The prefix an order type's token starts with.
const ORDER_TYPE_PREFIX: &str = "ORDER_TYPE_";
/// The prefix a sale source's token starts with.
const SOURCE_PREFIX: &str = "SALE_SOURCE_";

impl SaleToken {
    /// The store's own till, [`SaleSource::Pos`].
    pub const POS: Self = Self(Cow::Borrowed("SALE_SOURCE_POS"));
    /// A kiosk, [`SaleSource::Kiosk`] (ADR-0260 decision 4).
    pub const KIOSK: Self = Self(Cow::Borrowed("SALE_SOURCE_KIOSK"));
    /// An order type not recorded, [`OrderType::Unspecified`].
    pub const TYPE_NOT_RECORDED: Self = Self(Cow::Borrowed("ORDER_TYPE_UNSPECIFIED"));

    /// Checks and wraps an order type's token.
    ///
    /// # Errors
    ///
    /// [`SaleTokenError`] unless `text` is `ORDER_TYPE_` and a well-formed rest.
    pub fn order_type(text: &str) -> Result<Self, SaleTokenError> {
        Self::checked(text, ORDER_TYPE_PREFIX)
    }

    /// Checks and wraps a sale source's token.
    ///
    /// # Errors
    ///
    /// [`SaleTokenError`] unless `text` is `SALE_SOURCE_` and a well-formed rest.
    pub fn source(text: &str) -> Result<Self, SaleTokenError> {
        Self::checked(text, SOURCE_PREFIX)
    }

    /// A built-in's token, which is always well formed.
    #[must_use]
    pub fn of<E: WireEnum>(value: E) -> Self {
        Self(Cow::Borrowed(value.as_wire()))
    }

    /// Whether it is an order type's token.
    #[must_use]
    pub fn is_order_type(&self) -> bool {
        self.0.starts_with(ORDER_TYPE_PREFIX)
    }

    /// Whether it is a sale source's token.
    #[must_use]
    pub fn is_source(&self) -> bool {
        self.0.starts_with(SOURCE_PREFIX)
    }

    /// The token as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn checked(text: &str, prefix: &str) -> Result<Self, SaleTokenError> {
        let rest = text.strip_prefix(prefix).unwrap_or_default();
        let sound = !rest.is_empty()
            && text.len() <= MAX_TOKEN_LEN
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if sound {
            Ok(Self(Cow::Owned(text.to_owned())))
        } else {
            Err(SaleTokenError)
        }
    }

    /// Either axis's token, for reading one off the wire.
    fn either(text: &str) -> Result<Self, SaleTokenError> {
        Self::order_type(text).or_else(|_| Self::source(text))
    }
}

impl fmt::Display for SaleToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SaleToken {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        crate::string_value::deserialize(
            deserializer,
            "an order type or sale source token",
            Self::either,
        )
    }
}

/// An optional token on an event, read leniently: a value that is not a well-formed token reads as
/// absent, so the order reads as ADR-0259 derives it rather than the event failing to decode.
pub(crate) fn lenient_token<'de, D>(deserializer: D) -> Result<Option<SaleToken>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?
        .as_str()
        .and_then(|text| SaleToken::either(text).ok()))
}

/// One sale's place on both axes: its source, the app for an app's order, and its order type.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SaleChannel {
    /// Where the order came from: a `SALE_SOURCE_*` token, built in or published.
    pub source: SaleToken,
    /// The delivery app's connection, for `SALE_SOURCE_APP` only.
    pub vendor_id: Option<VendorId>,
    /// How it is served: an `ORDER_TYPE_*` token, built in or published;
    /// `ORDER_TYPE_UNSPECIFIED` where the order did not say.
    pub order_type: SaleToken,
}

impl SaleChannel {
    /// A bill whose order the reader never saw open, as on a log from before
    /// `sales.order.opened`: the store's own till, its type not recorded.
    pub const NOT_RECORDED: Self = Self {
        source: SaleToken::POS,
        vendor_id: None,
        order_type: SaleToken::TYPE_NOT_RECORDED,
    };
}

/// What `sales.order.opened` records about where an order came from and how it is served.
#[derive(Clone, Copy, Debug, Default)]
pub struct Opening<'a> {
    /// Its `channel`; `None` for a token this build does not know.
    pub channel: Option<SalesChannel>,
    /// Its table.
    pub table_id: Option<TableId>,
    /// Its delivery app's connection.
    pub vendor_id: Option<VendorId>,
    /// The order type it recorded (ADR-0260 decision 4); `None` on every older order.
    pub order_type: Option<&'a SaleToken>,
    /// The sale source it recorded (ADR-0260 decision 4); `None` on every older order.
    pub sale_source: Option<&'a SaleToken>,
}

/// The order type ADR-0259 decision 3 derives from an order's `channel` and table: what an order
/// that records none reads as, and the base kind a QR, API or app order is matched on
/// (ADR-0260 decision 4).
#[must_use]
pub fn derived_order_type(channel: Option<SalesChannel>, table_id: Option<TableId>) -> OrderType {
    match channel {
        Some(SalesChannel::DineIn) => OrderType::DineIn,
        Some(SalesChannel::Takeaway) => OrderType::Takeaway,
        Some(SalesChannel::Delivery) => OrderType::Delivery,
        Some(SalesChannel::Qr | SalesChannel::Api) if table_id.is_some() => OrderType::DineIn,
        Some(SalesChannel::Qr | SalesChannel::Api) => OrderType::Takeaway,
        Some(SalesChannel::Unspecified) | None => OrderType::Unspecified,
    }
}

/// The source ADR-0259 decision 2 derives for an order that records none.
#[must_use]
pub fn derived_source(channel: Option<SalesChannel>, vendor_id: Option<VendorId>) -> SaleSource {
    match (vendor_id, channel) {
        (Some(_), _) => SaleSource::App,
        (None, Some(SalesChannel::Qr)) => SaleSource::Qr,
        (None, Some(SalesChannel::Api)) => SaleSource::Api,
        (None, _) => SaleSource::Pos,
    }
}

/// Where an order came from and how it is served, as its `sales.order.opened` records it
/// (ADR-0260 decision 5): a recorded token wins, and an order that records none, every order from
/// before ADR-0260 included, reads exactly as ADR-0259 derives it. An app's order is always
/// `SALE_SOURCE_APP` with its connection.
#[must_use]
pub fn sale_channel(opening: Opening<'_>) -> SaleChannel {
    let order_type = opening
        .order_type
        .filter(|token| token.is_order_type())
        .cloned()
        .unwrap_or_else(|| SaleToken::of(derived_order_type(opening.channel, opening.table_id)));
    let derived = derived_source(opening.channel, opening.vendor_id);
    let source = match (derived, opening.sale_source) {
        (SaleSource::App, _) | (_, None) => SaleToken::of(derived),
        (_, Some(token)) if token.is_source() => token.clone(),
        (_, Some(_)) => SaleToken::of(derived),
    };
    SaleChannel {
        source,
        vendor_id: opening.vendor_id,
        order_type,
    }
}

#[cfg(test)]
mod tests {
    use super::{Opening, OrderType, SaleChannel, SaleSource, SaleToken, sale_channel};
    use crate::enums::SalesChannel;
    use crate::ids::{TableId, VendorId};
    use crate::ulid::Ulid;
    use crate::wire_enum::WireEnum;

    fn table() -> TableId {
        TableId::new(Ulid::from_parts(1, 1))
    }

    fn read(
        channel: Option<SalesChannel>,
        table_id: Option<TableId>,
        vendor_id: Option<VendorId>,
    ) -> (String, String) {
        let read = sale_channel(Opening {
            channel,
            table_id,
            vendor_id,
            ..Opening::default()
        });
        (
            read.source.as_str().to_owned(),
            read.order_type.as_str().to_owned(),
        )
    }

    fn pair(source: SaleSource, order_type: OrderType) -> (String, String) {
        (source.as_wire().to_owned(), order_type.as_wire().to_owned())
    }

    #[test]
    fn the_tokens_follow_the_naming_standard() {
        assert_eq!(SaleSource::UNSPECIFIED.as_wire(), "SALE_SOURCE_UNSPECIFIED");
        assert_eq!(SaleSource::App.as_wire(), "SALE_SOURCE_APP");
        assert_eq!(OrderType::UNSPECIFIED.as_wire(), "ORDER_TYPE_UNSPECIFIED");
        assert_eq!(OrderType::Takeaway.as_wire(), "ORDER_TYPE_TAKEAWAY");
        for token in [
            "SALE_SOURCE_POS",
            "SALE_SOURCE_QR",
            "SALE_SOURCE_KIOSK",
            "SALE_SOURCE_API",
        ] {
            assert_eq!(
                SaleSource::from_wire(token).map(SaleSource::as_wire),
                Some(token)
            );
        }
        assert_eq!(SaleToken::POS.as_str(), SaleSource::Pos.as_wire());
        assert_eq!(SaleToken::KIOSK.as_str(), SaleSource::Kiosk.as_wire());
        assert_eq!(
            SaleToken::TYPE_NOT_RECORDED.as_str(),
            OrderType::Unspecified.as_wire()
        );
    }

    #[test]
    fn the_stores_own_till_reads_by_its_channel() {
        assert_eq!(
            read(Some(SalesChannel::DineIn), Some(table()), None),
            pair(SaleSource::Pos, OrderType::DineIn)
        );
        assert_eq!(
            read(Some(SalesChannel::Takeaway), None, None),
            pair(SaleSource::Pos, OrderType::Takeaway)
        );
        assert_eq!(
            read(Some(SalesChannel::Delivery), None, None),
            pair(SaleSource::Pos, OrderType::Delivery)
        );
    }

    #[test]
    fn a_qr_or_api_order_is_dine_in_at_a_table_and_takeaway_without_one() {
        assert_eq!(
            read(Some(SalesChannel::Qr), Some(table()), None),
            pair(SaleSource::Qr, OrderType::DineIn)
        );
        assert_eq!(
            read(Some(SalesChannel::Qr), None, None),
            pair(SaleSource::Qr, OrderType::Takeaway)
        );
        assert_eq!(
            read(Some(SalesChannel::Api), None, None),
            pair(SaleSource::Api, OrderType::Takeaway)
        );
    }

    #[test]
    fn an_apps_order_names_its_connection_and_keeps_its_channels_type() {
        let grab = Some(VendorId::new(Ulid::from_parts(2, 7)));
        let delivered = sale_channel(Opening {
            channel: Some(SalesChannel::Delivery),
            vendor_id: grab,
            ..Opening::default()
        });
        assert_eq!(
            delivered,
            SaleChannel {
                source: SaleToken::of(SaleSource::App),
                vendor_id: grab,
                order_type: SaleToken::of(OrderType::Delivery),
            }
        );
        assert_eq!(
            read(Some(SalesChannel::Takeaway), None, grab),
            pair(SaleSource::App, OrderType::Takeaway)
        );
    }

    #[test]
    fn a_channel_this_build_does_not_know_is_not_recorded() {
        assert_eq!(sale_channel(Opening::default()), SaleChannel::NOT_RECORDED);
        assert_eq!(
            read(Some(SalesChannel::Unspecified), Some(table()), None).1,
            OrderType::Unspecified.as_wire()
        );
    }

    /// ADR-0260 decision 5: a recorded token wins on either axis, except that an app's order stays
    /// the app's, and a token of the other axis is not taken.
    #[test]
    fn a_recorded_token_wins_and_an_apps_order_stays_the_apps() {
        let pickup = SaleToken::order_type("ORDER_TYPE_PICKUP").expect("a token");
        let hotline = SaleToken::source("SALE_SOURCE_HOTLINE").expect("a token");
        let read = sale_channel(Opening {
            channel: Some(SalesChannel::Takeaway),
            order_type: Some(&pickup),
            sale_source: Some(&hotline),
            ..Opening::default()
        });
        assert_eq!(
            (read.source, read.order_type),
            (hotline.clone(), pickup.clone())
        );

        let grab = Some(VendorId::new(Ulid::from_parts(2, 7)));
        let app = sale_channel(Opening {
            channel: Some(SalesChannel::Takeaway),
            vendor_id: grab,
            order_type: Some(&pickup),
            sale_source: Some(&hotline),
            ..Opening::default()
        });
        assert_eq!(app.source, SaleToken::of(SaleSource::App));
        assert_eq!(app.order_type, pickup);

        let crossed = sale_channel(Opening {
            channel: Some(SalesChannel::DineIn),
            order_type: Some(&hotline),
            sale_source: Some(&pickup),
            ..Opening::default()
        });
        assert_eq!(
            (crossed.source, crossed.order_type),
            (SaleToken::POS, SaleToken::of(OrderType::DineIn))
        );
    }

    #[test]
    fn a_token_is_its_prefix_then_upper_case_letters_digits_and_underscores() {
        for sound in ["ORDER_TYPE_PICKUP", "ORDER_TYPE_DRIVE_THROUGH_2"] {
            assert!(SaleToken::order_type(sound).is_ok(), "{sound}");
        }
        for unsound in [
            "ORDER_TYPE_",
            "ORDER_TYPE_pickup",
            "ORDER_TYPE_PICK UP",
            "ORDER_TYPE_A@B",
            "SALE_SOURCE_KIOSK",
            "PICKUP",
            "ORDER_TYPE_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ABC",
        ] {
            assert!(SaleToken::order_type(unsound).is_err(), "{unsound}");
        }
        assert!(SaleToken::source("SALE_SOURCE_HOTLINE").is_ok());
        assert!(SaleToken::source("ORDER_TYPE_PICKUP").is_err());
        let parsed: SaleToken =
            serde_json::from_str("\"SALE_SOURCE_KIOSK\"").expect("a token reads back");
        assert_eq!(parsed, SaleToken::KIOSK);
        assert!(serde_json::from_str::<SaleToken>("\"kiosk\"").is_err());
    }
}
