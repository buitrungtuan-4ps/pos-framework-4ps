// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Inventory authoring (ADR-0079): ingredients, recipes, and suppliers as
//! per-(tenant, kind, id) jsonb rows, round-tripped and tenant-scoped.

use super::{TENANT_A, TENANT_B, block_on, prepared};

use pos_proto::enums::UnitOfMeasure;
use pos_proto::ids::{IngredientId, MenuItemId, SupplierId};
use pos_proto::inventory::{
    PublishedIngredient, PublishedRecipe, PublishedRecipeLine, PublishedSupplier,
};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use store_postgres::{PostgresInventory, RowUpdate};

fn ingredient(n: u128, name: &str) -> PublishedIngredient {
    PublishedIngredient {
        id: IngredientId::new(Ulid::from_u128(n)),
        name: DisplayName::new(name),
        unit: Open::from_known(UnitOfMeasure::Gram),
    }
}

/// Insert a wire record (already a JSON value) under `(tenant, kind, id)`, asserting the key was
/// free and handing back the version the row starts at — collapses the repeated
/// serialize-then-insert at every call site. Takes a `serde_json::Value` so the helper needs no
/// `serde::Serialize` bound (the test crate depends only on `serde_json`).
async fn put(
    inventory: &PostgresInventory,
    tenant: &str,
    kind: &str,
    id: &str,
    record: serde_json::Value,
) -> String {
    inventory
        .insert(tenant, kind, id, &record.to_string())
        .await
        .expect("insert")
        .expect("the key was free")
}

async fn count(inventory: &PostgresInventory, tenant: &str, kind: &str) -> usize {
    inventory.fetch(tenant, kind).await.expect("fetch").len()
}

/// The single-row read is scoped by tenant *and* kind, and a miss is `None`, not an error.
///
/// The tenant scope is the one that matters: `(tenant_id, kind, entity_id)` is the primary key,
/// so a neighbour's record with the same id is a different row — but only because the query
/// filters on all three. A `fetch_one` that dropped `tenant_id` would still pass the
/// found-and-decoded assertions and would leak across tenants, so this asserts the neighbour is
/// *not* found under this tenant, which is the half that catches it.
///
/// The `kind` scope has the same shape: the three record types share one table, and a recipe id
/// happening to equal an ingredient id must not cross.
#[test]
fn the_single_row_read_is_scoped_by_tenant_and_kind_and_a_miss_is_not_an_error() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let inventory = store.inventory();
        let shared = Ulid::from_u128(10).to_string();

        // The same entity id under two tenants, and under two kinds within one tenant.
        put(
            &inventory,
            TENANT_A,
            "ingredient",
            &shared,
            serde_json::to_value(ingredient(10, "Mine")).expect("json"),
        )
        .await;
        put(
            &inventory,
            TENANT_B,
            "ingredient",
            &shared,
            serde_json::to_value(ingredient(10, "Theirs")).expect("json"),
        )
        .await;
        put(
            &inventory,
            TENANT_A,
            "supplier",
            &shared,
            serde_json::to_value(PublishedSupplier {
                id: SupplierId::new(Ulid::from_u128(10)),
                name: DisplayName::new("Same id, other kind"),
            })
            .expect("json"),
        )
        .await;

        let mine = inventory
            .fetch_one(TENANT_A, "ingredient", &shared)
            .await
            .expect("read")
            .expect("this tenant has it");
        let decoded: PublishedIngredient = serde_json::from_str(&mine.doc_json).expect("decode");
        assert_eq!(
            decoded.name.as_str(),
            "Mine",
            "the tenant filter picks this tenant's row, not the neighbour's",
        );
        assert_eq!(mine.entity_id, shared);
        assert!(
            !mine.version.is_empty(),
            "the single-row read carries the version, so a caller can hand it back to update_at",
        );

        let theirs = inventory
            .fetch_one(TENANT_B, "ingredient", &shared)
            .await
            .expect("read")
            .expect("the neighbour has its own");
        let decoded: PublishedIngredient = serde_json::from_str(&theirs.doc_json).expect("decode");
        assert_eq!(decoded.name.as_str(), "Theirs");

        let other_kind = inventory
            .fetch_one(TENANT_A, "supplier", &shared)
            .await
            .expect("read")
            .expect("the same id under another kind is another row");
        let decoded: PublishedSupplier =
            serde_json::from_str(&other_kind.doc_json).expect("decode");
        assert_eq!(decoded.name.as_str(), "Same id, other kind");

        // Two ways to miss: an id nobody holds, and a kind this tenant has nothing of. Both are
        // `None` — the routes turn that into a 404, and an error would become a 503.
        assert!(
            inventory
                .fetch_one(TENANT_A, "ingredient", &Ulid::from_u128(404).to_string())
                .await
                .expect("a miss reads cleanly")
                .is_none()
        );
        assert!(
            inventory
                .fetch_one(TENANT_A, "recipe", &shared)
                .await
                .expect("a miss reads cleanly")
                .is_none()
        );
    });
}

