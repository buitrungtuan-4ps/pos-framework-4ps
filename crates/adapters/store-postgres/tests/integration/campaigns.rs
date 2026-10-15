// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Campaign authoring: jsonb insert/list/get/update-at/delete, id ordering, tenant isolation
//! (ADR-0077, ADR-0095).

use store_postgres::RowUpdate;

use super::{TENANT_A, TENANT_B, block_on, prepared};

fn doc(name: &str) -> String {
    format!("{{\"name\":\"{name}\"}}")
}

/// Per-campaign insert/get/delete over the jsonb column: rows read back id-ordered and carrying
/// the version they are at, an insert at a taken id writes nothing, a tenant cannot read
/// another's campaign by id, and a neighbour tenant's rows are never touched.
///
/// The insert/update split is [ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md):
/// what this asserted before was the `ON CONFLICT DO UPDATE` that made "add a campaign" and
/// "replace a campaign" the same statement, so a second admin creating at an id another had
/// just used silently replaced their document.
#[test]
fn an_insert_writes_nothing_when_the_id_is_taken_and_reads_stay_tenant_scoped() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let campaigns = store.campaigns();

        // A neighbour's campaign must survive our writes.
        campaigns
            .insert(TENANT_B, "camp-z", &doc("Neighbour"))
            .await
            .expect("neighbour")
            .expect("the neighbour's id was free");

        // Insert out of id order to prove the read sorts.
        campaigns
            .insert(TENANT_A, "camp-2", &doc("Dinner"))
            .await
            .expect("create 2")
            .expect("free");
        let at_create = campaigns
            .insert(TENANT_A, "camp-1", &doc("Lunch"))
            .await
            .expect("create 1")
            .expect("free");
        let listed = campaigns.fetch(TENANT_A).await.expect("fetch ours");
        assert_eq!(listed.len(), 2);
        let first = listed.first().expect("first row");
        assert_eq!(first.campaign_id, "camp-1", "rows read back id-ordered");
        assert_eq!(
            first.version, at_create,
            "the read carries the version the insert minted, which is the only way a caller \
             that did not perform the insert can obtain it"
        );

        // An insert at a taken id writes nothing and says so by returning no version — one round
        // trip, so there is no window between a check and the write.
        assert!(
            campaigns
                .insert(TENANT_A, "camp-1", &doc("Lunch (renamed)"))
                .await
                .expect("the conflict must not raise")
                .is_none(),
            "the id is taken"
        );
        let unchanged = campaigns
            .fetch_one(TENANT_A, "camp-1")
            .await
            .expect("fetch one")
            .expect("present");
        assert!(
            unchanged.campaign_json.contains("Lunch"),
            "and the document it refused to overwrite is untouched"
        );
        assert!(!unchanged.campaign_json.contains("renamed"));

        // A tenant cannot read another tenant's campaign by id.
        assert!(
            campaigns
                .fetch_one(TENANT_A, "camp-z")
                .await
                .expect("cross-tenant read")
                .is_none(),
            "camp-z belongs to the neighbour, not us"
        );

        campaigns.delete(TENANT_A, "camp-1").await.expect("delete");
        assert_eq!(
            campaigns
                .fetch(TENANT_A)
                .await
                .expect("fetch after delete")
                .len(),
            1
        );
        // The id is free again, so an insert at it succeeds.
        assert!(
            campaigns
                .insert(TENANT_A, "camp-1", &doc("Lunch again"))
                .await
                .expect("insert after the delete")
                .is_some()
        );
        assert_eq!(
            campaigns
                .fetch(TENANT_B)
                .await
                .expect("fetch neighbour")
                .len(),
            1,
            "the neighbour is untouched throughout"
        );
    });
}

/// A rename applies at the version the read carried, refuses a spent one, and answers `NotFound`
/// for an id that is not there — three answers, because the caller's next move differs for each.
#[test]
fn an_update_applies_only_at_the_version_the_read_carried() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let campaigns = store.campaigns();
        let at_create = campaigns
            .insert(TENANT_A, "camp-1", &doc("Lunch"))
            .await
            .expect("create")
            .expect("the id was free");

        let at_update = match campaigns
            .update_at(TENANT_A, "camp-1", &doc("Lunch (renamed)"), &at_create)
            .await
            .expect("the update")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the update to apply, got {other:?}")
            }
        };
        assert_ne!(at_update, at_create, "the version moves on every write");
        let one = campaigns
            .fetch_one(TENANT_A, "camp-1")
            .await
            .expect("fetch one")
            .expect("present");
        assert!(
            one.campaign_json.contains("Lunch (renamed)"),
            "the replaced document is what reads back"
        );
        assert_eq!(one.version, at_update);
        assert_eq!(
            campaigns.fetch(TENANT_A).await.expect("fetch").len(),
            1,
            "an update does not add a row"
        );

        assert_eq!(
            campaigns
                .update_at(TENANT_A, "camp-1", &doc("Lunch (again)"), &at_create)
                .await
                .expect("the comparison must not raise"),
            RowUpdate::VersionMismatch,
            "replaying a spent version is the lost update"
        );
        assert_eq!(
            campaigns
                .update_at(TENANT_A, "camp-none", &doc("Ghost"), &at_update)
                .await
                .expect("the comparison must not raise"),
            RowUpdate::NotFound
        );
    });
}
