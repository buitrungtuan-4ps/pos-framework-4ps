// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The custom-value projection (ADR-0194 decision 3, migration 0092).

use pos_proto::Ulid;
use pos_proto::ids::{EventId, StoreId, TenantId};
use store_postgres::CustomValueWrite;

use super::{block_on, prepared};

fn upsert(object_id: &str, field: &str, value: &str) -> CustomValueWrite {
    upsert_with(object_id, field, value, None)
}

/// The same write, for an object whose personal values a subject record holds.
fn upsert_for_subject(object_id: &str, field: &str, value: &str) -> CustomValueWrite {
    upsert_with(object_id, field, value, Some("SUBJECT0000000000000000AA"))
}

fn upsert_with(
    object_id: &str,
    field: &str,
    value: &str,
    subject: Option<&str>,
) -> CustomValueWrite {
    CustomValueWrite::Upsert {
        object_id: object_id.to_owned(),
        custom_field_id: field.to_owned(),
        object: Some("DATA_OBJECT_ORDER".to_owned()),
        object_type_id: None,
        value_json: value.to_owned(),
        subject_id: subject.map(str::to_owned),
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

/// A summary counts objects per value, the most first, keeps only the objects a filter matches,
/// and answers values and counts alone: no `subject_id` is read or returned, even where the rows
/// carry one.
#[test]
fn a_summary_counts_values_and_never_returns_a_subject() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let values = store.custom_values();
        let tenant = TenantId::new(Ulid::from_u128(0x0A));
        let shop = StoreId::new(Ulid::from_u128(0x5));
        let birthday = r#"{"custom_field_id":"OCCASION","list_entry_id":"BIRTHDAY"}"#;
        let wedding = r#"{"custom_field_id":"OCCASION","list_entry_id":"WEDDING"}"#;
        values
            .apply(
                tenant,
                shop,
                &[
                    upsert_for_subject("ORDER1", "OCCASION", birthday),
                    upsert_for_subject("ORDER2", "OCCASION", birthday),
                    upsert_for_subject("ORDER3", "OCCASION", wedding),
                    upsert(
                        "ORDER1",
                        "TERRACE",
                        r#"{"custom_field_id":"TERRACE","boolean":true}"#,
                    ),
                ],
                EventId::new(Ulid::from_u128(0x201)),
            )
            .await
            .expect("seed");

        let scope = |filter: Option<(&str, &str)>| store_postgres::SummaryScope {
            custom_field_id: "OCCASION".to_owned(),
            object: Some("DATA_OBJECT_ORDER".to_owned()),
            object_type_id: None,
            filter_field_id: filter.map(|(field, _)| field.to_owned()),
            filter_json: filter.map(|(_, json)| json.to_owned()),
        };
        let all = values
            .summary(tenant, shop, &scope(None), 10)
            .await
            .expect("summary");
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.first().map(|(_, count)| *count),
            Some(2),
            "birthdays first"
        );
        for (value, _) in &all {
            assert!(!value.contains("subject"), "{value}");
        }

        let on_the_terrace = values
            .summary(
                tenant,
                shop,
                &scope(Some(("TERRACE", r#"{"boolean":true}"#))),
                10,
            )
            .await
            .expect("filtered summary");
        assert_eq!(on_the_terrace.len(), 1);
        assert_eq!(on_the_terrace.first().map(|(_, count)| *count), Some(1));
    });
}

/// The stores whose events name a subject (migration 0093): a row once per store and subject, a
/// confirmation stamps it once, a confirmation for a subject the store never named makes no row,
/// and only ids are kept.
#[test]
fn a_subject_is_held_per_store_and_its_masking_is_recorded_once() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let values = store.custom_values();
        let tenant = TenantId::new(Ulid::from_u128(0x0A));
        let shop = StoreId::new(Ulid::from_u128(0x5));
        let subject = "SUBJECT0000000000000000AA";
        let named = |time: i64| CustomValueWrite::Subject {
            subject_id: subject.to_owned(),
            first_time_ms: time,
        };
        let masked = |id: &str, time: i64| CustomValueWrite::SubjectMasked {
            subject_id: id.to_owned(),
            masked_time_ms: time,
        };
        values
            .apply(
                tenant,
                shop,
                &[
                    named(1_000),
                    named(2_000),
                    masked("SUBJECT0000000000000000ZZ", 2_500),
                ],
                EventId::new(Ulid::from_u128(0x301)),
            )
            .await
            .expect("named");
        assert_eq!(
            values
                .subject_holders(tenant, subject)
                .await
                .expect("reads"),
            vec![(shop.to_string(), None)]
        );
        assert!(
            values
                .subject_holders(tenant, "SUBJECT0000000000000000ZZ")
                .await
                .expect("reads")
                .is_empty(),
            "a confirmation for a subject never named makes no row"
        );

        values
            .apply(
                tenant,
                shop,
                &[masked(subject, 3_000), masked(subject, 4_000)],
                EventId::new(Ulid::from_u128(0x302)),
            )
            .await
            .expect("masked");
        assert_eq!(
            values
                .subject_holders(tenant, subject)
                .await
                .expect("reads"),
            vec![(shop.to_string(), Some(3_000))],
            "the first confirmation's time is kept"
        );
        assert_eq!(
            values
                .masked_among(
                    tenant,
                    shop,
                    &[subject.to_owned(), "SUBJECT0000000000000000BB".to_owned()]
                )
                .await
                .expect("reads"),
            vec![subject.to_owned()]
        );
        let other = TenantId::new(Ulid::from_u128(0x0B));
        assert!(
            values
                .subject_holders(other, subject)
                .await
                .expect("reads")
                .is_empty(),
            "another tenant sees no holder"
        );
    });
}
