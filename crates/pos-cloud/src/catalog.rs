// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud catalog authoring seam ([ADR-0066](../../../docs/adr/0066-cloud-catalog.md)).
//!
//! The rich, normalized *source of truth* an operator edits — items, menus that inherit, and the
//! per-channel prices an item carries in a menu. It is deliberately distinct from two things it feeds:
//! the [`config_tree`](crate::config_tree), which carries only the *compiled* output a store pulls,
//! and the [`registry`](crate::registry), which is identity and naming. The compiler
//! (a later slice) resolves this model per `(store × channel)` into the flat
//! [`pos_proto::MenuBook`] the edge reprices from; nothing here crosses the wire to a store.
//!
//! This slice lands the item master, menus (with inheritance), and menu placements — the inputs a
//! `MenuBook` is compiled from. Modifier groups, the display taxonomy and layouts are later slices of
//! the same seam. Prices are a **T2** asset (a pricing model): authored and compiled in the cloud,
//! never written to a log or an event.
//!
//! The seam is a trait so it runs against an in-memory fake in tests and a `store-postgres` table in
//! the cloud, exactly as [`RegistryStore`](crate::registry::RegistryStore) does — the same
//! create/list/update shape, tenant-scoped and RLS-isolated, entities archived rather than
//! hard-deleted so a compiled menu that referenced one still resolves.

use core::fmt;
use core::future::Future;

use serde::Serialize;

use pos_proto::enums::SalesChannel;
use pos_proto::ids::{MenuItemId, TaxClassId, TenantId};
use pos_proto::money::Money;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::registry::EntityStatus;

/// A menu's identifier — a ULID minted at creation. A menu is an authoring concept that never
/// crosses the wire (the compiled [`pos_proto::MenuBook`] has no menu id), so its id is defined here
/// beside the seam rather than in [`pos_proto::ids`], the same way a `BrandId` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct MenuId(Ulid);

impl MenuId {
    /// Wraps a ULID as a menu id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for MenuId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An item in the catalog — the product master.
///
/// Its id is a [`MenuItemId`], the same identifier the compiled [`pos_proto::MenuEntry`] names and an
/// inbound order references: the item authored here is the item priced downstream. This slice carries
/// the fields a `MenuEntry` needs (name, tax class); its operational category, recipe/BOM link and
/// the rest arrive with later slices.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogItem {
    /// The item's id — shared with the compiled menu entry and any inbound order that names it.
    pub menu_item_id: MenuItemId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human name (the default caption, before a layout overrides it per channel).
    pub name: String,
    /// The tax class, which the store's channel-keyed rate table turns into a rate at reprice time.
    pub tax_class_id: TaxClassId,
    /// Active or archived.
    pub status: EntityStatus,
}

/// A menu — a named set of placements that may **inherit** from a parent menu.
///
/// Inheritance is how a brand menu overrides a tenant standard and a store special overrides the
/// brand: the compiler folds a parent's placements under a child's, most-specific-wins. A cycle in
/// `parent_menu_id` is a configuration error the compiler rejects; the seam only stores the edge.
#[derive(Debug, Clone, Serialize)]
pub struct Menu {
    /// The menu id.
    pub menu_id: MenuId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human name.
    pub name: String,
    /// The menu this one inherits from, or `None` for a root menu.
    pub parent_menu_id: Option<MenuId>,
    /// Active or archived.
    pub status: EntityStatus,
}

/// One channel's price for a [`MenuPlacement`].
///
/// This is where dine-in ≠ takeaway ≠ delivery pricing is authored; the compiler emits one channel's
/// prices into that channel's [`pos_proto::MenuCatalog`] within the store's `MenuBook`.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelPrice {
    /// The channel this price applies on. `Open`, the same shape [`pos_proto::locale::TaxRateRow`]
    /// uses, so authoring stored under a channel a later build learns still round-trips.
    pub sales_channel: Open<SalesChannel>,
    /// The price per unit on that channel. Integer [`Money`].
    pub unit_price: Money,
}

