// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `vendors` node a store is sent
//! ([ADR-0215](../../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 5): the
//! delivery connections it takes orders from, and how it takes each one.
//!
//! A store takes orders from each enabled delivery connection scoped to it, the connections a
//! connector serves (a connector takes only store-scoped ones). Each delivery provider declares two
//! settings the tenant chooses, the same for every vendor ([`STAFF_CONFIRM`], [`PREP_MINUTES`]), so
//! nothing here knows one vendor from another (decision 6). A connection that has not been given
//! them, stored before they existed, refuses the publish by name rather than being guessed at.

use pos_proto::ids::VendorId;
use pos_proto::integrations::SettingValue;
use pos_proto::text::DisplayName;
use pos_proto::vendors::{
    MAX_PREP_MINUTES, MAX_PRICE_DIFFERENCE_BPS, MIN_PREP_MINUTES, PublishedVendor,
    PublishedVendors, VendorPayment, VendorPriceSource,
};
use pos_proto::wire_enum::Open;
use pos_providers::Family;

use crate::connections::{Connection, ScopeLevel};

/// The setting that says whether a member of staff confirms each of the vendor's orders.
pub const STAFF_CONFIRM: &str = "staff_confirm";
/// The setting that says how many minutes the store tells the vendor an accepted order takes.
pub const PREP_MINUTES: &str = "prep_minutes";
/// The optional setting that says how the vendor's orders reach the kitchen
/// ([ADR-0247](../../../../docs/adr/0247-a-guest-or-vendor-order-reaches-the-kitchen-as-the-store-chooses.md)):
/// a `KITCHEN_RELEASE_*` token. Absent, the store takes two steps, as before.
pub const KITCHEN_RELEASE: &str = "kitchen_release";

/// The tokens [`KITCHEN_RELEASE`] takes, in the order the console lists them: the default first.
pub const KITCHEN_RELEASE_OPTIONS: &[&str] = &[
    "KITCHEN_RELEASE_CONFIRM_THEN_SEND",
    "KITCHEN_RELEASE_CONFIRM_FIRES",
    "KITCHEN_RELEASE_AUTO_ACCEPT_AND_FIRE",
];

/// The optional setting that says where the vendor's orders are priced
/// ([ADR-0248](../../../../docs/adr/0248-a-delivery-order-is-priced-paid-and-reconciled-as-the-app-sold-it.md)
/// decision 1): a `VENDOR_PRICE_SOURCE_*` token. Absent, the app's price.
pub const PRICE_SOURCE: &str = "price_source";

/// The tokens [`PRICE_SOURCE`] takes, the default first.
pub const PRICE_SOURCE_OPTIONS: &[&str] = &["VENDOR_PRICE_SOURCE_APP", "VENDOR_PRICE_SOURCE_MENU"];

/// The optional setting that says how far, in basis points, the app's price may differ from the
/// store's menu before the order is flagged. Absent, `0`: any difference.
pub const PRICE_DIFFERENCE_BPS: &str = "price_difference_bps";

/// The optional setting that says where the vendor's guest pays
/// ([ADR-0248](../../../../docs/adr/0248-a-delivery-order-is-priced-paid-and-reconciled-as-the-app-sold-it.md)
/// decision 2): a `VENDOR_PAYMENT_*` token. Absent, in the app.
pub const PAYMENT: &str = "payment";

/// The tokens [`PAYMENT`] takes, the default first.
pub const PAYMENT_OPTIONS: &[&str] = &["VENDOR_PAYMENT_IN_APP", "VENDOR_PAYMENT_AT_STORE"];
/// The optional setting that holds the vendor's commission, in basis points of what the guest paid
/// (ADR-0248 decision 3): the cloud's reconciliation reads it from the vendor's entry. Absent, `0`.
pub const COMMISSION_BPS: &str = "commission_bps";

/// Why a store's vendors cannot be published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VendorsRefusal {
    /// This connection lacks `staff_confirm` or `prep_minutes`, or its id is not a ULID.
    Incomplete(String),
    /// More delivery connections serve the store than one node lists.
    TooMany,
}

