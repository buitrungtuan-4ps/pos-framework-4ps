// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the X/Z report asks of the store's configuration: the names its tills go by.

use core::future::Future;
use std::collections::BTreeMap;

use pos_proto::ids::{StoreId, TenantId};

use crate::types::DailyCash;

/// The names a store's published configuration gives the tills among a day's cash drawers, by
/// `terminal_device_id` ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md)
/// decision 13), for the X/Z report to show beside each drawer.
///
/// The reports need this one fact from the configuration tree, which the rest of the cloud owns.
/// This trait is that seam, and nothing more: the cloud implements it over its configuration trees
/// and hands it to the reporting routes through [`ReportsState`](super::state::ReportsState). It
/// is not a port to the outside world, so it lives with the reports and needs no ADR of its own.
///
/// A till the configuration does not name is left out, and so is every till when the configuration
/// cannot be read: the report's figures stand without the names.
pub trait TillNames {
    /// The names of the tills among `cash`'s drawers that the store's configuration names.
    fn till_names(
        &self,
        store: (TenantId, StoreId),
        cash: &DailyCash,
    ) -> impl Future<Output = BTreeMap<String, String>> + Send;
}
