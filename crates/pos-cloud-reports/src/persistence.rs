// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The rollup store over PostgreSQL.

use store_postgres::PostgresRollups;

use pos_proto::ids::{StoreId, TenantId};

use crate::dashboard::projection::{RollupError, RollupStore, StoredRollups};

impl RollupStore for PostgresRollups {
    async fn load(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<StoredRollups, RollupError> {
        match self
            .load_state(tenant, store_id)
            .await
            .map_err(|error| RollupError::store(error.to_string()))?
        {
            Some(json) => serde_json::from_str(&json).map_err(|error| {
                RollupError::store(format!("decoding the stored rollup state failed: {error}"))
            }),
            // A store with no row yet reads as an empty, cursor-less rollup — the same default the
            // projector starts from, so a first read and a first projection agree.
            None => Ok(StoredRollups::default()),
        }
    }

    async fn save(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        rollups: &StoredRollups,
    ) -> Result<(), RollupError> {
        let json = serde_json::to_string(rollups).map_err(|error| {
            RollupError::store(format!("encoding the rollup state failed: {error}"))
        })?;
        self.save_state(tenant, store_id, &json)
            .await
            .map_err(|error| RollupError::store(error.to_string()))
    }
}

impl crate::custom_data::CustomValueStore for store_postgres::PostgresCustomValues {
    async fn cursor(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Option<pos_proto::ids::EventId>, pos_ports::PortError> {
        self.cursor(tenant, store_id)
            .await?
            .map(|text| {
                text.parse().map_err(|_| {
                    pos_ports::PortError::internal(
                        pos_ports::PortName::EventStore,
                        "a stored custom-value cursor is not an event id",
                    )
                })
            })
            .transpose()
    }

    async fn apply(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        changes: &[crate::custom_data::CustomValueChange],
        cursor: pos_proto::ids::EventId,
    ) -> Result<(), pos_ports::PortError> {
        let writes = changes
            .iter()
            .map(custom_value_write)
            .collect::<Result<Vec<_>, _>>()?;
        self.apply(tenant, store_id, &writes, cursor).await
    }
}

/// One change as the table's write: a row's value as the event's JSON, its object as its kind's
/// token or its object type's id.
fn custom_value_write(
    change: &crate::custom_data::CustomValueChange,
) -> Result<store_postgres::CustomValueWrite, pos_ports::PortError> {
    use crate::custom_data::{CustomObject, CustomValueChange};
    use pos_proto::wire_enum::WireEnum;

    Ok(match change {
        CustomValueChange::Set(row) => {
            let (object, object_type_id) = match &row.object {
                CustomObject::Existing(kind) => (Some(kind.known().as_wire().to_owned()), None),
                CustomObject::Record(object_type) => (None, Some(object_type.to_string())),
            };
            store_postgres::CustomValueWrite::Upsert {
                object_id: row.object_id.to_string(),
                custom_field_id: row.value.custom_field_id.to_string(),
                object,
                object_type_id,
                value_json: serde_json::to_string(&row.value).map_err(|_| {
                    pos_ports::PortError::internal(
                        pos_ports::PortName::EventStore,
                        "a custom value could not be encoded",
                    )
                })?,
                subject_id: row.subject_id.map(|subject| subject.to_string()),
                package_version_id: row.package_version_id.to_string(),
                update_time_ms: row.event_time.as_milliseconds_since_epoch(),
            }
        }
        CustomValueChange::Cleared {
            object_id,
            custom_field_id,
        } => store_postgres::CustomValueWrite::Delete {
            object_id: object_id.to_string(),
            custom_field_id: custom_field_id.to_string(),
        },
    })
}
