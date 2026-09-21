// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A demo store's configuration document, for `examples/minimal-edge`.
//!
//! Behind the `demo-fixtures` feature, which no shipped binary enables: a real store's roster and
//! price book are authored in the console and synced down
//! ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md),
//! [ADR-0066](../../../docs/adr/0066-cloud-catalog.md),
//! [ADR-0070](../../../docs/adr/0070-people-and-access.md)), so the edge owns neither.
//!
//! # Why this exists
//!
//! [`crate::app::EdgeSession::bootstrap`] seeds an **empty** staff roster and an **empty** price
//! book, which is correct — inventing either would put a fabricated employee or a guessed price on a
//! till that has not synced. But it also meant `just run-edge` could not seat a table: every sign-in
//! was refused, so the paired device never passed the second gate (S0b,
//! [ADR-0084](../../../docs/adr/0084-device-authentication.md)) and every domain route answered
//! `403`. The example that exists to show a contributor the edge working showed a wall.
//!
//! # The seam it uses
//!
//! This module emits a **configuration document**, not a session. The example hands it to
//! [`crate::config_client::session_from_config`] — the same public function a real store's synced
//! config goes through, node for node. There is no second path into the roster and no back door: a
//! fixture that took one would stop proving that the published-config path works, which is the whole
//! value of running the example.