/// The single-row read and the list agree about a row: same id, same document, same version.
///
/// This is what let the nine `/admin/inventory` handlers stop scanning the list. If the two
/// reads disagreed on any of the three, swapping one for the other would have changed what the
/// routes answer — and the column order is the way they would: both reads name
/// `INVENTORY_COLUMNS` and share one row reader, and this is the assertion that says so.
#[test]
fn the_single_row_read_returns_exactly_what_the_list_holds_for_that_row() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let inventory = store.inventory();
        for n in 1..=3_u128 {
            put(
                &inventory,
                TENANT_A,
                "ingredient",
                &Ulid::from_u128(n).to_string(),
                serde_json::to_value(ingredient(n, &format!("Ingredient {n}"))).expect("json"),
            )
            .await;
        }

        let listed = inventory
            .fetch(TENANT_A, "ingredient")
            .await
            .expect("the list");
        assert_eq!(listed.len(), 3);
        for row in listed {
            let one = inventory
                .fetch_one(TENANT_A, "ingredient", &row.entity_id)
                .await
                .expect("the single-row read")
                .expect("the list said it is there");
            assert_eq!(
                one, row,
                "the two reads must agree on the whole row, columns included",
            );
        }
    });
}

#[test]
fn ingredients_recipes_suppliers_round_trip_scoped_by_tenant_and_kind() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let inventory = store.inventory();
        let ing_id = Ulid::from_u128(10).to_string();

        // A neighbour tenant's ingredient must not leak into TENANT_A's reads.
        put(
            &inventory,
            TENANT_B,
            "ingredient",
            &Ulid::from_u128(99).to_string(),
            serde_json::to_value(ingredient(99, "Neighbour")).expect("json"),
        )
        .await;

        // Create one ingredient, then rename it through the update at the version the insert
        // minted. The insert/update split itself is asserted below, in its own test.
        let at_create = put(
            &inventory,
            TENANT_A,
            "ingredient",
            &ing_id,
            serde_json::to_value(ingredient(10, "Dough")).expect("json"),
        )
        .await;
        let renamed = serde_json::to_value(ingredient(10, "Dough (renamed)"))
            .expect("json")
            .to_string();
        assert!(matches!(
            inventory
                .update_at(TENANT_A, "ingredient", &ing_id, &renamed, &at_create)
                .await
                .expect("the rename"),
            RowUpdate::Updated(_)
        ));

        // A recipe and a supplier share the table under other kinds.
        let recipe = PublishedRecipe {
            item: MenuItemId::new(Ulid::from_u128(20)),
            lines: vec![PublishedRecipeLine {
                ingredient: IngredientId::new(Ulid::from_u128(10)),
                per_unit: Quantity::from_milli(100_000),
            }],
            auto_86_threshold: 3,
        };
        put(
            &inventory,
            TENANT_A,
            "recipe",
            &recipe.item.to_string(),
            serde_json::to_value(&recipe).expect("json"),
        )
        .await;
        let supplier = PublishedSupplier {
            id: SupplierId::new(Ulid::from_u128(30)),
            name: DisplayName::new("Anchor Dairy"),
        };
        put(
            &inventory,
            TENANT_A,
            "supplier",
            &supplier.id.to_string(),
            serde_json::to_value(&supplier).expect("json"),
        )
        .await;

        // The kind filter keeps the three apart, and the rename replaced in place.
        let ingredients = inventory.fetch(TENANT_A, "ingredient").await.expect("ings");
        assert_eq!(ingredients.len(), 1, "an update adds no row");
        let row = ingredients.first().expect("one");
        let decoded: PublishedIngredient = serde_json::from_str(&row.doc_json).expect("decode");
        assert_eq!(decoded.name, DisplayName::new("Dough (renamed)"));
        assert_ne!(
            row.version, at_create,
            "and the read carries the version the update left the row at, not the insert's"
        );
        assert_eq!(count(&inventory, TENANT_A, "recipe").await, 1);
        assert_eq!(count(&inventory, TENANT_A, "supplier").await, 1);

        // Tenant isolation: TENANT_B sees only its own ingredient, none of TENANT_A's rows.
        assert_eq!(count(&inventory, TENANT_B, "ingredient").await, 1);
        assert_eq!(count(&inventory, TENANT_B, "recipe").await, 0);

        // Delete is scoped by (kind, id) and idempotent; other kinds are untouched.
        inventory
            .delete(TENANT_A, "ingredient", &ing_id)
            .await
            .expect("delete");
        inventory
            .delete(TENANT_A, "ingredient", &ing_id)
            .await
            .expect("no-op");
        assert_eq!(count(&inventory, TENANT_A, "ingredient").await, 0);
        assert_eq!(count(&inventory, TENANT_A, "recipe").await, 1);
        assert_eq!(count(&inventory, TENANT_A, "supplier").await, 1);
    });
}

