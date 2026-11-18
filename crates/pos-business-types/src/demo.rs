// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A template's fake demo menu, floor and kitchen stations (`business-types/<id>/demo.json`), for the simulator's
//! template-by-country matrix and the console's preview
//! ([ADR-0200](../../../docs/adr/0200-six-business-type-templates-ship-with-the-framework.md)
//! decisions 1 and 6).
//!
//! Every name is obviously fake (`Demo …`), and an item states its price once per currency a shipped
//! country pack trades in, in that currency's minor unit, so one template sells in every country of
//! the matrix. The ids are derived from the business type and the item's or table's `key`, so the
//! same template always publishes the same ids.

use std::collections::BTreeMap;

use pos_proto::BusinessType;
use pos_proto::Open;
use pos_proto::floor::{FloorArea, FloorPlan, FloorTable, KitchenStation, StationPlan};
use pos_proto::ids::{AreaId, MenuItemId, StationId, TableId, TaxClassId};
use pos_proto::item_codes::ItemCodes;
use pos_proto::menu::{MenuBook, MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum;
use serde::Deserialize;

/// The most items, tables or stations a demo may hold.
pub const MAX_DEMO_ENTRIES: usize = 64;

/// A template's demo menu and floor, as read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Demo {
    /// The menu, in the order the till shows it.
    pub items: Vec<DemoItem>,
    /// The floor, for a template that serves at tables; absent for a counter.
    #[serde(default)]
    pub floor: Option<DemoFloor>,
    /// The kitchen stations' names, for a template with a kitchen display; the first is where a
    /// line no rule routes goes.
    #[serde(default)]
    pub stations: Vec<String>,
}

/// One demo item.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoItem {
    /// Unique within the template; the item's id is derived from it.
    pub key: u16,
    /// An obviously fake name, starting `Demo `.
    pub display_name: String,
    /// The tax class, one of the classes every pack prices.
    pub tax_class: DemoTaxClass,
    /// The price per ISO 4217 currency code, in that currency's minor unit, above zero.
    pub prices: BTreeMap<String, i64>,
    /// Fake barcodes a till scans for it, for a template that sells by barcode (ADR-0222).
    #[serde(default)]
    pub barcodes: Vec<String>,
    /// A PLU a cashier keys for it.
    #[serde(default)]
    pub plu: Option<String>,
}

impl DemoItem {
    /// Its barcodes and PLU as the menu carries them.
    fn codes(&self) -> ItemCodes {
        ItemCodes {
            barcodes: self.barcodes.clone(),
            plu: self.plu.clone(),
            open_price: None,
        }
    }
}

/// The tax class a demo item is sold under: one of `pos_country::tax_class`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoTaxClass {
    /// Food and non-alcoholic drink.
    Food,
    /// Alcohol.
    Alcohol,
}

impl DemoTaxClass {
    /// The class's id, as every country pack's rate table names it.
    #[must_use]
    pub fn tax_class_id(self) -> TaxClassId {
        match self {
            Self::Food => pos_country::tax_class::food(),
            Self::Alcohol => pos_country::tax_class::alcohol(),
        }
    }
}

/// The demo floor: areas of tables.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoFloor {
    /// The areas, in order.
    pub areas: Vec<DemoArea>,
}

/// One demo area.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoArea {
    /// An obviously fake name.
    pub name: String,
    /// Its tables.
    pub tables: Vec<DemoTable>,
}

/// One demo table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoTable {
    /// The label on the table.
    pub label: String,
    /// How many it seats.
    pub seats: u16,
}

/// Why a demo is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoFault {
    /// No items, or more than [`MAX_DEMO_ENTRIES`] items or tables.
    Size,
    /// Two items share a key.
    DuplicateKey,
    /// A name that does not start `Demo `, so a real-looking name cannot slip in.
    NotObviouslyFake,
    /// A price at or below zero, or a currency code that is not one.
    BadPrice,
    /// A barcode or PLU that is malformed, or that two items share (ADR-0222).
    BadCode,
}

