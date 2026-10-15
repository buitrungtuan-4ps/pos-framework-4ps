// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The per-(tax class × channel) tax rate table: wholesale replace, tenant-scoped (ADR-0074, M4).

use store_postgres::{RowUpdate, TaxRateRow};

use super::{TENANT_A, TENANT_B, block_on, prepared};

fn row(channel: &str, rate_bps: i32) -> TaxRateRow {
    TaxRateRow {
        tax_class_id: "class-standard".to_owned(),
        sales_channel: channel.to_owned(),
        rate_bps,
        // No breakdown: one rate, printed as one line, which is most of the world.
        components_json: "[]".to_owned(),
    }
}

/// The same row with the named parts an Indian invoice must print separately (ADR-0104).
fn row_with_components(channel: &str, rate_bps: i32, components: &str) -> TaxRateRow {
    TaxRateRow {
        components_json: components.to_owned(),
        ..row(channel, rate_bps)
    }
}

/// The version an applied save left the table at, or `None` if the store refused it — so a call
/// site can `.expect` inside its own test, where the lint config allows it.
fn applied(outcome: RowUpdate) -> Option<String> {
    match outcome {
        RowUpdate::Updated(version) => Some(version),
        RowUpdate::VersionMismatch | RowUpdate::NotFound => None,
    }
}

/// A save replaces the tenant's whole table (not append), reads back class-then-channel ordered,
/// carries the version it was saved at, and never touches a neighbour tenant's rows.
#[test]
fn replaces_wholesale_under_its_version_and_stays_tenant_scoped() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let rates = store.tax_rates();

        assert!(
            rates
                .fetch(TENANT_A)
                .await
                .expect("fetch before any save")
                .1
                .is_none(),
            "a tenant that has never saved rates has no version"
        );

        let neighbour = vec![row("SALES_CHANNEL_DINE_IN", 500)];
        applied(
            rates
                .replace(TENANT_B, &neighbour, None)
                .await
                .expect("set neighbour"),
        )
        .expect("the save applies");

        let first = vec![
            row("SALES_CHANNEL_DINE_IN", 1000),
            row("SALES_CHANNEL_TAKEAWAY", 800),
        ];
        let created = applied(
            rates
                .replace(TENANT_A, &first, None)
                .await
                .expect("set ours"),
        )
        .expect("the save applies");
        let (listed, version) = rates.fetch(TENANT_A).await.expect("fetch ours");
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed.first().expect("first row").sales_channel,
            "SALES_CHANNEL_DINE_IN",
            "rows read back class-then-channel ordered"
        );
        let version = version.expect("a saved table has a version");
        assert_eq!(
            version, created,
            "the version a save returns is the one the next fetch reads"
        );

        // A second save at that version replaces rather than appends, and moves the version.
        let second = vec![row("SALES_CHANNEL_DINE_IN", 1000)];
        let moved = applied(
            rates
                .replace(TENANT_A, &second, Some(&version))
                .await
                .expect("replace ours"),
        )
        .expect("the save applies");
        let (replaced, _) = rates.fetch(TENANT_A).await.expect("fetch replaced");
        assert_eq!(replaced, second, "the whole table is replaced");
        assert_ne!(
            moved, version,
            "xmin on the version row must move, or the next save would be unguarded"
        );

        // The neighbour is untouched — the version is per tenant, like the rows it guards.
        let (neighbour_after, _) = rates.fetch(TENANT_B).await.expect("fetch neighbour");
        assert_eq!(neighbour_after, neighbour);
    });
}

/// A rate's named parts survive the round trip through the `components` column (migration 0050,
/// [ADR-0104](../../../../docs/adr/0104-multi-component-and-inclusive-tax.md)).
///
/// India is the case: an intra-state invoice must print CGST and SGST separately, because the
/// halves go to different governments. A row saved with no breakdown reads back as `[]` — which
/// is also what every row written before this column existed reads back as, and the reason the
/// migration defaults rather than backfills.
#[test]
fn a_rates_named_parts_survive_the_round_trip() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let rates = store.tax_rates();

        let indian = vec![
            row_with_components(
                "SALES_CHANNEL_DINE_IN",
                500,
                r#"[{"name":"CGST","rate":250},{"name":"SGST","rate":250}]"#,
            ),
            row("SALES_CHANNEL_TAKEAWAY", 500),
        ];
        applied(
            rates
                .replace(TENANT_A, &indian, None)
                .await
                .expect("save the Indian table"),
        )
        .expect("the save applies");

        let (listed, _) = rates.fetch(TENANT_A).await.expect("fetch");
        let dine_in = listed
            .iter()
            .find(|stored| stored.sales_channel == "SALES_CHANNEL_DINE_IN")
            .expect("the dine-in row");
        assert!(
            dine_in.components_json.contains("CGST") && dine_in.components_json.contains("SGST"),
            "the breakdown reads back: {}",
            dine_in.components_json
        );

        let takeaway = listed
            .iter()
            .find(|stored| stored.sales_channel == "SALES_CHANNEL_TAKEAWAY")
            .expect("the takeaway row");
        assert_eq!(
            takeaway.components_json, "[]",
            "no breakdown is an empty list, not null — which is what a row written before this \
             column existed reads back as too"
        );
    });
}

/// The four ways a save is refused, and the proof that none of them touched the rate rows.
///
/// This is the case the `xmin`-on-the-row scheme cannot cover, and the reason migration 0039
/// exists: a replace deletes and reinserts every rate row, so no rate row's version survives one
/// to be compared against. The version row is what does.
#[test]
fn a_stale_garbled_or_already_taken_save_is_refused_and_writes_nothing() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let rates = store.tax_rates();
        let kept = vec![row("SALES_CHANNEL_DINE_IN", 1000)];
        let clobber = vec![row("SALES_CHANNEL_DINE_IN", 9900)];

        // Naming a version for a tenant that has never saved is an absence, not a conflict.
        assert_eq!(
            rates
                .replace(TENANT_A, &clobber, Some("1"))
                .await
                .expect("the probe"),
            RowUpdate::NotFound
        );

        let stale = applied(
            rates
                .replace(TENANT_A, &kept, None)
                .await
                .expect("the first save"),
        )
        .expect("the save applies");
        applied(
            rates
                .replace(TENANT_A, &kept, Some(&stale))
                .await
                .expect("a second save"),
        )
        .expect("the save applies");

        // Replaying a version the table has moved past is the lost update this refuses.
        assert_eq!(
            rates
                .replace(TENANT_A, &clobber, Some(&stale))
                .await
                .expect("the stale save"),
            RowUpdate::VersionMismatch
        );

        // A garbled tag is a mismatch too, not a database error — the comparison is on
        // `xmin::text`, because casting caller text to `xid` would raise instead of refusing.
        assert_eq!(
            rates
                .replace(TENANT_A, &clobber, Some("not-a-transaction-id"))
                .await
                .expect("the comparison must not raise"),
            RowUpdate::VersionMismatch
        );

        // Claiming "nothing saved yet" about a table that has been saved: refused, not upserted.
        assert_eq!(
            rates
                .replace(TENANT_A, &clobber, None)
                .await
                .expect("the create path"),
            RowUpdate::VersionMismatch
        );

        let (survived, _) = rates
            .fetch(TENANT_A)
            .await
            .expect("fetch after the refusals");
        assert_eq!(
            survived, kept,
            "no refused save deleted or reinserted a rate row"
        );
    });
}
