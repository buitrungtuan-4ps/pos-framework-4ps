// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `vendors` config node
//! ([ADR-0215](../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 5): the
//! delivery vendors a store takes orders from, and how it takes each one.
//!
//! A vendor is a tenant's delivery connection that serves this store; its `vendor_id` is the opaque
//! id a delivery order carries (decision 1). Nothing here names a particular vendor (decision 6): a
//! store learns what to call one, whether a member of staff confirms each of its orders before the
//! kitchen sees it, as a QR order waits ([ADR-0012](../../../docs/adr/0012-qr-ordering-via-cloud.md)),
//! and how many minutes it tells the vendor an order takes when it accepts it.
//!
//! **Absent means none**: a store with no node holds no delivery order for staff and answers no
//! vendor, which is how every store ran before this node.

use serde::{Deserialize, Serialize};

use crate::ids::VendorId;
use crate::kitchen_release::KitchenRelease;
use crate::text::DisplayName;
use crate::wire_enum;
use crate::wire_enum::Open;

wire_enum! {
    /// Where a vendor's order is priced
    /// ([ADR-0248](../../../docs/adr/0248-a-delivery-order-is-priced-paid-and-reconciled-as-the-app-sold-it.md)
    /// decision 1).
    VendorPriceSource, prefix = "VENDOR_PRICE_SOURCE";
    /// At the price the vendor's app quoted for each line, which is what the guest paid. The
    /// default; a line the app quoted no price for is priced at the store's menu.
    App = "APP",
    /// At the store's own menu for the delivery channel, whatever the app quoted.
    Menu = "MENU",
}

impl VendorPriceSource {
    /// A node's value as the store runs it: [`Self::App`] for an absent value and for one this
    /// release does not know.
    #[must_use]
    pub fn read(value: &Open<Self>) -> Self {
        match value.known() {
            Self::Menu => Self::Menu,
            Self::Unspecified | Self::App => Self::App,
        }
    }
}

/// A price source token as a node carries it, any value read leniently: one this release does not
/// know, or not a string at all, is kept as absent rather than refusing the node.
fn price_source_token<'de, D>(deserializer: D) -> Result<Open<VendorPriceSource>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?
        .as_str()
        .map_or_else(Open::default, Open::parse))
}

/// The most a price difference may be tolerated by: 100%, in basis points.
pub const MAX_PRICE_DIFFERENCE_BPS: u32 = 10_000;

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// The most vendors one store's node lists.
pub const MAX_VENDORS: usize = 64;

/// The fewest minutes an accepted order may be said to take.
pub const MIN_PREP_MINUTES: u16 = 1;

/// The most minutes an accepted order may be said to take.
pub const MAX_PREP_MINUTES: u16 = 240;

/// One vendor a store takes orders from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedVendor {
    /// The vendor, as its orders name it: the tenant's delivery connection id.
    pub vendor_id: VendorId,
    /// What the connection is called in the console, for the till to show.
    pub display_name: DisplayName,
    /// Whether each of its orders waits for a member of staff before the kitchen sees it.
    pub staff_confirm: bool,
    /// The minutes the store tells the vendor an accepted order takes, 1 to 240.
    pub prep_minutes: u16,
    /// How the vendor's orders reach the kitchen
    /// ([ADR-0247](../../../docs/adr/0247-a-guest-or-vendor-order-reaches-the-kitchen-as-the-store-chooses.md)).
    /// Absent, from a cloud before the setting or a connection that never set it, reads as
    /// [`KitchenRelease::ConfirmThenSend`]; left off the wire while absent.
    #[serde(
        default,
        deserialize_with = "crate::kitchen_release::token",
        skip_serializing_if = "crate::wire_enum::is_absent"
    )]
    pub kitchen_release: Open<KitchenRelease>,
    /// Where the vendor's orders are priced (ADR-0248 decision 1). Absent, from a cloud before the
    /// setting or a connection that never set it, reads as [`VendorPriceSource::App`].
    #[serde(
        default,
        deserialize_with = "price_source_token",
        skip_serializing_if = "crate::wire_enum::is_absent"
    )]
    pub price_source: Open<VendorPriceSource>,
    /// How far, in basis points of the menu price, the app's quote may differ from the store's
    /// menu before the order is flagged; `0` (the default) flags any difference.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub price_difference_bps: u32,
}

/// The `vendors` config node: the vendors a store takes orders from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedVendors {
    /// The vendors, at most [`MAX_VENDORS`].
    #[serde(default)]
    vendors: Vec<PublishedVendor>,
}

impl PublishedVendors {
    /// A node listing exactly `vendors`.
    #[must_use]
    pub fn new(vendors: Vec<PublishedVendor>) -> Self {
        Self { vendors }
    }

    /// Every vendor the node lists.
    #[must_use]
    pub fn vendors(&self) -> &[PublishedVendor] {
        &self.vendors
    }

    /// The vendor `vendor_id` names, if the store takes its orders.
    #[must_use]
    pub fn get(&self, vendor_id: VendorId) -> Option<&PublishedVendor> {
        self.vendors
            .iter()
            .find(|vendor| vendor.vendor_id == vendor_id)
    }