use pos_core::permission::Permission;
use pos_proto::SalesChannel;
use pos_proto::ids::{AreaId, EmployeeId, MenuItemId, TableId};
use pos_proto::menu::{MenuBook, MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

use crate::app::EdgeSession;

/// The badge code the demo store's one employee signs in with.
///
/// A published credential for a store that has no data, listens on loopback and forgets everything
/// on exit. It is printed at start-up rather than left for a reader to find, because a demo whose
/// credential is a scavenger hunt is a demo nobody runs.
pub const DEMO_STAFF_CODE: &str = "1001";

/// The PIN that goes with [`DEMO_STAFF_CODE`].
pub const DEMO_STAFF_PIN: &str = "1234";

/// The demo store's employee id — the identity every sale in the example is attributed to.
fn demo_employee() -> EmployeeId {
    EmployeeId::new(Ulid::from_u128(2))
}

/// Three items at round prices, priced on dine-in and on the fallback both, so the takeaway screen
/// sells the same things the floor does ([ADR-0093](../../../docs/adr/0093-takeaway.md)).
///
/// The tax class is [`EdgeSession::standard_tax_class`], the one the bootstrap rate table carries a
/// rate for on every channel — an entry naming any other class would price fine and then refuse to
/// settle, which is a worse first run than no menu at all.
fn demo_menu() -> MenuBook {
    let tax_class = EdgeSession::standard_tax_class();
    let item = |id: u128, name: &str, price: i64| {
        MenuEntry::new(
            MenuItemId::new(Ulid::from_u128(id)),
            DisplayName::new(name),
            Money::new(CurrencyCode::VND, price),
            tax_class,
        )
    };
    let catalog = MenuCatalog::new()
        .with(item(101, "Margherita", 149_000))
        .with(item(102, "Garden salad", 89_000))
        .with(item(103, "Iced tea", 39_000));
    MenuBook::new()
        .with(SalesChannel::DineIn, catalog.clone())
        .with_fallback(catalog)
}

/// The demo store's configuration document: a `permissions` node with one employee, and a `menu`
/// node with three items.
///
/// `None` if the PIN could not be hashed — the OS entropy source being unavailable is the only way
/// that happens, and a fixture that answered with a roster nobody can sign into would be worse than
/// one that says so.
///
/// Every other node is absent on purpose. The document a cloud publishes is partial by design and
/// each unnamed node leaves the base session as it was (the never-blank contract
/// [`crate::config_client::session_from_config`] keeps), so the floor stays the front end's own
/// fallback and the tax table stays the bootstrap 10% — exactly what a store that has synced people
/// and a menu, and nothing else, would show.
#[must_use]
pub fn config_document() -> Option<serde_json::Value> {
    let permissions: Vec<&str> = Permission::ALL
        .iter()
        .map(|permission| permission.meta().id)
        .collect();
    Some(serde_json::json!({
        "permissions": {
            "staff": [{
                "id": demo_employee().to_string(),
                "code": DEMO_STAFF_CODE,
                "permissions": permissions,
                "pin_phc": crate::auth::hash_pin(DEMO_STAFF_PIN)?,
            }],
        },
        "menu": serde_json::to_value(demo_menu()).ok()?,
        "floor": demo_floor(),
        // Seats on. The capability defaults **off** (§10) because most counters have no seats, so a
        // demo that left it at the default would ship a seat picker no contributor and no browser
        // gate ever saw — the same dark corner the floor plan sat in until it was published here.
        // The floor above gives every table a capacity, which is what the picker offers.
        "seats_enabled": true,
    }))
}

/// A floor with two named areas and every table placed on the editor's grid.
///
/// Published as JSON rather than built with the typed constructors, because that is what a cloud
/// actually sends and this fixture is only worth anything if it goes through the same
/// deserialization a real document does.
///
/// It exists so the example is a *room* rather than a list. The till draws areas, seat counts and
/// grid positions from the published plan ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)),
/// and until this node was here the only floor anybody — contributor or browser gate — ever saw was
/// the front end's eight-table fallback, which has none of those. A feature nothing exercises is a
/// feature nobody notices breaking.
///
/// The shape is deliberate: the main hall is a 3 × 2 grid with a **gap** where the walkway is, and
/// the terrace is a single row. The gap is the point — it is what proves the screen honours the
/// editor's coordinates rather than merely reflowing in order. Positions are **zero-based**
/// ([`pos_proto::display::GridPosition`]).
fn demo_floor() -> serde_json::Value {
    let table = |id: u128, label: &str, seats: u16, column: u16, row: u16| {
        serde_json::json!({
            "table_id": TableId::new(Ulid::from_u128(id)).to_string(),
            "label": label,
            "seats": seats,
            "position": { "column": column, "row": row },
        })
    };
    serde_json::json!({
        "areas": [
            {
                "area_id": AreaId::new(Ulid::from_u128(1)).to_string(),
                "name": "Main hall",
                "tables": [
                    table(201, "1", 2, 0, 0),
                    table(202, "2", 4, 1, 0),
                    table(203, "3", 4, 2, 0),
                    // Column 1 of the second row is the walkway: no table, and the screen must leave
                    // it empty rather than closing the gap.
                    table(204, "4", 6, 0, 1),
                    table(205, "5", 2, 2, 1),
                ],
            },
            {
                "area_id": AreaId::new(Ulid::from_u128(2)).to_string(),
                "name": "Terrace",
                "tables": [
                    table(206, "6", 4, 0, 0),
                    table(207, "7", 4, 1, 0),
                ],
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::{DEMO_STAFF_CODE, DEMO_STAFF_PIN, config_document};
    use crate::app::EdgeSession;
    use crate::config_client::session_from_config;

    /// The fixture is only worth anything if it goes through the published-config seam and comes out
    /// the other side as a roster that signs in and a book that prices — which is the exact pair the
    /// example needs and the exact pair `bootstrap()` cannot supply.
    #[test]
    fn the_demo_document_seeds_a_roster_that_signs_in_and_a_menu_that_prices() {
        let document = config_document().expect("the fixture hashes its PIN");
        let session = session_from_config(&EdgeSession::bootstrap(), &document);

        assert!(
            session.staff.credentials(DEMO_STAFF_CODE).is_some(),
            "the demo code resolves to an employee and a hash"
        );
        assert!(
            session
                .staff
                .authorise(DEMO_STAFF_CODE, DEMO_STAFF_PIN)
                .is_some(),
            "the demo PIN verifies against the hash the fixture published"
        );
        assert!(
            session.staff.authorise(DEMO_STAFF_CODE, "0000").is_none(),
            "a wrong PIN is refused, so the fixture is a roster and not a bypass"
        );
        assert_eq!(session.menu.items().len(), 3, "three items to sell");
    }

    /// The floor has to arrive as a *room*: named areas, seat counts, and grid positions with the
    /// walkway gap intact.
    ///
    /// This is the fixture's only reason for existing. The till reads areas, seats and positions from
    /// the published plan (ADR-0072), and before this node the example served none of them — so the
    /// only floor a contributor or the browser gate ever saw was the front end's flat fallback, and
    /// every one of those three could have broken without a single check going red.
    #[test]
    fn the_demo_document_publishes_a_floor_with_areas_seats_and_positions() {
        let document = config_document().expect("the fixture hashes its PIN");
        let session = session_from_config(&EdgeSession::bootstrap(), &document);

        let areas = session.floor.areas();
        assert_eq!(areas.len(), 2, "two named areas");
        assert_eq!(areas[0].name.as_str(), "Main hall");
        assert_eq!(areas[1].name.as_str(), "Terrace");

        let hall = &areas[0];
        assert_eq!(hall.tables.len(), 5, "five tables in the hall");
        assert!(
            hall.tables.iter().all(|table| table.position.is_some()),
            "every table is placed, which is what makes the screen draw a grid rather than a list"
        );
        assert!(
            hall.tables.iter().any(|table| table.seats == 6),
            "a six-top exists, so 'which table seats a party of six' has an answer"
        );

        // The walkway: row 1 has tables in columns 0 and 2, and nothing in column 1. A screen that
        // reflowed in order would close this gap, and the room would stop matching the screen.
        let second_row: Vec<u16> = hall
            .tables
            .iter()
            .filter_map(|table| table.position)
            .filter(|position| position.row == 1)
            .map(|position| position.column)
            .collect();
        assert_eq!(
            second_row,
            vec![0, 2],
            "the walkway in column 1 is left empty"
        );
    }

    /// Every item the fixture publishes has to be settleable, not merely addable: the bill assembles
    /// at the rate its class carries for the order's channel, so a class with no rate would fail at
    /// the till rather than here.
    #[test]
    fn every_demo_item_is_priced_in_a_class_the_bootstrap_table_rates() {
        let document = config_document().expect("the fixture hashes its PIN");
        let session = session_from_config(&EdgeSession::bootstrap(), &document);
        for entry in session.menu.items() {
            assert_eq!(
                entry.tax_class_id,
                EdgeSession::standard_tax_class(),
                "{} is priced in a class the bootstrap rate table does not carry",
                entry.display_name
            );
        }
    }
}
