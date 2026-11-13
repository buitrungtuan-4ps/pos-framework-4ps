// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use pos_proto::Ulid;
use pos_proto::ids::TenantId;
use pos_proto::time::Timestamp;

use super::reseal_tenant;
use crate::connections::{
    Connection, ConnectionSecret, ConnectionStore, ConnectionStoreError, ScopeLevel, SealedFor,
    seal,
};
use crate::gateway::endpoints::{
    GatewayEndpoint, GatewayEndpointStore, GatewayEndpointStoreError, GatewayMethod,
};
use crate::keys::{EnvironmentKeys, Keys, MemoryTenantKeys, TenantKeyStore, sealed_version};
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

#[derive(Clone, Copy)]
struct Clock;

impl pos_proto::ClockSource for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(1_700_000_000_000).expect("a time")
    }
}

/// One record per id, at a version that changes on every write.
#[derive(Clone)]
struct Rows<T>(Arc<Mutex<BTreeMap<String, (T, u32)>>>);

impl<T: Clone> Rows<T> {
    fn empty() -> Self {
        Self(Arc::new(Mutex::new(BTreeMap::new())))
    }

    fn put(&self, id: &str, record: T) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned(), (record, 1));
    }

    fn all(&self) -> Vec<Versioned<T>> {
        let rows = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        rows.values()
            .map(|(record, at)| Versioned::new(record.clone(), Version::new(at.to_string())))
            .collect()
    }

    fn get(&self, id: &str) -> Option<T> {
        let rows = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        rows.get(id).map(|(record, _)| record.clone())
    }

    fn update(&self, id: &str, record: &T, expected: &Version) -> UpdateOutcome {
        let mut rows = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match rows.get_mut(id) {
            None => UpdateOutcome::NotFound,
            Some((_, at)) if at.to_string() != expected.as_str() => UpdateOutcome::VersionMismatch,
            Some(row) => {
                row.1 += 1;
                row.0 = record.clone();
                UpdateOutcome::Updated(Version::new(row.1.to_string()))
            }
        }
    }
}

impl ConnectionStore for Rows<Connection> {
    async fn list(&self, _: TenantId) -> Result<Vec<Versioned<Connection>>, ConnectionStoreError> {
        Ok(self.all())
    }
    async fn get(
        &self,
        _: TenantId,
        _: &str,
    ) -> Result<Option<Versioned<Connection>>, ConnectionStoreError> {
        Ok(None)
    }
    async fn create(
        &self,
        _: TenantId,
        _: &Connection,
    ) -> Result<CreateOutcome, ConnectionStoreError> {
        Err(ConnectionStoreError::new("not in this test"))
    }
    async fn update(
        &self,
        _: TenantId,
        record: &Connection,
        expected: &Version,
    ) -> Result<UpdateOutcome, ConnectionStoreError> {
        Ok(Rows::update(self, &record.connection_id, record, expected))
    }
    async fn delete(&self, _: TenantId, _: &str) -> Result<(), ConnectionStoreError> {
        Ok(())
    }
}

impl GatewayEndpointStore for Rows<GatewayEndpoint> {
    async fn list(
        &self,
        _: TenantId,
    ) -> Result<Vec<Versioned<GatewayEndpoint>>, GatewayEndpointStoreError> {
        Ok(self.all())
    }
    async fn get(
        &self,
        _: TenantId,
        _: &str,
    ) -> Result<Option<Versioned<GatewayEndpoint>>, GatewayEndpointStoreError> {
        Ok(None)
    }
    async fn create(
        &self,
        _: TenantId,
        _: &GatewayEndpoint,
    ) -> Result<CreateOutcome, GatewayEndpointStoreError> {
        Err(GatewayEndpointStoreError::new("not in this test"))
    }
    async fn update(
        &self,
        _: TenantId,
        record: &GatewayEndpoint,
        expected: &Version,
    ) -> Result<UpdateOutcome, GatewayEndpointStoreError> {
        Ok(Rows::update(
            self,
            &record.gateway_endpoint_id,
            record,
            expected,
        ))
    }
    async fn delete(&self, _: TenantId, _: &str) -> Result<(), GatewayEndpointStoreError> {
        Ok(())
    }
}

const CONNECTION: &str = "01J0000000000000000000CN01";
const ENDPOINT: &str = "01J0000000000000000000GW01";