    /// How an order from `vendor_id` reaches the kitchen (ADR-0247): its entry's choice, and
    /// [`KitchenRelease::ConfirmThenSend`] for a vendor the node does not list.
    #[must_use]
    pub fn kitchen_release(&self, vendor_id: Option<VendorId>) -> KitchenRelease {
        vendor_id
            .and_then(|vendor_id| self.get(vendor_id))
            .map_or(KitchenRelease::ConfirmThenSend, |vendor| {
                KitchenRelease::read(&vendor.kitchen_release)
            })
    }

    /// Whether an order from `vendor_id` waits for a member of staff. An order from a vendor the
    /// node does not list waits for no one, as every delivery order did before the node.
    #[must_use]
    pub fn staff_confirm(&self, vendor_id: Option<VendorId>) -> bool {
        vendor_id
            .and_then(|vendor_id| self.get(vendor_id))
            .is_some_and(|vendor| vendor.staff_confirm)
    }

    /// Whether the node is well formed: at most [`MAX_VENDORS`] vendors, each listed once, each
    /// with its minutes in range.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.vendors.len() <= MAX_VENDORS
            && self.vendors.iter().enumerate().all(|(index, vendor)| {
                (MIN_PREP_MINUTES..=MAX_PREP_MINUTES).contains(&vendor.prep_minutes)
                    && vendor.price_difference_bps <= MAX_PRICE_DIFFERENCE_BPS
                    && !self
                        .vendors
                        .iter()
                        .take(index)
                        .any(|earlier| earlier.vendor_id == vendor.vendor_id)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{PublishedVendor, PublishedVendors, VendorPriceSource};
    use crate::ids::VendorId;
    use crate::text::DisplayName;
    use crate::ulid::Ulid;
    use crate::wire_enum::Open;

    fn vendor(n: u128, staff_confirm: bool, prep_minutes: u16) -> PublishedVendor {
        PublishedVendor {
            vendor_id: VendorId::new(Ulid::from_u128(n)),
            display_name: DisplayName::new("Sandbox delivery"),
            staff_confirm,
            prep_minutes,
            kitchen_release: Open::default(),
            price_source: Open::default(),
            price_difference_bps: 0,
        }
    }

    #[test]
    fn a_vendor_s_orders_wait_for_staff_only_where_the_node_says_so() {
        let node = PublishedVendors::new(vec![vendor(1, true, 20), vendor(2, false, 15)]);
        assert!(node.staff_confirm(Some(VendorId::new(Ulid::from_u128(1)))));
        assert!(!node.staff_confirm(Some(VendorId::new(Ulid::from_u128(2)))));
        assert!(
            !node.staff_confirm(Some(VendorId::new(Ulid::from_u128(3)))),
            "unlisted"
        );
        assert!(!node.staff_confirm(None));
        assert!(
            !PublishedVendors::default().staff_confirm(Some(VendorId::new(Ulid::from_u128(1))))
        );
    }

    #[test]
    fn the_node_round_trips_and_says_when_it_is_malformed() {
        let node = PublishedVendors::new(vec![vendor(1, true, 20)]);
        let json = serde_json::to_string(&node).expect("encode");
        assert!(json.contains(r#""staff_confirm":true"#), "{json}");
        assert_eq!(
            serde_json::from_str::<PublishedVendors>(&json).expect("decode"),
            node
        );
        assert!(node.is_valid());
        assert!(!PublishedVendors::new(vec![vendor(1, true, 0)]).is_valid());
        assert!(!PublishedVendors::new(vec![vendor(1, true, 241)]).is_valid());
        assert!(!PublishedVendors::new(vec![vendor(1, true, 20), vendor(1, false, 20)]).is_valid());
        let mut tolerant = vendor(1, true, 20);
        tolerant.price_difference_bps = 10_001;
        assert!(!PublishedVendors::new(vec![tolerant]).is_valid());
    }

    /// ADR-0248 decision 1: absent or unknown, a vendor's orders are priced at the app's quote;
    /// the store's menu only where the node says so.
    #[test]
    fn a_vendor_s_price_source_reads_as_the_app_unless_it_says_the_menu() {
        let json = r#"{"vendors":[{"vendor_id":"00000000000000000000000001","display_name":"A",
            "staff_confirm":false,"prep_minutes":20,"price_source":"VENDOR_PRICE_SOURCE_TELEPATHY"},
            {"vendor_id":"00000000000000000000000002","display_name":"B","staff_confirm":false,
            "prep_minutes":20,"price_source":"VENDOR_PRICE_SOURCE_MENU","price_difference_bps":500}]}"#;
        let node: PublishedVendors = serde_json::from_str(json).expect("lenient");
        let sources: Vec<_> = node
            .vendors()
            .iter()
            .map(|vendor| {
                (
                    VendorPriceSource::read(&vendor.price_source),
                    vendor.price_difference_bps,
                )
            })
            .collect();
        assert_eq!(
            sources,
            [(VendorPriceSource::App, 0), (VendorPriceSource::Menu, 500)]
        );
        assert_eq!(
            VendorPriceSource::read(&vendor(1, false, 20).price_source),
            VendorPriceSource::App
        );
    }
}