/// An item placed in a menu, with its per-channel prices — the row a compiled `MenuEntry` is made
/// from. Its identity is the pair `(menu_id, menu_item_id)`: an item appears in a menu at most once.
#[derive(Debug, Clone, Serialize)]
pub struct MenuPlacement {
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The menu this placement belongs to.
    pub menu_id: MenuId,
    /// The item placed.
    pub menu_item_id: MenuItemId,
    /// The item's price per channel in this menu. A channel with no row here falls to the menu's
    /// parent (inheritance) or, failing that, is not sold on that channel.
    pub prices: Vec<ChannelPrice>,
    /// Whether the item is for sale in this menu right now — the operator's *published* floor, which
    /// the edge's live-stock auto-86 (`pos-core` §8) can only push further down, never re-raise.
    pub available: bool,
}

/// Persists and reads the catalog authoring model.
///
/// Every entity has the same shape as the registry's: `create` (a freshly-minted record), `list`
/// (scoped to its tenant, and to its menu for a placement), and `update`/`set` (returning whether a
/// row changed, so a handler can answer `404`). A placement is upserted by its `(menu_id,
/// menu_item_id)` pair and removed by it. All reads and writes are tenant-scoped; the `store-postgres`
/// impl (a later slice) is RLS-isolated by tenant, like every other cloud table.
pub trait CatalogStore {
    /// Inserts an item.
    fn create_item(
        &self,
        item: &CatalogItem,
    ) -> impl Future<Output = Result<(), CatalogStoreError>> + Send;

