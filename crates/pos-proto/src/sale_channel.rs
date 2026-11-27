// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The two axes a sale is reported on: where its order came from, and how it is served
//! ([ADR-0259](../../../docs/adr/0259-a-sale-is-reported-by-its-channel-and-its-order-type.md)).
//!
//! Both are **derived**, never stored: [`sale_channel`] reads them off the fields
//! `sales.order.opened` already carries, so an order on any log, an old one included, reads the same
//! on the edge and in the cloud, which both call this one function.

use crate::enums::SalesChannel;
use crate::ids::{TableId, VendorId};
use crate::wire_enum;

wire_enum! {
    /// Where an order came from (ADR-0259 decision 2). A delivery app is
    /// [`App`](Self::App) with the order's vendor, named by its connection, never by a list here.
    SaleSource, prefix = "SALE_SOURCE";
    /// The store's own till: at a table, at the counter, or a phone order staff took.
    Pos = "POS",
    /// A guest ordering from a table's QR page.
    Qr = "QR",
    /// A kiosk. Reserved: a kiosk's orders read as `POS` until the order records it (decision 2).
    Kiosk = "KIOSK",
    /// The public API.
    Api = "API",
    /// A delivery app, through one of the store's vendor connections.
    App = "APP",
}

wire_enum! {
    /// How an order is served (ADR-0259 decision 3): a closed set, by how the food leaves the store.
    OrderType, prefix = "ORDER_TYPE";
    /// Eaten here.
    DineIn = "DINE_IN",
    /// Carried away by the guest.
    Takeaway = "TAKEAWAY",
    /// Carried to the guest.
    Delivery = "DELIVERY",
}

/// One sale's place on both axes: its source, the app for an app's order, and its order type.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct SaleChannel {
    /// Where the order came from.
    pub source: SaleSource,
    /// The delivery app's connection, for [`SaleSource::App`] only.
    pub vendor_id: Option<VendorId>,
    /// How it is served; [`OrderType::Unspecified`] where the order did not say.
    pub order_type: OrderType,
}

impl SaleChannel {
    /// A bill whose order the reader never saw open, as on a log from before
    /// `sales.order.opened`: the store's own till, its type not recorded.
    pub const NOT_RECORDED: Self = Self {
        source: SaleSource::Pos,
        vendor_id: None,
        order_type: OrderType::Unspecified,
    };
}

/// Where an order with `channel`, `table_id` and `vendor_id`, as `sales.order.opened` records them,
/// came from and how it is served (ADR-0259 decisions 2 and 3). `channel` is `None` for a token this
/// build does not know.
#[must_use]
pub fn sale_channel(
    channel: Option<SalesChannel>,
    table_id: Option<TableId>,
    vendor_id: Option<VendorId>,
) -> SaleChannel {
    let at_table = if table_id.is_some() {
        OrderType::DineIn
    } else {
        OrderType::Takeaway
    };
    let order_type = match channel {
        Some(SalesChannel::DineIn) => OrderType::DineIn,
        Some(SalesChannel::Takeaway) => OrderType::Takeaway,
        Some(SalesChannel::Delivery) => OrderType::Delivery,
        Some(SalesChannel::Qr | SalesChannel::Api) => at_table,
        Some(SalesChannel::Unspecified) | None => OrderType::Unspecified,
    };
    let source = match (vendor_id, channel) {
        (Some(_), _) => SaleSource::App,
        (None, Some(SalesChannel::Qr)) => SaleSource::Qr,
        (None, Some(SalesChannel::Api)) => SaleSource::Api,
        (None, _) => SaleSource::Pos,
    };
    SaleChannel {
        source,
        vendor_id,
        order_type,
    }
}

#[cfg(test)]
mod tests {
    use super::{OrderType, SaleChannel, SaleSource, sale_channel};
    use crate::enums::SalesChannel;
    use crate::ids::{TableId, VendorId};
    use crate::ulid::Ulid;
    use crate::wire_enum::WireEnum;

    fn table() -> TableId {
        TableId::new(Ulid::from_parts(1, 1))
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
    }

    #[test]
    fn the_stores_own_till_reads_by_its_channel() {
        let dine_in = sale_channel(Some(SalesChannel::DineIn), Some(table()), None);
        assert_eq!(
            (dine_in.source, dine_in.order_type),
            (SaleSource::Pos, OrderType::DineIn)
        );
        let takeaway = sale_channel(Some(SalesChannel::Takeaway), None, None);
        assert_eq!(
            (takeaway.source, takeaway.order_type),
            (SaleSource::Pos, OrderType::Takeaway)
        );
        let phone = sale_channel(Some(SalesChannel::Delivery), None, None);
        assert_eq!(
            (phone.source, phone.order_type),
            (SaleSource::Pos, OrderType::Delivery)
        );
    }

    #[test]
    fn a_qr_or_api_order_is_dine_in_at_a_table_and_takeaway_without_one() {
        let at_table = sale_channel(Some(SalesChannel::Qr), Some(table()), None);
        assert_eq!(
            (at_table.source, at_table.order_type),
            (SaleSource::Qr, OrderType::DineIn)
        );
        let counter = sale_channel(Some(SalesChannel::Qr), None, None);
        assert_eq!(counter.order_type, OrderType::Takeaway);
        let api = sale_channel(Some(SalesChannel::Api), None, None);
        assert_eq!(
            (api.source, api.order_type),
            (SaleSource::Api, OrderType::Takeaway)
        );
    }

    #[test]
    fn an_apps_order_names_its_connection_and_keeps_its_channels_type() {
        let grab = Some(VendorId::new(Ulid::from_parts(2, 7)));
        let delivered = sale_channel(Some(SalesChannel::Delivery), None, grab);
        assert_eq!(
            delivered,
            SaleChannel {
                source: SaleSource::App,
                vendor_id: grab,
                order_type: OrderType::Delivery,
            }
        );
        let picked_up = sale_channel(Some(SalesChannel::Takeaway), None, grab);
        assert_eq!(
            (picked_up.source, picked_up.order_type),
            (SaleSource::App, OrderType::Takeaway)
        );
    }

    #[test]
    fn a_channel_this_build_does_not_know_is_not_recorded() {
        assert_eq!(sale_channel(None, None, None), SaleChannel::NOT_RECORDED);
        assert_eq!(
            sale_channel(Some(SalesChannel::Unspecified), Some(table()), None).order_type,
            OrderType::Unspecified
        );
    }
}
