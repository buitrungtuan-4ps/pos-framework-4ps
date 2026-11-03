// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! [`PackageStore`] over `store-postgres`'s `packages` and `package_versions` tables. The SQL stays
//! in the adapter; this is the (de)serialisation, as for the settings store.

use serde::Serialize;
use serde::de::DeserializeOwned;
use store_postgres::PostgresPackages;

use pos_proto::ids::{PackageId, PackageVersionId, TenantId};

use super::{PackageRecord, PackageStore, PackageStoreError, PackageVersionRecord, first_version};
use crate::version::CreateOutcome;

fn failed(error: &impl ToString) -> PackageStoreError {
    PackageStoreError::new(error.to_string())
}

fn decode<T: DeserializeOwned>(doc: &str) -> Result<T, PackageStoreError> {
    serde_json::from_str(doc).map_err(|error| {
        PackageStoreError::new(format!(
            "a stored package row could not be decoded: {error}"
        ))
    })
}

fn encode<T: Serialize>(record: &T) -> Result<String, PackageStoreError> {
    serde_json::to_string(record).map_err(|error| {
        PackageStoreError::new(format!("could not serialize a package row: {error}"))
    })
}

fn created(inserted: bool) -> CreateOutcome {
    if inserted {
        CreateOutcome::Created(first_version())
    } else {
        CreateOutcome::AlreadyExists
    }
}

impl PackageStore for PostgresPackages {
    async fn list_packages(
        &self,
        tenant_id: TenantId,
    ) -> Result<Vec<PackageRecord>, PackageStoreError> {
        let docs = self
            .fetch_packages(&tenant_id.to_string())
            .await
            .map_err(|error| failed(&error))?;
        docs.iter().map(|doc| decode(doc)).collect()
    }

    async fn get_package(
        &self,
        tenant_id: TenantId,
        package_id: PackageId,
    ) -> Result<Option<PackageRecord>, PackageStoreError> {
        self.fetch_package(&tenant_id.to_string(), &package_id.to_string())
            .await
            .map_err(|error| failed(&error))?
            .map(|doc| decode(&doc))
            .transpose()
    }

    async fn create_package(
        &self,
        tenant_id: TenantId,
        package: &PackageRecord,
    ) -> Result<CreateOutcome, PackageStoreError> {
        let doc = encode(package)?;
        self.insert_package(
            &tenant_id.to_string(),
            &package.package_id.to_string(),
            &doc,
        )
        .await
        .map(created)
        .map_err(|error| failed(&error))
    }

    async fn list_versions(
        &self,
        tenant_id: TenantId,
        package_id: PackageId,
    ) -> Result<Vec<PackageVersionRecord>, PackageStoreError> {
        let docs = self
            .fetch_versions(&tenant_id.to_string(), &package_id.to_string())
            .await
            .map_err(|error| failed(&error))?;
        docs.iter().map(|doc| decode(doc)).collect()
    }

    async fn get_version(
        &self,
        tenant_id: TenantId,
        package_version_id: PackageVersionId,
    ) -> Result<Option<PackageVersionRecord>, PackageStoreError> {
        self.fetch_version(&tenant_id.to_string(), &package_version_id.to_string())
            .await
            .map_err(|error| failed(&error))?
            .map(|doc| decode(&doc))
            .transpose()
    }

    async fn publish_version(
        &self,
        tenant_id: TenantId,
        version: &PackageVersionRecord,
    ) -> Result<CreateOutcome, PackageStoreError> {
        let doc = encode(version)?;
        self.insert_version(
            &tenant_id.to_string(),
            &version.content.package_version_id.to_string(),
            &version.content.package_id.to_string(),
            &doc,
        )
        .await
        .map(created)
        .map_err(|error| failed(&error))
    }
}
