// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's fee rules (ADR-0159, migration 0075).

use super::{TENANT_A, TENANT_B, block_on, prepared};

use store_postgres::{FeeRuleSlot, FeeRuleWrite, PostgresFeeRules};

const FEE: &str = "01J000000000000000000000FE";

const STORE_SLOT: FeeRuleSlot<'static> = FeeRuleSlot {
    scope: "FEE_SCOPE_STORE",
    scope_id: "01J0000000000000000000STOR",
    fee_id: FEE,
};

/// A rule as `pos-cloud` writes one. The adapter treats `doc` as opaque JSON, so the test crate
/// needs none of the cloud's types.
fn doc(code: &str) -> String {
    serde_json::json!({
        "fee_id": FEE,
        "code": code,
        "display_name": "Service charge",
        "kind": "FEE_KIND_PERCENT",
        "rate": { "numerator": 5, "denominator": 100 },
    })
    .to_string()
}

/// Writes `doc_json` into `slot` for `tenant`, at `update_time_ms`, as `updated_by`.
async fn write(
    rules: &PostgresFeeRules,
    tenant: &str,
    slot: FeeRuleSlot<'_>,
    doc_json: &str,
    update_time_ms: i64,
    updated_by: &str,
) {
    rules
        .upsert(
            tenant,
            slot,
            FeeRuleWrite {
                doc_json,
                update_time_ms,
                updated_by,
            },
        )
        .await
        .expect("write a fee rule");
}

#[test]
fn a_rule_is_written_replaced_and_deleted_in_its_slot() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let rules = store.fee_rules();

        let first = doc("SERVICE");
        write(&rules, TENANT_A, STORE_SLOT, &first, 1_000, "admin-1").await;
        write(
            &rules,
            TENANT_A,
            STORE_SLOT,
            &doc("SERVICE_2"),
            2_000,
            "admin-2",
        )
        .await;
        let tenant_slot = FeeRuleSlot {
            scope: "FEE_SCOPE_TENANT",
            scope_id: TENANT_A,
            ..STORE_SLOT
        };
        write(&rules, TENANT_A, tenant_slot, &first, 3_000, "admin-1").await;
        // Two brands whose ids sort one way by bytes and the other way in a natural-language
        // collation, so the list's order is pinned to the bytes, as the in-memory store's is.
        for brand in ["a-brand", "B-brand"] {
            let brand_slot = FeeRuleSlot {
                scope: "FEE_SCOPE_BRAND",
                scope_id: brand,
                ..STORE_SLOT
            };
            write(&rules, TENANT_A, brand_slot, &first, 4_000, "admin-1").await;
        }

        let listed = rules.fetch(TENANT_A).await.expect("list");
        assert_eq!(
            listed
                .iter()
                .map(|row| (row.scope.as_str(), row.scope_id.as_str()))
                .collect::<Vec<_>>(),
            [
                ("FEE_SCOPE_BRAND", "B-brand"),
                ("FEE_SCOPE_BRAND", "a-brand"),
                ("FEE_SCOPE_STORE", STORE_SLOT.scope_id),
                ("FEE_SCOPE_TENANT", TENANT_A),
            ],
            "the second write to a slot replaced the first, each scope's is its own row, and \
             the list is in scope and scope-id order, by bytes"
        );
        let store_rule = listed
            .iter()
            .find(|row| row.scope == "FEE_SCOPE_STORE")
            .expect("the store's rule");
        assert!(store_rule.doc_json.contains("SERVICE_2"), "the later rule");
        assert_eq!(store_rule.update_time_ms, 2_000, "the cloud's clock, kept");
        assert_eq!(store_rule.updated_by, "admin-2");
        assert_eq!(store_rule.fee_id, FEE);
        assert!(
            rules.fetch(TENANT_B).await.expect("list").is_empty(),
            "another tenant reads nothing"
        );

        assert!(
            !rules.delete(TENANT_B, STORE_SLOT).await.expect("delete"),
            "another tenant cannot delete it"
        );
        assert!(rules.delete(TENANT_A, STORE_SLOT).await.expect("delete"));
        assert!(
            !rules
                .delete(TENANT_A, STORE_SLOT)
                .await
                .expect("delete again"),
            "deleting what is not there is no error"
        );
        assert_eq!(rules.fetch(TENANT_A).await.expect("list").len(), 3);
    });
}

