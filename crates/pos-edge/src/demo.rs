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

use std::collections::BTreeMap;

use pos_core::permission::Permission;
use pos_proto::SalesChannel;
use pos_proto::ids::{AreaId, CourseId, EmployeeId, MenuItemId, ModifierGroupId, TableId};
use pos_proto::menu::{MenuBook, MenuCatalog, MenuCourse, MenuEntry, MenuModifierGroup};
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

/// Three items priced on dine-in and on the fallback both, so the takeaway screen sells the same
/// things the floor does ([ADR-0093](../../../docs/adr/0093-takeaway.md)).
///
/// **One of them is deliberately not a round thousand**, and that is a gate decision rather than a
/// menu one. Every price here used to be a multiple of 1,000; add a 10% exclusive rate to any of
/// them, in any combination, at any quantity, and the total is a multiple of 100. Five, ten and
/// fifteen percent of a multiple of 100 are all whole numbers, so the till's tip keys came out whole
/// no matter what a flow bought — and the browser gate could not see a money bug that only shows on
/// a total the arithmetic does not divide. It did not see one: the pay screen computed its tip keys
/// with a float division, and on a bill of 304,733₫ the 5% key was 15,236.65, which the edge's own
/// deserializer refuses. A cashier met that as a generic store error at the settle.
///
/// The iced tea is 39,500₫ so that one item, bought alone, produces 43,450₫ — and five percent of
/// that is 2,172.5. A fixture whose every number divides is a fixture that proves the easy half.
///
/// The bottled water is here for the same kind of reason at the other end of the range: at 9,000₫ it
/// is the only bill small enough that a 1,000₫ cash increment swallows the difference between a 5%
/// and a 15% tip, which is the case the till's tip keys have to stand down on.
///
/// The tax class is [`EdgeSession::standard_tax_class`], the one the bootstrap rate table carries a
/// rate for on every channel — an entry naming any other class would price fine and then refuse to
/// settle, which is a worse first run than no menu at all.
/// Whether this demo store runs table service — `POS_DEMO_PROFILE=counter` says it does not.
///
/// Read here rather than threaded through `config_document`'s signature because it is a property of
/// the *fixture*, not of the caller: `examples/minimal-edge` asks for "the demo store" and the
/// environment says which one, exactly as it says which port to bind.
fn table_service() -> bool {
    !std::env::var("POS_DEMO_PROFILE").is_ok_and(|profile| profile.eq_ignore_ascii_case("counter"))
}

/// Whether this demo store's country rounds its cash — `POS_DEMO_PROFILE=cash-rounding`.
///
/// A property of the *country*, not of the shop: Vietnam rounds to the thousand đồng because that is
/// the smallest note, India to the rupee because no smaller coin settles the difference, Japan not
/// at all because the 1-yen coin circulates ([ADR-0105](../../../docs/adr/0105-country-pack.md)).
/// The default demo store rounds nothing, which is the posture every other flow is written against
/// and the one that leaves a total with awkward arithmetic in it.
///
/// It is a third profile rather than a second flag on the counter one because the two are
/// orthogonal: a counter cafe in Hanoi rounds its cash and a table-service restaurant in Tokyo does
/// not. This profile leaves table service on, so the flows that drive the floor drive it unchanged.
fn cash_rounding() -> bool {
    std::env::var("POS_DEMO_PROFILE")
        .is_ok_and(|profile| profile.eq_ignore_ascii_case("cash-rounding"))
}

/// Whether this demo store has anybody to sign in: `POS_DEMO_PROFILE=unstaffed` says it has not.
///
/// That profile publishes no `permissions` node, which is what a store the console has not staffed
/// yet receives. It exists for the sign-in screen, which has to say so rather than refuse every
/// code as a mistyped one; `examples/minimal-edge` reads it to know whether there is a badge to
/// print.
#[must_use]
pub fn staffed() -> bool {
    !std::env::var("POS_DEMO_PROFILE")
        .is_ok_and(|profile| profile.eq_ignore_ascii_case("unstaffed"))
}