    /// Lists a tenant's items.
    fn list_items(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<CatalogItem>, CatalogStoreError>> + Send;

    /// Renames an item, sets its tax class, and/or sets its status, within its tenant. Returns
    /// whether a row was found and changed.
    fn update_item(
        &self,
        item: &CatalogItem,
    ) -> impl Future<Output = Result<bool, CatalogStoreError>> + Send;

    /// Inserts a menu.
    fn create_menu(
        &self,
        menu: &Menu,
    ) -> impl Future<Output = Result<(), CatalogStoreError>> + Send;

    /// Lists a tenant's menus.
    fn list_menus(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Menu>, CatalogStoreError>> + Send;

    /// Renames a menu, (re)sets its parent, and/or sets its status, within its tenant. Returns
    /// whether a row changed.
    fn update_menu(
        &self,
        menu: &Menu,
    ) -> impl Future<Output = Result<bool, CatalogStoreError>> + Send;

    /// Inserts or replaces a placement, by its `(menu_id, menu_item_id)` pair.
    fn set_placement(
        &self,
        placement: &MenuPlacement,
    ) -> impl Future<Output = Result<(), CatalogStoreError>> + Send;

    /// Lists a menu's placements, within its tenant.
    fn list_placements(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
    ) -> impl Future<Output = Result<Vec<MenuPlacement>, CatalogStoreError>> + Send;

    /// Removes an item from a menu. Returns whether a row was found and removed.
    fn remove_placement(
        &self,
        tenant_id: TenantId,
        menu_id: MenuId,
        menu_item_id: MenuItemId,
    ) -> impl Future<Output = Result<bool, CatalogStoreError>> + Send;
}

/// A failure of the catalog store itself — the database is unreachable.
#[derive(Debug, thiserror::Error)]
#[error("the catalog store failed: {0}")]
pub struct CatalogStoreError(String);

impl CatalogStoreError {
    /// Wraps a message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::enums::SalesChannel;
    use pos_proto::ids::{MenuItemId, TaxClassId, TenantId};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::ulid::Ulid;
    use pos_proto::wire_enum::Open;

    use super::{
        CatalogItem, CatalogStore, CatalogStoreError, ChannelPrice, Menu, MenuId, MenuPlacement,
    };
    use crate::registry::EntityStatus;

    /// An in-memory `CatalogStore` for the domain tests here and, later, the compiler's. Tenant-scoped
    /// like the real thing; every list filters by tenant so a test can prove isolation.
    #[derive(Default)]
    struct FakeCatalog {
        items: Mutex<Vec<CatalogItem>>,
        menus: Mutex<Vec<Menu>>,
        placements: Mutex<Vec<MenuPlacement>>,
    }

    impl CatalogStore for FakeCatalog {
        async fn create_item(&self, item: &CatalogItem) -> Result<(), CatalogStoreError> {
            self.items.lock().expect("lock").push(item.clone());
            Ok(())
        }

        async fn list_items(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<CatalogItem>, CatalogStoreError> {
            Ok(self
                .items
                .lock()
                .expect("lock")
                .iter()
                .filter(|item| item.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn update_item(&self, item: &CatalogItem) -> Result<bool, CatalogStoreError> {
            let mut items = self.items.lock().expect("lock");
            let Some(row) = items.iter_mut().find(|row| {
                row.menu_item_id == item.menu_item_id && row.tenant_id == item.tenant_id
            }) else {
                return Ok(false);
            };
            row.name.clone_from(&item.name);
            row.tax_class_id = item.tax_class_id;
            row.status = item.status;
            Ok(true)
        }

        async fn create_menu(&self, menu: &Menu) -> Result<(), CatalogStoreError> {
            self.menus.lock().expect("lock").push(menu.clone());
            Ok(())
        }

        async fn list_menus(&self, tenant_id: TenantId) -> Result<Vec<Menu>, CatalogStoreError> {
            Ok(self
                .menus
                .lock()
                .expect("lock")
                .iter()
                .filter(|menu| menu.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn update_menu(&self, menu: &Menu) -> Result<bool, CatalogStoreError> {
            let mut menus = self.menus.lock().expect("lock");
            let Some(row) = menus
                .iter_mut()
                .find(|row| row.menu_id == menu.menu_id && row.tenant_id == menu.tenant_id)
            else {
                return Ok(false);
            };
            row.name.clone_from(&menu.name);
            row.parent_menu_id = menu.parent_menu_id;
            row.status = menu.status;
            Ok(true)
        }

        async fn set_placement(&self, placement: &MenuPlacement) -> Result<(), CatalogStoreError> {
            let mut placements = self.placements.lock().expect("lock");
            if let Some(row) = placements.iter_mut().find(|row| {
                row.tenant_id == placement.tenant_id
                    && row.menu_id == placement.menu_id
                    && row.menu_item_id == placement.menu_item_id
            }) {
                *row = placement.clone();
            } else {
                placements.push(placement.clone());
            }
            Ok(())
        }

        async fn list_placements(
            &self,
            tenant_id: TenantId,
            menu_id: MenuId,
        ) -> Result<Vec<MenuPlacement>, CatalogStoreError> {
            Ok(self
                .placements
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.tenant_id == tenant_id && row.menu_id == menu_id)
                .cloned()
                .collect())
        }

        async fn remove_placement(
            &self,
            tenant_id: TenantId,
            menu_id: MenuId,
            menu_item_id: MenuItemId,
        ) -> Result<bool, CatalogStoreError> {
            let mut placements = self.placements.lock().expect("lock");
            let before = placements.len();
            placements.retain(|row| {
                !(row.tenant_id == tenant_id
                    && row.menu_id == menu_id
                    && row.menu_item_id == menu_item_id)
            });
            Ok(placements.len() != before)
        }
    }

    fn tenant(n: u128) -> TenantId {
        TenantId::new(Ulid::from_u128(n))
    }

    fn item_id(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn menu_id(n: u128) -> MenuId {
        MenuId::new(Ulid::from_u128(n))
    }

    fn tax_class(n: u128) -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(n))
    }

    fn vnd(minor: i64) -> Money {
        Money::new(CurrencyCode::VND, minor)
    }

    fn item(tenant_n: u128, id_n: u128, name: &str) -> CatalogItem {
        CatalogItem {
            menu_item_id: item_id(id_n),
            tenant_id: tenant(tenant_n),
            name: name.to_owned(),
            tax_class_id: tax_class(1),
            status: EntityStatus::Active,
        }
    }

    #[tokio::test]
    async fn items_are_created_listed_and_updated_within_a_tenant() {
        let store = FakeCatalog::default();
        store
            .create_item(&item(1, 500, "Margherita"))
            .await
            .expect("create");
        store
            .create_item(&item(1, 501, "Marinara"))
            .await
            .expect("create");
        store
            .create_item(&item(2, 999, "Someone else's"))
            .await
            .expect("create");

        let mine = store.list_items(tenant(1)).await.expect("list");
        assert_eq!(mine.len(), 2, "another tenant's item is not listed");

        let renamed = CatalogItem {
            name: "Margherita (classic)".to_owned(),
            ..item(1, 500, "unused")
        };
        assert!(store.update_item(&renamed).await.expect("update"));
        let found = store
            .list_items(tenant(1))
            .await
            .expect("list")
            .into_iter()
            .find(|row| row.menu_item_id == item_id(500))
            .expect("present");
        assert_eq!(found.name, "Margherita (classic)");

        // An unknown id updates nothing.
        assert!(
            !store
                .update_item(&item(1, 12345, "ghost"))
                .await
                .expect("update")
        );
    }

    #[tokio::test]
    async fn a_menu_can_inherit_from_a_parent() {
        let store = FakeCatalog::default();
        let standard = Menu {
            menu_id: menu_id(10),
            tenant_id: tenant(1),
            name: "Standard".to_owned(),
            parent_menu_id: None,
            status: EntityStatus::Active,
        };
        let grab = Menu {
            menu_id: menu_id(11),
            tenant_id: tenant(1),
            name: "Grab".to_owned(),
            parent_menu_id: Some(menu_id(10)),
            status: EntityStatus::Active,
        };
        store.create_menu(&standard).await.expect("create");
        store.create_menu(&grab).await.expect("create");

        let menus = store.list_menus(tenant(1)).await.expect("list");
        assert_eq!(menus.len(), 2);
        let child = menus
            .iter()
            .find(|m| m.menu_id == menu_id(11))
            .expect("present");
        assert_eq!(
            child.parent_menu_id,
            Some(menu_id(10)),
            "the inheritance edge is stored"
        );
    }

    #[tokio::test]
    async fn a_placement_is_upserted_by_its_menu_and_item_pair() {
        let store = FakeCatalog::default();
        let placement = |price: i64| MenuPlacement {
            tenant_id: tenant(1),
            menu_id: menu_id(10),
            menu_item_id: item_id(500),
            prices: vec![ChannelPrice {
                sales_channel: Open::from_known(SalesChannel::DineIn),
                unit_price: vnd(price),
            }],
            available: true,
        };
        store.set_placement(&placement(150_000)).await.expect("set");
        store
            .set_placement(&placement(160_000))
            .await
            .expect("re-set");

        let rows = store
            .list_placements(tenant(1), menu_id(10))
            .await
            .expect("list");
        assert_eq!(
            rows.len(),
            1,
            "re-setting the same pair replaces, not appends"
        );
        let row = rows.first().expect("one placement");
        assert_eq!(
            row.prices.first().expect("one price").unit_price,
            vnd(160_000)
        );

        assert!(
            store
                .remove_placement(tenant(1), menu_id(10), item_id(500))
                .await
                .expect("remove")
        );
        assert!(
            store
                .list_placements(tenant(1), menu_id(10))
                .await
                .expect("list")
                .is_empty()
        );
        assert!(
            !store
                .remove_placement(tenant(1), menu_id(10), item_id(500))
                .await
                .expect("remove"),
            "removing an absent placement changes nothing"
        );
    }
}