/// A record's key is `(tenant, kind, entity_id)` and it comes from the caller, so a second
/// insert at a taken key writes nothing and returns no version — where the `upsert` this
/// replaced replaced the document (ADR-0095). A stale update is refused, an absent one is
/// `NotFound`, and the two answers are distinct because the caller's next move differs.
#[test]
fn an_insert_writes_nothing_when_the_key_is_taken_and_an_update_needs_the_version_read() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let inventory = store.inventory();
        let ing_id = Ulid::from_u128(10).to_string();
        let renamed = serde_json::to_value(ingredient(10, "Dough (renamed)"))
            .expect("json")
            .to_string();

        let at_create = put(
            &inventory,
            TENANT_A,
            "ingredient",
            &ing_id,
            serde_json::to_value(ingredient(10, "Dough")).expect("json"),
        )
        .await;
        assert!(
            inventory
                .insert(TENANT_A, "ingredient", &ing_id, &renamed)
                .await
                .expect("the conflict must not raise")
                .is_none(),
            "the (kind, id) is taken"
        );
        let row = inventory
            .fetch(TENANT_A, "ingredient")
            .await
            .expect("fetch")
            .pop()
            .expect("one");
        let decoded: PublishedIngredient = serde_json::from_str(&row.doc_json).expect("decode");
        assert_eq!(
            decoded.name,
            DisplayName::new("Dough"),
            "and does not rename the record it refused to overwrite"
        );
        assert_eq!(row.version, at_create);

        assert!(matches!(
            inventory
                .update_at(TENANT_A, "ingredient", &ing_id, &renamed, &at_create)
                .await
                .expect("the rename"),
            RowUpdate::Updated(_)
        ));
        assert_eq!(
            inventory
                .update_at(TENANT_A, "ingredient", &ing_id, &renamed, &at_create)
                .await
                .expect("the comparison must not raise"),
            RowUpdate::VersionMismatch,
            "replaying a spent version is the lost update"
        );
        assert_eq!(
            inventory
                .update_at(TENANT_A, "ingredient", "none", &renamed, &at_create)
                .await
                .expect("the comparison must not raise"),
            RowUpdate::NotFound
        );
    });
}
