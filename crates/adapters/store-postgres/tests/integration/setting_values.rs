// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's setting values (ADR-0160, migration 0072).

use super::{TENANT_A, TENANT_B, block_on, prepared};

use store_postgres::SettingSlot;

const STORE_SLOT: SettingSlot<'static> = SettingSlot {
    setting_key: "shift.no_shift_selling",
    scope: "SETTING_SCOPE_STORE",
    scope_id: "01J0000000000000000000STOR",
};

/// A stored value as `pos-cloud` writes one. The adapter treats `doc` as opaque JSON, so the
/// test crate needs none of the cloud's types.
fn doc(token: &str) -> String {
    serde_json::json!({
        "setting_key": STORE_SLOT.setting_key,
        "scope": STORE_SLOT.scope,
        "scope_id": STORE_SLOT.scope_id,
        "value": token,
        "update_time": "2026-10-01T00:00:00Z",
    })
    .to_string()
}

#[test]
fn a_value_is_written_replaced_and_deleted_in_its_slot() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let settings = store.settings();

        settings
            .upsert(TENANT_A, STORE_SLOT, &doc("NO_SHIFT_SELLING_REFUSE"))
            .await
            .expect("write");
        settings
            .upsert(TENANT_A, STORE_SLOT, &doc("NO_SHIFT_SELLING_ALLOW"))
            .await
            .expect("write again");
        let tenant_slot = SettingSlot {
            scope: "SETTING_SCOPE_TENANT",
            scope_id: TENANT_A,
            ..STORE_SLOT
        };
        settings
            .upsert(TENANT_A, tenant_slot, &doc("NO_SHIFT_SELLING_REFUSE"))
            .await
            .expect("write at another scope");

        let listed = settings.fetch(TENANT_A).await.expect("list");
        assert_eq!(
            listed.len(),
            2,
            "the second write to a slot replaced the first"
        );
        assert!(
            listed
                .iter()
                .any(|row| row.contains("NO_SHIFT_SELLING_ALLOW")),
            "the store's slot holds the later value"
        );
        assert!(
            settings.fetch(TENANT_B).await.expect("list").is_empty(),
            "another tenant reads nothing"
        );

        assert!(settings.delete(TENANT_A, STORE_SLOT).await.expect("delete"));
        assert!(
            !settings
                .delete(TENANT_A, STORE_SLOT)
                .await
                .expect("delete again"),
            "deleting what is not there is no error"
        );
        assert_eq!(settings.fetch(TENANT_A).await.expect("list").len(), 1);
    });
}

#[test]
fn the_query_role_sees_only_its_own_tenants_values() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let settings = store.settings();
        settings
            .upsert(TENANT_A, STORE_SLOT, &doc("NO_SHIFT_SELLING_REFUSE"))
            .await
            .expect("write A");
        settings
            .upsert(TENANT_B, STORE_SLOT, &doc("NO_SHIFT_SELLING_ALLOW"))
            .await
            .expect("write B");

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
            .query("SELECT tenant_id FROM setting_values", &[])
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
