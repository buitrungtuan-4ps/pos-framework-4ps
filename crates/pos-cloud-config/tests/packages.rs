// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The in-memory package store holds the `PackageStore` contract.

mod package_contract;

use pos_cloud_config::packages::InMemoryPackages;

#[tokio::test]
async fn the_in_memory_store_holds_the_package_contract() {
    package_contract::holds(&InMemoryPackages::new(), 0).await;
}
