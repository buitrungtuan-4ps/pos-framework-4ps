// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Order connectors: a delivery vendor's own webhook, turned into the internal order intake
//! ([ADR-0083](../../../docs/adr/0083-integration-doctrine.md) decision 3, roadmap-v3 B9.3).
//!
//! A vendor whose order format differs from `/v1/orders` gets a connector, not a core change. A
//! connector is two things a vendor decides, and nothing else: how its webhook is signed
//! ([`OrderConnector::verify`]) and how its order reads ([`OrderConnector::read`]). Everything else
//! is shared here and in [`route`]:
//!
//! - the tenant's **connection** to the vendor ([`crate::connections`]) names the store it serves,
//!   holds the vendor's signing secret sealed, and holds the **item map**, the vendor's item codes
//!   to the store's menu items, as a setting;
//! - the order goes to the same [`OrderIn`](pos_ports::order_in::OrderIn) intake `/v1/orders`
//!   uses, on `SALES_CHANNEL_DELIVERY`, so the store reprices it, routes it to the kitchen and
//!   accepts it offline exactly as it does any other inbound order;
//! - its idempotency key is the vendor's own order reference prefixed by the connection id, so a
//!   vendor's retry finds the order it already placed and two vendors' references never meet.
//!
//! # The recipient
//!
//! A vendor's order may name who it goes to: a name, a phone number and an address
//! ([`VendorRecipient`]). That is personal data
//! ([ADR-0215](../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 3), so it never
//! enters the order: [`recipient`] keeps it as a subject record on the cloud, apart from the relay's
//! queue and under the store's retention, and the order carries only its `subject_id`. It is never
//! logged and never printed by `Debug`.

pub mod answer;
pub mod demo;
pub mod recipient;
pub mod route;
pub mod vendors;

use std::collections::BTreeMap;

use pos_ports::order_in::{ExternalReference, InboundOrder, InboundOrderLine};
use pos_proto::ids::{MenuItemId, StoreId, VendorId};
use pos_proto::text::GuestNote;
use pos_proto::time::Timestamp;
use pos_proto::wire_enum::Open;
use pos_proto::{Quantity, SalesChannel};

/// The most lines one vendor order may carry.
pub const MAX_LINES: usize = 200;
/// The most entries an item map may hold.
pub const MAX_MAPPED_ITEMS: usize = 2_000;
/// The longest note a line may carry, in characters; a longer one is cut.
pub const MAX_NOTE_CHARS: usize = 200;

/// A vendor's order as its connector read it: the vendor's reference, when it was placed, and its
/// lines by the vendor's own item codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VendorOrder {
    /// The vendor's reference for the order.
    pub reference: String,
    /// When the vendor says it was placed, in Unix milliseconds.
    pub placed_at_ms: i64,
    /// What was ordered.
    pub lines: Vec<VendorLine>,
    /// Who it goes to, when the vendor says.
    pub recipient: Option<VendorRecipient>,
}

/// Who a delivery goes to, as the vendor sent it. Personal data: kept as a subject record
/// ([`recipient`]), never in the order, a log or an event, and its `Debug` prints no value.
#[derive(Clone, PartialEq, Eq)]
pub struct VendorRecipient {
    /// The recipient's name.
    pub name: String,
    /// The recipient's phone number.
    pub phone: String,
    /// Where it goes.
    pub address: String,
}

impl core::fmt::Debug for VendorRecipient {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("VendorRecipient([REDACTED])")
    }
}

/// One line of a vendor's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VendorLine {
    /// The vendor's code for the item, looked up in the connection's item map.
    pub item_code: String,
    /// How many, as a whole number.
    pub quantity: u32,
    /// The guest's note, which stays at the store and never enters the event log.
    pub note: Option<String>,
}

/// What one vendor decides: how its webhook is signed, and how its order reads. Pure: no I/O, no
/// clock, so each is proved by its body and its signature alone.
pub trait OrderConnector: Send + Sync {
    /// The catalogue provider this connector serves (`delivery.<vendor>`).
    fn provider_id(&self) -> &'static str;