/// The vendors `store_id` takes orders from, among `connections`, each of whose provider
/// `providers` lists in the delivery family.
///
/// # Errors
///
/// A [`VendorsRefusal`].
pub fn vendors_for_store(
    connections: &[Connection],
    providers: &pos_providers::Registry,
    store_id: &str,
) -> Result<PublishedVendors, VendorsRefusal> {
    let mut vendors = Vec::new();
    for connection in connections.iter().filter(|connection| {
        connection.enabled
            && connection.scope_level == ScopeLevel::Store
            && connection.scope_id.as_deref() == Some(store_id)
            && providers
                .get(&connection.provider_id)
                .is_some_and(|provider| provider.family == Family::Delivery)
    }) {
        let incomplete = || VendorsRefusal::Incomplete(connection.connection_id.clone());
        let vendor_id = connection
            .connection_id
            .parse::<VendorId>()
            .map_err(|_| incomplete())?;
        let staff_confirm = match connection.settings.get(STAFF_CONFIRM) {
            Some(SettingValue::Flag(flag)) => *flag,
            _ => return Err(incomplete()),
        };
        let prep_minutes = match connection.settings.get(PREP_MINUTES) {
            Some(SettingValue::Number(minutes)) => u16::try_from(*minutes)
                .ok()
                .filter(|minutes| (MIN_PREP_MINUTES..=MAX_PREP_MINUTES).contains(minutes))
                .ok_or_else(incomplete)?,
            _ => return Err(incomplete()),
        };
        // Optional, and a token the provider's choice field already checked: anything else reads
        // as absent, which the store runs as two steps.
        let kitchen_release = match connection.settings.get(KITCHEN_RELEASE) {
            Some(SettingValue::Text(token))
                if KITCHEN_RELEASE_OPTIONS.contains(&token.as_str()) =>
            {
                Open::parse(token)
            }
            _ => Open::default(),
        };
        let (price_source, price_difference_bps) = price_terms(connection);
        vendors.push(PublishedVendor {
            vendor_id,
            display_name: DisplayName::new(connection.display_name.clone()),
            staff_confirm,
            prep_minutes,
            kitchen_release,
            price_source,
            price_difference_bps,
            payment: payment_of(connection),
            commission_bps: basis_points(connection, COMMISSION_BPS),
        });
    }
    vendors.sort_by_key(|vendor| vendor.vendor_id);
    let node = PublishedVendors::new(vendors);
    if node.is_valid() {
        Ok(node)
    } else {
        Err(VendorsRefusal::TooMany)
    }
}

/// A connection's price source and tolerance (ADR-0248 decision 1), each optional and already
/// checked by the provider's schema: anything else reads as absent, the app's price and `0`.
fn price_terms(connection: &Connection) -> (Open<VendorPriceSource>, u32) {
    let source = match connection.settings.get(PRICE_SOURCE) {
        Some(SettingValue::Text(token)) if PRICE_SOURCE_OPTIONS.contains(&token.as_str()) => {
            Open::parse(token)
        }
        _ => Open::default(),
    };
    (source, basis_points(connection, PRICE_DIFFERENCE_BPS))
}

/// A connection's optional basis-points setting `key`, 0 to 10,000, already checked by the
/// provider's schema: anything else reads as `0`.
fn basis_points(connection: &Connection, key: &str) -> u32 {
    match connection.settings.get(key) {
        Some(SettingValue::Number(bps)) => u32::try_from(*bps)
            .ok()
            .filter(|bps| *bps <= MAX_PRICE_DIFFERENCE_BPS)
            .unwrap_or(0),
        _ => 0,
    }
}