impl Demo {
    /// Checks the demo's shape: its size, its keys, its names and its prices.
    ///
    /// # Errors
    ///
    /// The first [`DemoFault`] found.
    pub fn check(&self) -> Result<(), DemoFault> {
        let tables: usize = self
            .floor
            .iter()
            .flat_map(|floor| &floor.areas)
            .map(|area| area.tables.len())
            .sum();
        if self.items.is_empty()
            || self.items.len() > MAX_DEMO_ENTRIES
            || tables > MAX_DEMO_ENTRIES
            || self.stations.len() > MAX_DEMO_ENTRIES
        {
            return Err(DemoFault::Size);
        }
        for (index, item) in self.items.iter().enumerate() {
            if self
                .items
                .iter()
                .take(index)
                .any(|earlier| earlier.key == item.key)
            {
                return Err(DemoFault::DuplicateKey);
            }
            if !item.display_name.starts_with("Demo ") {
                return Err(DemoFault::NotObviouslyFake);
            }
            let priced = item
                .prices
                .iter()
                .all(|(code, minor)| *minor > 0 && CurrencyCode::parse(code).is_ok());
            if !priced {
                return Err(DemoFault::BadPrice);
            }
            let codes = item.codes();
            let shared = self.items.iter().take(index).any(|earlier| {
                earlier
                    .codes()
                    .codes()
                    .any(|code| codes.codes().any(|own| own == code))
            });
            if codes.fault().is_some() || shared {
                return Err(DemoFault::BadCode);
            }
        }
        let names = self
            .floor
            .iter()
            .flat_map(|floor| &floor.areas)
            .map(|area| area.name.as_str());
        let mut names = names.chain(self.stations.iter().map(String::as_str));
        if names.any(|name| !name.starts_with("Demo ")) {
            return Err(DemoFault::NotObviouslyFake);
        }
        Ok(())
    }

    /// The demo menu priced in `currency`, for every channel; `None` when an item has no price in
    /// it.
    #[must_use]
    pub fn menu(&self, business_type: BusinessType, currency: CurrencyCode) -> Option<MenuBook> {
        let mut catalog = MenuCatalog::new();
        for item in &self.items {
            let minor = *item.prices.get(currency.as_str())?;
            catalog = catalog.with(MenuEntry {
                codes: item.codes(),
                ..MenuEntry::new(
                    MenuItemId::new(derived_id(business_type, 1, item.key)),
                    DisplayName::new(item.display_name.as_str()),
                    Money::new(currency, minor),
                    item.tax_class.tax_class_id(),
                )
            });
        }
        Some(MenuBook::new().with_fallback(catalog))
    }

    /// The demo floor as a store's floor plan, or `None` for a counter.
    #[must_use]
    pub fn floor_plan(&self, business_type: BusinessType) -> Option<FloorPlan> {
        let floor = self.floor.as_ref()?;
        let mut next_table: u16 = 0;
        let mut plan = FloorPlan::new();
        for (area_index, area) in (0_u16..).zip(&floor.areas) {
            let mut tables = Vec::with_capacity(area.tables.len());
            for table in &area.tables {
                next_table = next_table.saturating_add(1);
                tables.push(FloorTable {
                    table_id: TableId::new(derived_id(business_type, 3, next_table)),
                    label: DisplayName::new(table.label.as_str()),
                    seats: table.seats,
                    position: None,
                });
            }
            plan = plan.with(FloorArea::top(
                AreaId::new(derived_id(business_type, 2, area_index)),
                DisplayName::new(area.name.as_str()),
                tables,
            ));
        }
        Some(plan)
    }
}

impl Demo {
    /// The demo's kitchen stations as a store's station plan, the first the default; `None` for a
    /// template with no kitchen.
    #[must_use]
    pub fn station_plan(&self, business_type: BusinessType) -> Option<StationPlan> {
        let mut plan = StationPlan::new();
        for (key, name) in (1_u16..).zip(&self.stations) {
            let station_id = StationId::new(derived_id(business_type, 4, key));
            if key == 1 {
                plan = plan.with_default(station_id);
            }
            plan = plan.with_station(KitchenStation {
                station_id,
                name: DisplayName::new(name.as_str()),
                backup_station_id: None,
                late_after_seconds: None,
                ticket_language: Open::default(),
            });
        }
        (!self.stations.is_empty()).then_some(plan)
    }
}

/// A fixed id for a template's item (kind 1), area (2), table (3) or station (4), so a template always publishes
/// the same ids and two templates never share one.
fn derived_id(business_type: BusinessType, kind: u128, key: u16) -> Ulid {
    let type_index = BusinessType::ALL
        .iter()
        .position(|known| *known == business_type)
        .unwrap_or_default();
    // `usize` to `u128` never truncates; the position is below 16.
    let type_index = u128::try_from(type_index).unwrap_or_default();
    Ulid::from_u128(0xDE30_0000_0000 + (type_index << 24) + (kind << 16) + u128::from(key))
}
