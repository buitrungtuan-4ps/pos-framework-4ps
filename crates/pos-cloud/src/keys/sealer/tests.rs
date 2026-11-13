// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_ports::key_keeper::KeyKeeper;
use pos_ports::key_vault::Secret;
use pos_proto::ids::TenantId;
use pos_proto::time::Timestamp;

use super::{Keys, sealed_version};
use crate::connections::{ConnectionSecret, SealError, SealedFor, seal};
use crate::keys::{EnvironmentKeys, MemoryTenantKeys, TenantKey, TenantKeyStore};

#[derive(Clone, Copy)]
struct FixedClock;

impl pos_proto::ClockSource for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(1_700_000_000_000).expect("a time")
    }
}

fn legacy() -> ConnectionSecret {
    ConnectionSecret::new("ab".repeat(32))
}

fn keeper() -> EnvironmentKeys {
    EnvironmentKeys::from_file_key(&legacy()).expect("a file keeper")
}

fn tenant(n: u128) -> TenantId {
    TenantId::new(pos_proto::Ulid::from_u128(n))
}

fn field(tenant_id: TenantId) -> SealedFor<'static> {
    SealedFor {
        tenant_id,
        connection_id: "01J0000000000000000000CN01",
        field_key: "api_key",
    }
}

/// ADR-0217 decision 2: a value sealed before tenant keys, with no prefix, still opens.
#[tokio::test]
async fn a_value_from_before_tenant_keys_still_opens() {
    let keys = Keys::new(
        keeper(),
        MemoryTenantKeys::default(),
        Some(legacy()),
        FixedClock,
    );
    let old = seal(&legacy(), field(tenant(1)), "fake-vendor-key").expect("seals");
    assert_eq!(sealed_version(&old), Some(0));
    let tenant_keys = keys.for_tenant(tenant(1)).await.expect("a sealer");
    assert_eq!(
        *tenant_keys.open(field(tenant(1)), &old).expect("opens"),
        "fake-vendor-key"
    );

    let without_legacy = Keys::new(keeper(), MemoryTenantKeys::default(), None, FixedClock);
    let tenant_keys = without_legacy
        .for_tenant(tenant(1))
        .await
        .expect("a sealer");
    assert!(matches!(
        tenant_keys.open(field(tenant(1)), &old),
        Err(SealError::Key)
    ));
}

/// A new value names its tenant-key version, opens for its tenant and field only, and the tenant's
/// first key is made once.
#[tokio::test]
async fn a_new_value_is_sealed_under_its_tenants_own_key() {
    let store = MemoryTenantKeys::default();
    let keys = Keys::new(keeper(), store.clone(), Some(legacy()), FixedClock);
    let tenant_keys = keys.for_tenant(tenant(1)).await.expect("a sealer");
    let sealed = tenant_keys
        .seal(field(tenant(1)), "fake-vendor-key")
        .expect("seals");
    assert!(sealed.starts_with("t1."), "{sealed}");
    assert!(!sealed.contains("fake-vendor-key"));

    let again = keys.for_tenant(tenant(1)).await.expect("a sealer");
    assert_eq!(
        *again.open(field(tenant(1)), &sealed).expect("opens"),
        "fake-vendor-key"
    );
    assert_eq!(store.versions(tenant(1)).await.expect("versions").len(), 1);

    // Another tenant has another key, and the value is bound to its tenant besides.
    let other = keys.for_tenant(tenant(2)).await.expect("a sealer");
    assert!(other.open(field(tenant(2)), &sealed).is_err());
    assert!(matches!(
        again.open(field(tenant(2)), &sealed),
        Err(SealError::Key)
    ));
    assert!(matches!(
        tenant_keys.open(field(tenant(1)), "tx.00"),
        Err(SealError::Malformed)
    ));
}

/// A newer version seals new values, and every version still held opens what it sealed.
#[tokio::test]
async fn a_newer_tenant_key_seals_and_the_older_still_opens() {
    let store = MemoryTenantKeys::default();
    let keys = Keys::new(keeper(), store.clone(), None, FixedClock);
    let first = keys.for_tenant(tenant(1)).await.expect("a sealer");
    let old = first.seal(field(tenant(1)), "first").expect("seals");
    let wrapped = keeper()
        .wrap(&Secret::new(vec![3; 32]))
        .await
        .expect("wraps");
    let second = TenantKey {
        key_version: 2,
        wrapped,
        created_at_ms: 1,
    };
    assert!(store.insert(tenant(1), &second).await.expect("stores"));
    assert!(
        !store.insert(tenant(1), &second).await.expect("stores"),
        "a version is stored once"
    );

    let tenant_keys = keys.for_tenant(tenant(1)).await.expect("a sealer");
    assert_eq!(tenant_keys.newest_version(), 2);
    let new = tenant_keys.seal(field(tenant(1)), "second").expect("seals");
    assert_eq!(sealed_version(&new), Some(2));
    assert_eq!(
        *tenant_keys.open(field(tenant(1)), &old).expect("opens"),
        "first"
    );
    assert_eq!(
        format!("{tenant_keys:?}"),
        "TenantSealer { versions: [1, 2], .. }"
    );
}
