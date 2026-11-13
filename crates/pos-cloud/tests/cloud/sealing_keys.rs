// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The keys the integration tests seal under (ADR-0217): the file keeper over a test
//! `integration_secret`, and an in-memory tenant-key store per test file, so a value a router
//! sealed opens again under the same tenant's key. Each file configures its own key, and a tenant
//! key wrapped under one does not unwrap under another, as across two environments.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use pos_cloud::clock::SystemClock;
use pos_cloud::connections::ConnectionSecret;
use pos_cloud::keys::{EnvironmentKeys, Keys, MemoryTenantKeys};

static TENANT_KEYS: LazyLock<Mutex<BTreeMap<&'static str, MemoryTenantKeys>>> =
    LazyLock::new(Mutex::default);

/// The keys over `secret` for the test file `file`: tenant keys wrapped by it, and values from
/// before tenant keys opened under it.
pub(crate) fn file_keys(file: &'static str, secret: &ConnectionSecret) -> Keys {
    let keeper = EnvironmentKeys::from_file_key(secret).expect("a well-formed test key");
    let store = TENANT_KEYS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(file)
        .or_default()
        .clone();
    Keys::new(keeper, store, Some(secret.clone()), SystemClock)
}
