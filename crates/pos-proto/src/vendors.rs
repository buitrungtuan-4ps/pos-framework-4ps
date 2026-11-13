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
use crate::text::DisplayName;

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
    use super::{PublishedVendor, PublishedVendors};
    use crate::ids::VendorId;
    use crate::text::DisplayName;
    use crate::ulid::Ulid;

    fn vendor(n: u128, staff_confirm: bool, prep_minutes: u16) -> PublishedVendor {
        PublishedVendor {
            vendor_id: VendorId::new(Ulid::from_u128(n)),
            display_name: DisplayName::new("Sandbox delivery"),
            staff_confirm,
            prep_minutes,
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
    }
}
