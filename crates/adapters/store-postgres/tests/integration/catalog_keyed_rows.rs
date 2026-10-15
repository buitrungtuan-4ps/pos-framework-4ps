// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Catalog placements and layout buttons: the two caller-keyed rows, on the insert/update-at split
//! (ADR-0095).

use store_postgres::{PostgresCatalog, RowUpdate};

use super::{TENANT_A, TENANT_B, block_on, prepared};

const MENU: &str = "menu-1";
const ITEM: &str = "item-1";
const CHANNEL: &str = "SALES_CHANNEL_DINE_IN";
const CATEGORY: &str = "cat-1";

fn prices(amount: i64) -> String {
    format!(
        "[{{\"sales_channel\":\"SALES_CHANNEL_DINE_IN\",\
           \"unit_price\":{{\"currency_code\":\"VND\",\"amount_minor\":{amount}}}}}]"
    )
}

/// The version an applied write left the row at, or `None` when it was refused. `Option` rather
/// than a panic because this is a plain helper, not a `#[test]` fn — the caller unwraps it, where
/// `clippy.toml`'s test allowances apply.
fn applied(outcome: RowUpdate) -> Option<String> {
    match outcome {
        RowUpdate::Updated(version) => Some(version),
        RowUpdate::VersionMismatch | RowUpdate::NotFound => None,
    }
}

/// Puts an item on a menu at `amount`, answering the version the row starts at (or `None` when
/// the `(menu, item)` pair is already taken).
async fn place(
    catalog: &PostgresCatalog,
    tenant: &str,
    menu: &str,
    item: &str,
    amount: i64,
) -> Option<String> {
    catalog
        .insert_placement(tenant, menu, item, None, &prices(amount), true)
        .await
        .expect("the insert must not raise")
}

/// Reprices that placement, only at `expected`.
async fn reprice(
    catalog: &PostgresCatalog,
    tenant: &str,
    item: &str,
    amount: i64,
    expected: &str,
) -> RowUpdate {
    catalog
        .update_placement_at(tenant, MENU, item, None, &prices(amount), true, expected)
        .await
        .expect("the comparison must not raise")
}

/// Places a button in the `(channel, item)` slot, answering the version it starts at (or `None`
/// when the slot is taken).
async fn press(catalog: &PostgresCatalog, tenant: &str, item: &str, label: &str) -> Option<String> {
    catalog
        .insert_layout_button(tenant, CHANNEL, CATEGORY, None, item, label, None, None, 0)
        .await
        .expect("the insert must not raise")
}

/// Relabels and re-sorts that button, only at `expected`.
async fn relabel(
    catalog: &PostgresCatalog,
    tenant: &str,
    item: &str,
    label: &str,
    sort: i32,
    expected: &str,
) -> RowUpdate {
    catalog
        .update_layout_button_at(
            tenant, CHANNEL, CATEGORY, None, item, label, None, None, sort, expected,
        )
        .await
        .expect("the comparison must not raise")
}

/// A placement's identity is the caller's `(menu, item)` pair, so a second insert at the same
/// pair writes nothing rather than repricing what is already on the menu. The per-channel prices
/// are the price-change journal (ADR-0069), so that overwrite was recorded as a set with no
/// `before` to compare against — which is why ADR-0095 split this seam.
#[test]
fn a_placement_insert_writes_nothing_when_the_pair_is_already_on_the_menu() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();

        // A neighbour's placement on its own menu must survive our writes.
        place(&catalog, TENANT_B, "menu-z", ITEM, 99_000)
            .await
            .expect("free");
        let at_create = place(&catalog, TENANT_A, MENU, ITEM, 150_000)
            .await
            .expect("the pair was free");

        assert!(
            place(&catalog, TENANT_A, MENU, ITEM, 160_000)
                .await
                .is_none(),
            "the pair is taken"
        );
        let rows = catalog
            .fetch_placements(TENANT_A, MENU)
            .await
            .expect("fetch");
        assert_eq!(rows.len(), 1, "a refused insert adds no row");
        let row = rows.first().expect("one");
        assert!(
            row.prices_json.contains("150000"),
            "and does not reprice the placement it refused to overwrite"
        );
        assert_eq!(
            row.version, at_create,
            "the read carries the version the insert minted, which is the only way a caller \
             that did not perform the insert can obtain it"
        );

        // Removing frees the pair again, and the neighbour is untouched throughout.
        assert!(
            catalog
                .delete_placement(TENANT_A, MENU, ITEM)
                .await
                .expect("remove")
        );
        assert!(
            place(&catalog, TENANT_A, MENU, ITEM, 150_000)
                .await
                .is_some(),
            "the pair is free after the remove"
        );
        assert_eq!(
            catalog
                .fetch_placements(TENANT_B, "menu-z")
                .await
                .expect("fetch neighbour")
                .len(),
            1
        );
    });
}

