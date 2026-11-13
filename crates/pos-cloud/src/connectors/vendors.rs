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
use pos_proto::vendors::{MAX_PREP_MINUTES, MIN_PREP_MINUTES, PublishedVendor, PublishedVendors};
use pos_providers::Family;

use crate::connections::{Connection, ScopeLevel};

/// The setting that says whether a member of staff confirms each of the vendor's orders.
pub const STAFF_CONFIRM: &str = "staff_confirm";
/// The setting that says how many minutes the store tells the vendor an accepted order takes.
pub const PREP_MINUTES: &str = "prep_minutes";

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
        vendors.push(PublishedVendor {
            vendor_id,
            display_name: DisplayName::new(connection.display_name.clone()),
            staff_confirm,
            prep_minutes,
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pos_proto::integrations::SettingValue;

    use super::{PREP_MINUTES, STAFF_CONFIRM, VendorsRefusal, vendors_for_store};
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