fn demo_menu() -> MenuBook {
    let tax_class = EdgeSession::standard_tax_class();
    let menu_item = |id: u128| MenuItemId::new(Ulid::from_u128(id));
    let group = |id: u128| ModifierGroupId::new(Ulid::from_u128(id));
    let item = |id: u128, name: &str, price: i64| {
        MenuEntry::new(
            MenuItemId::new(Ulid::from_u128(id)),
            DisplayName::new(name),
            Money::new(CurrencyCode::VND, price),
            tax_class,
        )
    };
    // The two sizes a Margherita comes in, and one topping. Modifiers are ordinary items with their
    // own prices (ADR-0066 entity 4), which is how a large costs more than a small without a second
    // pricing concept — and why they are in the catalog beside the pizza rather than beside the rule.
    // A plain item leads, deliberately. The browser gate's "add an item" precondition taps the
    // *first* item on the grid, and it wants a line rather than a conversation — so the item that
    // asks a question is not the one a flow reaches by accident. The flow that does want it types
    // the name first, which the menu search made possible and which costs no tap.
    let course = |id: u128| CourseId::new(Ulid::from_u128(id));
    // Starter, main, dessert — the sequence the grouping exists for (ADR-0130). The salad is a
    // starter and the pizza a main, so the order screen has two courses to offer and firing one of
    // them leaves the other waiting; a fixture where everything shared a course would let a
    // fire-by-course that ignored the filter pass. The iced tea is on **no** course deliberately: a
    // drink goes when it is poured, and a line on no course is what most lines in most stores are —
    // so the flow that fires the starters must leave it alone, and the gate can see that it does.
    let catalog = MenuCatalog::new()
        .with(item(102, "Garden salad", 89_000).with_course(course(900)))
        .with(item(103, "Iced tea", 39_500))
        .with(
            item(101, "Margherita", 149_000)
                .with_modifier_groups(vec![group(700), group(701)])
                .with_course(course(901)),
        )
        // One item with tone marks on it, and it is not decoration. The order screen's menu search
        // folds diacritics so that `dac` reaches this — nobody switches input mode mid-service — and
        // a fixture whose every item was ASCII would leave that fold with no gate over it, the same
        // dark corner the floor plan and the seat picker sat in before they were published here.
        //
        // The name is picked for what it is made of, not for the cuisine. `ặ` decomposes into a
        // letter and two combining marks; `đ` decomposes into nothing at all, because it is its own
        // letter rather than `d` with a mark on it. Those are the two cases the fold has to handle
        // separately, and `đặc` is one syllable carrying both.
        .with(item(104, "Phở bò đặc biệt", 99_000))
        // The cheapest thing on the menu, and it is here for the arithmetic at the bottom of the
        // range rather than for the thirst. A store that rounds its cash to 1,000 đồng rounds this
        // bill to 10,000, and five, ten and fifteen percent of that snap to 1,000, 1,000 and 2,000 —
        // two identical buttons and a row a cashier cannot choose from. That is the case the till's
        // tip keys have to stand down on, and with every other item priced in the tens of thousands
        // there was no bill small enough to reach it.
        .with(item(105, "Bottled water", 9_000))
        .with(item(201, "Size — 25cm", 0))
        .with(item(202, "Size — 30cm", 40_000))
        .with(item(210, "Extra cheese", 25_000))
        // One required group and one optional, because the pair is what makes the rule visible: a
        // pizza cannot be sold without a size, and can be sold without cheese. A demo with only the
        // optional one would ship a picker no contributor and no browser gate ever had to satisfy.
        .with_modifier_group(MenuModifierGroup {
            modifier_group_id: group(700),
            display_name: DisplayName::new("Size"),
            display_name_translations: BTreeMap::new(),
            min_select: 1,
            max_select: 1,
            member_menu_item_ids: vec![menu_item(201), menu_item(202)],
        })
        .with_modifier_group(MenuModifierGroup {
            modifier_group_id: group(701),
            display_name: DisplayName::new("Extras"),
            display_name_translations: BTreeMap::new(),
            min_select: 0,
            max_select: 1,
            member_menu_item_ids: vec![menu_item(210)],
        })
        // Out of sequence on purpose, and with a gap between the positions: the compiler emits these
        // sorted and the till must render them in the order given, so a fixture authored in service
        // order would let a till that re-sorted — or one that ignored `sort` entirely — pass.
        .with_course(MenuCourse::new(course(901), DisplayName::new("Mains"), 20))
        .with_course(MenuCourse::new(
            course(900),
            DisplayName::new("Starters"),
            10,
        ));
    MenuBook::new()
        .with(SalesChannel::DineIn, catalog.clone())
        .with_fallback(catalog)
}

