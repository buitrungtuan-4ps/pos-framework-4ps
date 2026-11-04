// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The custom-value projection (ADR-0194 decision 3, migration 0092).

use pos_proto::Ulid;
use pos_proto::ids::{EventId, StoreId, TenantId};
use store_postgres::CustomValueWrite;

use super::{block_on, prepared};

fn upsert(object_id: &str, field: &str, value: &str) -> CustomValueWrite {
    CustomValueWrite::Upsert {
        object_id: object_id.to_owned(),
        custom_field_id: field.to_owned(),
        object: Some("DATA_OBJECT_ORDER".to_owned()),
        object_type_id: None,
        value_json: value.to_owned(),
        subject_id: None,
        package_version_id: "01J00000000000000000000V01".to_owned(),
        update_time_ms: 1_767_225_600_000,
    }
}

/// A page's writes and its cursor land together; a later value replaces an earlier one; a cleared
/// value removes its row; and a tenant sees only its own rows, by key and, as the query role, by
/// policy.
#[test]
fn values_are_upserted_cleared_and_isolated_with_their_cursor() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let values = store.custom_values();
        let tenant_a = TenantId::new(Ulid::from_u128(0x0A));
        let tenant_b = TenantId::new(Ulid::from_u128(0x0B));
        let shop = StoreId::new(Ulid::from_u128(0x5));
        let first = EventId::new(Ulid::from_u128(0x101));
        let second = EventId::new(Ulid::from_u128(0x102));

        assert_eq!(values.cursor(tenant_a, shop).await.expect("cursor"), None);
        values
            .apply(
                tenant_a,
                shop,
                &[
                    upsert(
                        "ORDER1",
                        "FIELD1",
                        r#"{"custom_field_id":"FIELD1","integer":2}"#,
                    ),
                    upsert(
                        "ORDER1",
                        "FIELD2",
                        r#"{"custom_field_id":"FIELD2","boolean":true}"#,
                    ),
                ],
                first,
            )
            .await
            .expect("first page");
        values
            .apply(
                tenant_a,
                shop,
                &[
                    upsert(
                        "ORDER1",
                        "FIELD1",
                        r#"{"custom_field_id":"FIELD1","integer":4}"#,
                    ),
                    CustomValueWrite::Delete {
                        object_id: "ORDER1".to_owned(),
                        custom_field_id: "FIELD2".to_owned(),
                    },
                ],
                second,
            )
            .await
            .expect("second page");
        assert_eq!(
            values.cursor(tenant_a, shop).await.expect("cursor"),
            Some(second.to_string())
        );
        assert_eq!(values.cursor(tenant_b, shop).await.expect("cursor"), None);

        let rows = admin
            .query(
                "SELECT custom_field_id, value->>'integer' FROM custom_values \
                 WHERE tenant_id = $1 ORDER BY custom_field_id",
                &[&tenant_a.to_string()],
            )
            .await
            .expect("read rows");
        let read: Vec<(String, Option<String>)> =
            rows.iter().map(|row| (row.get(0), row.get(1))).collect();
        assert_eq!(read, vec![("FIELD1".to_owned(), Some("4".to_owned()))]);

        admin
            .batch_execute(&format!(
                "SET ROLE app_tenant; SET app.tenant_id = '{tenant_b}'"
            ))
            .await
            .expect("assume tenant B");
        let seen = admin
            .query("SELECT 1 FROM custom_values", &[])
            .await
            .expect("query as B");
        admin
            .batch_execute("RESET ROLE; RESET app.tenant_id")
            .await
            .expect("reset");
        assert!(seen.is_empty(), "tenant B sees none of tenant A's values");
    });
}