    /// Whether `headers` carry the vendor's valid signature over `body` under `secret`. Must
    /// compare in constant time.
    fn verify(&self, headers: &axum::http::HeaderMap, body: &[u8], secret: &str) -> bool;

    /// Reads the vendor's order.
    ///
    /// # Errors
    ///
    /// A short reason when the body is not one of the vendor's orders.
    fn read(&self, body: &[u8]) -> Result<VendorOrder, &'static str>;

    /// The request that tells the vendor a store's answer about `reference` (ADR-0215 decision 4),
    /// signed under `secret`, or `None` when the vendor takes no answers. Pure, like the rest: the
    /// route sends it.
    fn answer(
        &self,
        reference: &str,
        answer: &answer::DeliveryAnswer,
        secret: &str,
    ) -> Option<answer::AnswerCall>;
}

/// Why a vendor order could not become an inbound order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MappingRefusal {
    /// The item map setting is malformed.
    BadItemMap,
    /// An item code is not in the item map: the whole order is refused, so a vendor never has one
    /// line silently dropped.
    UnmappedItem(String),
    /// The order is empty, too large, has a zero quantity, or a reference or time out of range.
    Invalid(&'static str),
}

/// Reads an item map: one `code=menu_item_id` per line, blank lines ignored. A code is mapped once.
///
/// # Errors
///
/// [`MappingRefusal::BadItemMap`] for a line without `=`, an empty code, a menu item id that is not
/// a ULID, a code mapped twice, or more than [`MAX_MAPPED_ITEMS`] entries.
pub fn item_map(text: &str) -> Result<BTreeMap<String, MenuItemId>, MappingRefusal> {
    let mut map = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let (code, item) = line.split_once('=').ok_or(MappingRefusal::BadItemMap)?;
        let code = code.trim();
        let item = item
            .trim()
            .parse::<MenuItemId>()
            .map_err(|_| MappingRefusal::BadItemMap)?;
        if code.is_empty() || map.insert(code.to_owned(), item).is_some() {
            return Err(MappingRefusal::BadItemMap);
        }
        if map.len() > MAX_MAPPED_ITEMS {
            return Err(MappingRefusal::BadItemMap);
        }
    }
    Ok(map)
}