fn legacy() -> ConnectionSecret {
    ConnectionSecret::new("5a".repeat(32))
}

fn tenant() -> TenantId {
    TenantId::new(Ulid::from_u128(7))
}

fn connection(sealed: &str) -> Connection {
    Connection {
        connection_id: CONNECTION.to_owned(),
        provider_id: "demo".to_owned(),
        scope_level: ScopeLevel::Tenant,
        scope_id: None,
        display_name: "Demo".to_owned(),
        enabled: true,
        settings: pos_providers::Settings::default(),
        sealed_secrets: [("api_key".to_owned(), sealed.to_owned())].into(),
        processing_country: None,
    }
}

fn endpoint(sealed: &str) -> GatewayEndpoint {
    GatewayEndpoint {
        gateway_endpoint_id: ENDPOINT.to_owned(),
        display_name: "Loyalty".to_owned(),
        url: "https://93.184.216.34/balance".to_owned(),
        method: GatewayMethod::Post,
        request_fields: Vec::new(),
        response_fields: Vec::new(),
        deadline_ms: 3_000,
        idempotent: true,
        enabled: true,
        sealed_headers: [("x-api-key".to_owned(), sealed.to_owned())].into(),
    }
}

fn sealed_for(record: &'static str, field_key: &'static str) -> SealedFor<'static> {
    SealedFor {
        tenant_id: tenant(),
        connection_id: record,
        field_key,
    }
}

/// ADR-0217 decision 3: values from before tenant keys and under an older version move to the
/// newest, still open to the same text, within the run's bound; once none is left, the older
/// version is retired; erasing the keys then leaves nothing readable.
#[tokio::test]
async fn the_pass_moves_every_value_to_the_newest_key_and_retires_the_old() {
    let store = MemoryTenantKeys::default();
    let keeper = EnvironmentKeys::from_file_key(&legacy()).expect("a keeper");
    let keys = Keys::new(keeper, store.clone(), Some(legacy()), Clock);
    let connections = Rows::<Connection>::empty();
    let endpoints = Rows::<GatewayEndpoint>::empty();
    connections.put(
        CONNECTION,
        connection(&seal(&legacy(), sealed_for(CONNECTION, "api_key"), "fake-api").expect("seals")),
    );
    let first = keys.for_tenant(tenant()).await.expect("keys");
    endpoints.put(
        ENDPOINT,
        endpoint(
            &first
                .seal(sealed_for(ENDPOINT, "header:x-api-key"), "fake-header")
                .expect("seals"),
        ),
    );
    assert_eq!(keys.rotate_tenant(tenant()).await.expect("rotates"), 2);

    let bounded = reseal_tenant(&keys, &connections, &endpoints, tenant(), 1)
        .await
        .expect("runs");
    assert_eq!(
        (bounded.resealed, bounded.remaining, bounded.retired),
        (1, 1, 0)
    );
    let done = reseal_tenant(&keys, &connections, &endpoints, tenant(), 10)
        .await
        .expect("runs");
    assert_eq!(
        (done.newest_version, done.resealed, done.remaining),
        (2, 1, 0)
    );
    assert_eq!(done.retired, 1, "version 1 holds nothing any more");
    assert_eq!(store.versions(tenant()).await.expect("versions").len(), 1);

    let now = keys.for_tenant(tenant()).await.expect("keys");
    let api = connections.get(CONNECTION).expect("kept").sealed_secrets["api_key"].clone();
    let header = endpoints.get(ENDPOINT).expect("kept").sealed_headers["x-api-key"].clone();
    assert_eq!(sealed_version(&api), Some(2));
    assert_eq!(
        *now.open(sealed_for(CONNECTION, "api_key"), &api)
            .expect("opens"),
        "fake-api"
    );
    assert_eq!(
        *now.open(sealed_for(ENDPOINT, "header:x-api-key"), &header)
            .expect("opens"),
        "fake-header"
    );

    assert_eq!(keys.erase_tenant(tenant()).await.expect("erases"), 1);
    let after = Keys::new(
        EnvironmentKeys::from_file_key(&legacy()).expect("a keeper"),
        store,
        Some(legacy()),
        Clock,
    );
    let fresh = after.for_tenant(tenant()).await.expect("a new first key");
    assert!(
        fresh.open(sealed_for(CONNECTION, "api_key"), &api).is_err(),
        "an erased key opens nothing"
    );
}