/// Where a connection's guest pays (ADR-0248 decision 2), optional and already checked by the
/// provider's schema: anything else reads as absent, in the app.
fn payment_of(connection: &Connection) -> Open<VendorPayment> {
    match connection.settings.get(PAYMENT) {
        Some(SettingValue::Text(token)) if PAYMENT_OPTIONS.contains(&token.as_str()) => {
            Open::parse(token)
        }
        _ => Open::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pos_proto::integrations::SettingValue;

    use pos_proto::kitchen_release::KitchenRelease;

    use pos_proto::vendors::{VendorPayment, VendorPriceSource};

    use super::{
        COMMISSION_BPS, KITCHEN_RELEASE, PAYMENT, PREP_MINUTES, PRICE_DIFFERENCE_BPS, PRICE_SOURCE,
        STAFF_CONFIRM, VendorsRefusal, vendors_for_store,
    };
    use crate::connections::{Connection, ScopeLevel};

    const STORE: &str = "01J00000000000000000000STR";

    fn delivery(id: &str, scope: Option<&str>, settings: &[(&str, SettingValue)]) -> Connection {
        Connection {
            connection_id: id.to_owned(),
            provider_id: "delivery.demo".to_owned(),
            scope_level: if scope.is_some() {
                ScopeLevel::Store
            } else {
                ScopeLevel::Tenant
            },
            scope_id: scope.map(str::to_owned),
            display_name: "Sandbox delivery".to_owned(),
            enabled: true,
            settings: settings
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
            sealed_secrets: BTreeMap::new(),
            processing_country: None,
        }
    }

    fn registry() -> pos_providers::Registry {
        pos_providers::Registry::new([&crate::connectors::demo::PROVIDER]).expect("a registry")
    }

    #[test]
    fn a_store_takes_its_own_enabled_delivery_connections_with_their_settings() {
        let settings = [
            (STAFF_CONFIRM, SettingValue::Flag(true)),
            (PREP_MINUTES, SettingValue::Number(20)),
        ];
        let mut disabled = delivery("01J0000000000000000000CNN2", Some(STORE), &settings);
        disabled.enabled = false;
        let connections = [
            delivery("01J0000000000000000000CNN1", Some(STORE), &settings),
            disabled,
            delivery("01J0000000000000000000CNN3", None, &settings),
            delivery(
                "01J0000000000000000000CNN4",
                Some("01J00000000000000000000OTH"),
                &settings,
            ),
        ];
        let node = vendors_for_store(&connections, &registry(), STORE).expect("publishes");
        assert_eq!(node.vendors().len(), 1, "only the store's own enabled one");
        let vendor = &node.vendors()[0];
        assert_eq!(vendor.vendor_id.to_string(), "01J0000000000000000000CNN1");
        assert!(vendor.staff_confirm);
        assert_eq!(vendor.prep_minutes, 20);
    }

    /// A connection's kitchen release rides its vendor's entry (ADR-0247); one that never set it
    /// takes two steps.
    #[test]
    fn a_connection_says_how_its_orders_reach_the_kitchen() {
        let mut settings = vec![
            (STAFF_CONFIRM, SettingValue::Flag(true)),
            (PREP_MINUTES, SettingValue::Number(20)),
        ];
        let plain = delivery("01J0000000000000000000CNN1", Some(STORE), &settings);
        settings.push((
            KITCHEN_RELEASE,
            SettingValue::Text("KITCHEN_RELEASE_AUTO_ACCEPT_AND_FIRE".to_owned()),
        ));
        let auto = delivery("01J0000000000000000000CNN2", Some(STORE), &settings);
        let node = vendors_for_store(&[plain, auto], &registry(), STORE).expect("publishes");
        let releases: Vec<KitchenRelease> = node
            .vendors()
            .iter()
            .map(|vendor| node.kitchen_release(Some(vendor.vendor_id)))
            .collect();
        assert_eq!(
            releases,
            [
                KitchenRelease::ConfirmThenSend,
                KitchenRelease::AutoAcceptAndFire
            ]
        );
    }

    /// A connection's price source and tolerance ride its vendor's entry (ADR-0248 decision 1);
    /// one that never set them is priced at the app, flagging any difference.
    #[test]
    fn a_connection_says_where_its_orders_are_priced() {
        let mut settings = vec![
            (STAFF_CONFIRM, SettingValue::Flag(true)),
            (PREP_MINUTES, SettingValue::Number(20)),
        ];
        let plain = delivery("01J0000000000000000000CNN1", Some(STORE), &settings);
        settings.push((
            PRICE_SOURCE,
            SettingValue::Text("VENDOR_PRICE_SOURCE_MENU".to_owned()),
        ));
        settings.push((PRICE_DIFFERENCE_BPS, SettingValue::Number(500)));
        settings.push((COMMISSION_BPS, SettingValue::Number(2_500)));
        let menu = delivery("01J0000000000000000000CNN2", Some(STORE), &settings);
        let node = vendors_for_store(&[plain, menu], &registry(), STORE).expect("publishes");
        let terms: Vec<(VendorPriceSource, u32)> = node
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
            terms,
            [(VendorPriceSource::App, 0), (VendorPriceSource::Menu, 500)]
        );
        let commissions: Vec<u32> = node
            .vendors()
            .iter()
            .map(|vendor| vendor.commission_bps)
            .collect();
        assert_eq!(commissions, [0, 2_500], "ADR-0248 decision 3");
    }

    /// A connection's payment rides its vendor's entry (ADR-0248 decision 2); one that never set
    /// it is paid in the app.
    #[test]
    fn a_connection_says_where_its_guest_pays() {
        let mut settings = vec![
            (STAFF_CONFIRM, SettingValue::Flag(true)),
            (PREP_MINUTES, SettingValue::Number(20)),
        ];
        let plain = delivery("01J0000000000000000000CNN1", Some(STORE), &settings);
        settings.push((
            PAYMENT,
            SettingValue::Text("VENDOR_PAYMENT_AT_STORE".to_owned()),
        ));
        let at_store = delivery("01J0000000000000000000CNN2", Some(STORE), &settings);
        let node = vendors_for_store(&[plain, at_store], &registry(), STORE).expect("publishes");
        let payments: Vec<VendorPayment> = node
            .vendors()
            .iter()
            .map(|vendor| VendorPayment::read(&vendor.payment))
            .collect();
        assert_eq!(payments, [VendorPayment::InApp, VendorPayment::AtStore]);
    }

    #[test]
    fn a_connection_without_its_choices_refuses_the_publish_by_name() {
        let connections = [delivery(
            "01J0000000000000000000CNN1",
            Some(STORE),
            &[(STAFF_CONFIRM, SettingValue::Flag(false))],
        )];
        assert_eq!(
            vendors_for_store(&connections, &registry(), STORE),
            Err(VendorsRefusal::Incomplete(
                "01J0000000000000000000CNN1".to_owned()
            ))
        );
    }
}