/// The inbound order a vendor's order becomes for `store_id`, keyed by the connection.
///
/// # Errors
///
/// A [`MappingRefusal`].
pub fn to_inbound(
    connection_id: &str,
    store_id: StoreId,
    order: &VendorOrder,
    map: &BTreeMap<String, MenuItemId>,
) -> Result<InboundOrder, MappingRefusal> {
    if order.lines.is_empty() || order.lines.len() > MAX_LINES {
        return Err(MappingRefusal::Invalid("an order has 1 to 200 lines"));
    }
    if order.reference.is_empty() {
        return Err(MappingRefusal::Invalid("an order has a reference"));
    }
    let external_reference =
        ExternalReference::parse(&format!("{connection_id}:{}", order.reference))
            .map_err(|_| MappingRefusal::Invalid("the order's reference is too long"))?;
    let placed_at = Timestamp::from_milliseconds_since_epoch(order.placed_at_ms)
        .map_err(|_| MappingRefusal::Invalid("the order's time is out of range"))?;
    let mut lines = Vec::with_capacity(order.lines.len());
    for line in &order.lines {
        if line.quantity == 0 {
            return Err(MappingRefusal::Invalid("a line's quantity is at least 1"));
        }
        let menu_item_id = *map
            .get(&line.item_code)
            .ok_or_else(|| MappingRefusal::UnmappedItem(line.item_code.clone()))?;
        lines.push(InboundOrderLine {
            menu_item_id,
            quantity: Quantity::from_milli(i64::from(line.quantity).saturating_mul(1_000)),
            modifier_menu_item_ids: Vec::new(),
            // The store's own price is what counts: no quote is passed, so nothing is repriced.
            quoted_unit_price: None,
            note: line
                .note
                .as_deref()
                .map(|note| GuestNote::new(note.chars().take(MAX_NOTE_CHARS).collect::<String>())),
        });
    }
    // The vendor is the connection, opaque to the store (ADR-0215 decisions 1 and 6).
    let vendor_id = connection_id
        .parse::<VendorId>()
        .map_err(|_| MappingRefusal::Invalid("the connection's id is not a ULID"))?;
    Ok(InboundOrder {
        external_reference,
        sales_channel: Open::from_known(SalesChannel::Delivery),
        store_id,
        table_id: None,
        subject_id: None,
        vendor_id: Some(vendor_id),
        lines,
        placed_at,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pos_proto::ids::{MenuItemId, StoreId};
    use pos_proto::ulid::Ulid;
    use pos_proto::{Quantity, SalesChannel};

    use super::{MappingRefusal, VendorLine, VendorOrder, item_map, to_inbound};

    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn order(lines: Vec<VendorLine>) -> VendorOrder {
        VendorOrder {
            reference: "D-1001".to_owned(),
            placed_at_ms: 1_800_000_000_000,
            lines,
            recipient: None,
        }
    }

    fn line(code: &str, quantity: u32) -> VendorLine {
        VendorLine {
            item_code: code.to_owned(),
            quantity,
            note: None,
        }
    }

    #[test]
    fn an_item_map_is_one_code_per_line_each_mapped_once() {
        let text = format!("\n PIZZA-M = {}\nCOLA={}\n\n", item(1), item(2));
        let map = item_map(&text).expect("a good map");
        assert_eq!(map.get("PIZZA-M"), Some(&item(1)));
        assert_eq!(map.get("COLA"), Some(&item(2)));
        for bad in [
            "PIZZA-M".to_owned(),
            format!("={}", item(1)),
            "PIZZA-M=not-a-ulid".to_owned(),
            format!("A={}\nA={}", item(1), item(2)),
        ] {
            assert_eq!(item_map(&bad), Err(MappingRefusal::BadItemMap), "{bad}");
        }
    }

    #[test]
    fn a_vendor_order_becomes_a_delivery_order_keyed_by_its_connection() {
        let map = BTreeMap::from([("PIZZA-M".to_owned(), item(1))]);
        let store = StoreId::new(Ulid::from_u128(9));
        let inbound = to_inbound(
            "01J0000000000000000000CN01",
            store,
            &order(vec![line("PIZZA-M", 2)]),
            &map,
        )
        .expect("maps");
        assert_eq!(
            inbound.external_reference.as_str(),
            "01J0000000000000000000CN01:D-1001"
        );
        assert_eq!(
            inbound.vendor_id.map(|id| id.to_string()).as_deref(),
            Some("01J0000000000000000000CN01"),
            "the vendor is the connection, an opaque id (ADR-0215)"
        );
        assert_eq!(inbound.sales_channel.known(), SalesChannel::Delivery);
        assert_eq!(inbound.store_id, store);
        assert_eq!(inbound.lines[0].quantity, Quantity::from_milli(2_000));
        assert_eq!(
            inbound.lines[0].quoted_unit_price, None,
            "the store prices it"
        );

        assert_eq!(
            to_inbound(
                "C",
                store,
                &order(vec![line("PIZZA-M", 1), line("SUSHI", 1)]),
                &map
            ),
            Err(MappingRefusal::UnmappedItem("SUSHI".to_owned())),
            "one unmapped item refuses the whole order"
        );
        assert!(to_inbound("C", store, &order(Vec::new()), &map).is_err());
        assert!(to_inbound("C", store, &order(vec![line("PIZZA-M", 0)]), &map).is_err());
        let long = VendorOrder {
            reference: "x".repeat(200),
            ..order(vec![line("PIZZA-M", 1)])
        };
        assert!(to_inbound("C", store, &long, &map).is_err());
    }
}