/// A reprice applies at the version the read carried, refuses a spent one, and answers `NotFound`
/// for a pair that is not on the menu — the probe on that failure path is what separates a `412`
/// from a `404`, and it has to name the same table the update did (the defect #152 fixed).
#[test]
fn a_placement_reprice_applies_only_at_the_version_the_read_carried() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();
        let at_create = place(&catalog, TENANT_A, MENU, ITEM, 150_000)
            .await
            .expect("the pair was free");

        let at_update = applied(reprice(&catalog, TENANT_A, ITEM, 160_000, &at_create).await)
            .expect("the reprice applies at the version the read carried");
        assert_ne!(at_update, at_create, "the version moves on every write");
        let rows = catalog
            .fetch_placements(TENANT_A, MENU)
            .await
            .expect("fetch");
        assert_eq!(rows.len(), 1, "an update does not add a row");
        assert!(rows.first().expect("one").prices_json.contains("160000"));

        assert_eq!(
            reprice(&catalog, TENANT_A, ITEM, 170_000, &at_create).await,
            RowUpdate::VersionMismatch,
            "replaying a spent version is the lost update"
        );
        assert_eq!(
            reprice(&catalog, TENANT_A, "item-none", 170_000, &at_update).await,
            RowUpdate::NotFound,
            "and an absent pair is a different answer, because the caller's next move differs"
        );
    });
}

/// A layout button's identity is the caller's `(channel, item)` slot, and it behaves the same way.
#[test]
fn a_layout_button_insert_writes_nothing_when_the_slot_is_taken() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();

        press(&catalog, TENANT_B, ITEM, "Neighbour")
            .await
            .expect("free");
        let at_create = press(&catalog, TENANT_A, ITEM, "Margherita")
            .await
            .expect("the slot was free");

        assert!(
            press(&catalog, TENANT_A, ITEM, "Margherita (classic)")
                .await
                .is_none(),
            "the slot is taken"
        );
        let rows = catalog.fetch_layout_buttons(TENANT_A).await.expect("fetch");
        assert_eq!(rows.len(), 1, "a refused insert adds no row");
        let row = rows.first().expect("one");
        assert_eq!(
            row.label, "Margherita",
            "and does not relabel the button it refused to overwrite"
        );
        assert_eq!(row.version, at_create);

        assert!(
            catalog
                .delete_layout_button(TENANT_A, CHANNEL, ITEM)
                .await
                .expect("remove")
        );
        assert_eq!(
            catalog
                .fetch_layout_buttons(TENANT_B)
                .await
                .expect("fetch neighbour")
                .len(),
            1,
            "the neighbour's button is untouched throughout"
        );
    });
}

/// And a relabel is conditional the same way, with the same two distinct refusals.
#[test]
fn a_layout_button_relabel_applies_only_at_the_version_the_read_carried() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();
        let at_create = press(&catalog, TENANT_A, ITEM, "Margherita")
            .await
            .expect("the slot was free");

        let at_update = applied(
            relabel(
                &catalog,
                TENANT_A,
                ITEM,
                "Margherita (classic)",
                3,
                &at_create,
            )
            .await,
        )
        .expect("the relabel applies at the version the read carried");
        assert_ne!(at_update, at_create);
        let rows = catalog.fetch_layout_buttons(TENANT_A).await.expect("fetch");
        assert_eq!(rows.len(), 1);
        let row = rows.first().expect("one");
        assert_eq!(row.label, "Margherita (classic)");
        assert_eq!(row.sort, 3);

        assert_eq!(
            relabel(
                &catalog,
                TENANT_A,
                ITEM,
                "Margherita (again)",
                0,
                &at_create
            )
            .await,
            RowUpdate::VersionMismatch
        );
        assert_eq!(
            relabel(&catalog, TENANT_A, "item-none", "Ghost", 0, &at_update).await,
            RowUpdate::NotFound
        );
    });
}