/// A rule with every field set comes back from the `jsonb` column as the rule it was written:
/// the column keeps its own spacing and key order, and what `pos-cloud` parses from that text is
/// still the same rule, its money, its translated name and its open enums included.
#[test]
fn a_whole_rule_reads_back_from_jsonb_as_it_was_written() {
    use std::collections::BTreeMap;

    use pos_proto::Ulid;
    use pos_proto::enums::SalesChannel;
    use pos_proto::fees::{FeeCode, FeeItems, FeeKind, FeeTax, PublishedFee};
    use pos_proto::ids::{FeeId, MenuItemId, TaxClassId};
    use pos_proto::money::{CurrencyCode, Money, Ratio};
    use pos_proto::text::DisplayName;
    use pos_proto::wire_enum::Open;

    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let rules = store.fee_rules();
        let fee_id = FeeId::new(Ulid::from_u128(0xFE));
        let rule = PublishedFee {
            fee_id,
            code: FeeCode::new("PACKAGING"),
            display_name: DisplayName::new("Packaging"),
            display_name_translations: BTreeMap::from([(
                "vi".to_owned(),
                DisplayName::new("Phí đóng gói"),
            )]),
            kind: Open::from_known(FeeKind::AmountPerUnit),
            rate: Some(Ratio::percent(5).expect("a rate")),
            amount: Some(Money::new(CurrencyCode::VND, 5_000)),
            channels: vec![
                Open::from_known(SalesChannel::Takeaway),
                Open::from_known(SalesChannel::Delivery),
            ],
            item_scope: Open::from_known(FeeItems::Include),
            menu_item_ids: vec![
                MenuItemId::new(Ulid::from_u128(1)),
                MenuItemId::new(Ulid::from_u128(2)),
            ],
            base_discounted: false,
            base_tax_inclusive: true,
            tax: Open::from_known(FeeTax::TaxClass),
            tax_class_id: Some(TaxClassId::new(Ulid::from_u128(3))),
            waivable: true,
            active: false,
        };
        let doc_json = serde_json::to_string(&rule).expect("encode the rule");
        let fee_id_text = fee_id.to_string();
        let slot = FeeRuleSlot {
            fee_id: &fee_id_text,
            ..STORE_SLOT
        };
        write(&rules, TENANT_A, slot, &doc_json, 1_000, "admin-1").await;

        let listed = rules.fetch(TENANT_A).await.expect("list");
        assert_eq!(listed.len(), 1);
        let row = listed.first().expect("the rule");
        assert_ne!(row.doc_json, doc_json, "jsonb keeps its own spacing");
        let read: PublishedFee =
            serde_json::from_str(&row.doc_json).expect("the stored rule parses");
        assert_eq!(read, rule);
    });
}

#[test]
fn the_query_role_sees_only_its_own_tenants_rules() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let rules = store.fee_rules();
        let rule = doc("SERVICE");
        for tenant in [TENANT_A, TENANT_B] {
            write(&rules, tenant, STORE_SLOT, &rule, 1_000, "admin-1").await;
        }

        admin
            .batch_execute("SET ROLE app_tenant")
            .await
            .expect("assume the app_tenant role");
        admin
            .execute(
                "SELECT set_config('app.tenant_id', $1, false)",
                &[&TENANT_A],
            )
            .await
            .expect("scope the session to tenant A");
        let visible: Vec<String> = admin
            .query("SELECT tenant_id FROM fee_rules", &[])
            .await
            .expect("readable")
            .iter()
            .map(|row| row.get(0))
            .collect();
        admin
            .batch_execute("RESET ROLE")
            .await
            .expect("reset the role");
        assert_eq!(visible, vec![TENANT_A.to_owned()]);
    });
}