/// The demo store's configuration document: a `permissions` node with one employee, and a `menu`
/// node with four products, the three modifiers they are made of, and the two groups that offer
/// them.
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
///
/// # The counter profile
///
/// `POS_DEMO_PROFILE=counter` publishes the same store with **table service off**. §10's capability
/// model has carried a counter preset since it was written — `Capability::PayFirst` is even
/// declared incompatible with `Tables` in its own validity rules — and the till implemented exactly
/// one profile, because there was no way to run the other one. A contributor can now see what a
/// counter cafe sees, and `ui/tests/replay.spec.mjs` drives it.
///
/// # The cash-rounding profile
///
/// `POS_DEMO_PROFILE=cash-rounding` publishes the same store with a `locale` node naming Vietnam's
/// own increment, 1,000 đồng. ADR-0105 has carried `cash_rounding_increment` since it was written
/// and no fixture ever published one, so the whole posture — the core's rounding adjustment, the
/// till's tip keys, what a guest is actually asked for — had no browser gate over it at all.
///
/// It is the one profile where a total is guaranteed to be a round note, which is why it cannot be
/// the default: the fractional-tip replay needs a total that is *not*, and no single store can be
/// both.
///
/// # The unstaffed profile
///
/// `POS_DEMO_PROFILE=unstaffed` publishes the same store with no `permissions` node: nobody can
/// sign in, as on a store the console has not staffed yet. See [`staffed`].
///
/// An environment variable rather than a second example binary: the profiles differ by a published
/// node apiece, and a second `main.rs` would be a second copy of the boot path — which is the thing
/// that drifts.
#[must_use]
pub fn config_document() -> Option<serde_json::Value> {
    let permissions: Vec<&str> = Permission::ALL
        .iter()
        .map(|permission| permission.meta().id)
        .collect();
    let mut document = serde_json::json!({
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
        // Table service, unless the profile says otherwise. On by default (§10), so the flag is
        // published either way rather than only when it is false — a document that named it only to
        // turn it off would leave the common case relying on the default and the rare case on the
        // node, which is two paths where the till reads one.
        "tables_enabled": table_service(),
        // The kitchen board follows the same profile: a counter cafe hands the drink over, and a
        // board nobody looks at is a destination in the status bar that leads nowhere.
        "kds_enabled": table_service(),
        // Seats on. The capability defaults **off** (§10) because most counters have no seats, so a
        // demo that left it at the default would ship a seat picker no contributor and no browser
        // gate ever saw — the same dark corner the floor plan sat in until it was published here.
        // The floor above gives every table a capacity, which is what the picker offers.
        "seats_enabled": true,
    });
    // Published only on the profile that asks for it, so the default store's money settings stay the
    // bootstrap's and every existing flow reads the totals it always read. A node that was always
    // present, merely carrying a different increment, would make the two profiles differ by a value
    // inside a node rather than by the node — and the one that is absent is the one this fixture is
    // documenting, because absent is what every store publishes today.
    if cash_rounding()
        && let Some(object) = document.as_object_mut()
    {
        object.insert("locale".to_owned(), demo_locale());
    }
    // Removed rather than built empty: an absent node is what an unstaffed store is published, and
    // it leaves the bootstrap's empty roster in place exactly as it would there.
    if !staffed()
        && let Some(object) = document.as_object_mut()
    {
        object.remove("permissions");
    }
    Some(document)
}

/// The money settings of a store in a country that rounds its cash (ADR-0105).
///
/// `currency_code`, `timezone` and `cutoff_hour` carry no default on the wire, so a locale node has
/// to name them even when the increment is the only thing it is published for; they repeat what the
/// bootstrap already holds. `cash_denominations` is deliberately **absent**: which notes a guest
/// carries is a separate fact from what the total rounds to, the till already falls back to its own
/// table for the currency, and a fixture that published both would not show which of the two the
/// quick-cash keys come from.
fn demo_locale() -> serde_json::Value {
    serde_json::json!({
        "currency_code": "VND",
        "timezone": "Asia/Ho_Chi_Minh",
        "cutoff_hour": 4,
        // The smallest note in circulation. Anything finer is a figure a cashier cannot settle and a
        // guest cannot hand over.
        "cash_rounding_increment": 1_000,
        // How Vietnam writes a number: `.` between groups and `,` before a fraction — the mirror of
        // the convention this app compiled in for every country
        // ([ADR-0136](../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)).
        //
        // Published here rather than in a profile of its own because this fixture already *is* a
        // Vietnamese store: it publishes Vietnam's cash increment and quotes in đồng, and a
        // Vietnamese store that grouped `98,000` was the fixture being less faithful than the
        // country it names. The browser gate's own expectations move with it, which is the proof
        // that the screen reads the published marks rather than a compiled-in guess.
        "number_format": {
            "decimal_separator": ",",
            "group_separator": ".",
            "digits_per_group": 3,
        },
    })
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
    use pos_proto::ids::MenuItemId;
    use pos_proto::ulid::Ulid;

    use super::{DEMO_STAFF_CODE, DEMO_STAFF_PIN, config_document};
    use crate::app::EdgeSession;
    use crate::config_client::session_from_config;

    /// The same numbering the fixture above mints its items from, so an assertion names the item it
    /// means rather than a ULID nobody can read.
    fn menu_item(id: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(id))
    }

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
        assert_eq!(
            session.menu.items().len(),
            8,
            "five products plus the three modifiers, which are ordinary priced items (ADR-0066 \
             entity 4) and so are counted here"
        );
        let sizes = session
            .menu
            .groups_for(menu_item(101))
            .first()
            .copied()
            .cloned()
            .expect("the pizza asks what size");
        assert!(sizes.required(), "a pizza cannot be sold without a size");
        assert_eq!(
            sizes.member_menu_item_ids,
            vec![menu_item(201), menu_item(202)],
            "and both sizes are on the menu, so the picker has something to offer"
        );
        assert!(
            session.menu.groups_for(menu_item(102)).is_empty(),
            "a salad asks nothing, which is what keeps adding it one tap"
        );
        assert!(
            session
                .menu
                .items()
                .iter()
                .any(|entry| entry.display_name.as_str() == "Phở bò đặc biệt"),
            "a name with tone marks survives the published-config seam, so the till's menu search \
             has something to fold"
        );
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
