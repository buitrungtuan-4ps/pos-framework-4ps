// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::SalesChannel;
use pos_proto::ids::MenuItemId;
use pos_proto::shift::DrawerModel;
use pos_proto::ulid::Ulid;

use super::{
    DEMO_BAR_FLOAT, DEMO_MANAGER_CODE, DEMO_MANAGER_PIN, DEMO_STAFF_CODE, DEMO_STAFF_PIN,
    WITH_APPROVAL, config_document, demo_counter, demo_drawers, demo_menu, demo_permissions,
    demo_stations, demo_tender_keys, demo_tills,
};
use crate::app::EdgeSession;

use crate::config_client::session_from_config;
use crate::printing::{published_printers, published_terminals};

/// The same numbering the fixture above mints its items from, so an assertion names the item it
/// means rather than a ULID nobody can read.
fn menu_item(id: u128) -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(id))
}

/// The `approvals` profile enforces each person's own set: the employee holds the voids and the
/// reductions only with approval, and the manager's badge signs in and holds them directly.
#[test]
fn the_approvals_profile_asks_a_manager_for_voids_and_reductions() {
    let mut document = config_document().expect("the fixture hashes its PIN");
    document["permissions"] = demo_permissions(true).expect("the fixture hashes its PINs");
    assert_eq!(document["permissions"]["enforced"], true);
    let employee = &document["permissions"]["staff"][0];
    for id in WITH_APPROVAL {
        assert!(
            !employee["permissions"]
                .as_array()
                .expect("a list")
                .iter()
                .any(|held| held == id),
            "{id} is not held directly"
        );
    }
    assert_eq!(
        employee["permissions_with_approval"]
            .as_array()
            .map(Vec::len),
        Some(4)
    );
    let session = session_from_config(&EdgeSession::bootstrap(), &document);
    assert!(
        session
            .staff
            .authorise(DEMO_MANAGER_CODE, DEMO_MANAGER_PIN)
            .is_some(),
        "the manager's badge signs in"
    );
    assert!(
        session
            .staff
            .authorise(DEMO_STAFF_CODE, DEMO_STAFF_PIN)
            .is_some()
    );
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

/// The kitchen-stations profile's node arrives as two stations with their own thresholds: the
/// kitchen's two minutes, and the bar's ten, which it sets by setting nothing. The iced tea goes
/// to the bar and everything else to the kitchen, so one order puts a ticket on each.
#[test]
fn the_kitchen_stations_node_gives_each_station_its_own_late_threshold() {
    let document = serde_json::json!({ "stations": demo_stations() });
    let session = session_from_config(&EdgeSession::bootstrap(), &document);
    let thresholds: Vec<(&str, u32)> = session
        .stations
        .stations()
        .iter()
        .map(|station| (station.name.as_str(), station.late_after_seconds()))
        .collect();
    assert_eq!(thresholds, [("Kitchen", 120), ("Bar", 600)]);
    let kitchen = session.stations.stations()[0].station_id;
    let bar = session.stations.stations()[1].station_id;
    assert_eq!(session.resolve_station(menu_item(103), None), Some(bar));
    assert_eq!(session.resolve_station(menu_item(102), None), Some(kitchen));
}

/// The tills profile's node arrives as two tills a device can be bound to, in its order, and
/// nothing a receipt prints at.
#[test]
fn the_tills_node_publishes_two_terminals_and_no_printer() {
    let document = serde_json::json!({ "devices": demo_tills(None) });
    let session = session_from_config(&EdgeSession::bootstrap(), &document);
    let tills: Vec<&str> = published_terminals(&session.devices)
        .map(|till| till.name.as_str())
        .collect();
    assert_eq!(tills, ["Counter till", "Bar till"]);
    assert!(published_printers(&session.devices).is_empty());
}

/// The drawers profile keeps a drawer per till, the counter's on the store's float and the
/// bar's on its own, and asks a reason of a close more than 20,000₫ over or short.
#[test]
fn the_drawers_profile_keeps_a_drawer_per_till_and_the_bar_floats_its_own() {
    let document = serde_json::json!({
        "devices": demo_tills(Some(DEMO_BAR_FLOAT)),
        "shift": demo_drawers(),
    });
    let session = session_from_config(&EdgeSession::bootstrap(), &document);
    assert_eq!(session.shift.drawer_model(), DrawerModel::PerTerminal);
    assert_eq!(session.shift.opening_float_minor(), 500_000);
    assert_eq!(session.shift.variance_reason_minor(), 20_000);
    let floats: Vec<(&str, Option<i64>)> = published_terminals(&session.devices)
        .map(|till| (till.name.as_str(), till.opening_float_minor()))
        .collect();
    assert_eq!(
        floats,
        [("Counter till", None), ("Bar till", Some(DEMO_BAR_FLOAT))]
    );
}

/// The tender-keys profile's node offers the store's own two tip keys, without the one it hid.
#[test]
fn the_tender_keys_node_offers_the_stores_two_tip_keys() {
    let document = serde_json::json!({ "tender_keys": demo_tender_keys() });
    let session = session_from_config(&EdgeSession::bootstrap(), &document);
    assert_eq!(session.tender_keys.tip_percents(), vec![10, 20]);
    assert_eq!(session.tender_keys.split_ways_max(), 12);
}

/// The walk-in profiles sell the salad for 79,000₫ to take away and 89,000₫ in the dining room,
/// and `walk-in-dine-in` opens its walk-ins eaten in; the book every other profile publishes
/// prices takeaway as the dining room.
#[test]
fn the_walk_in_profiles_price_takeaway_apart_and_one_eats_its_walk_ins_in() {
    let salad = |session: &EdgeSession, channel| {
        session
            .menu_for(channel)
            .items()
            .iter()
            .find(|entry| entry.menu_item_id == menu_item(102))
            .map(|entry| entry.unit_price.amount_minor)
    };
    let document = serde_json::json!({
        "menu": serde_json::to_value(demo_menu(true)).expect("the book serialises"),
        "counter": demo_counter("walk-in-dine-in"),
    });
    let session = session_from_config(&EdgeSession::bootstrap(), &document);
    assert_eq!(salad(&session, SalesChannel::Takeaway), Some(79_000));
    assert_eq!(salad(&session, SalesChannel::DineIn), Some(89_000));
    assert_eq!(
        session.counter.walk_in_channel().sales_channel(),
        SalesChannel::DineIn
    );
    assert_eq!(demo_counter("walk-in-takeaway"), None);

    let document = serde_json::json!({
        "menu": serde_json::to_value(demo_menu(false)).expect("the book serialises"),
    });
    let session = session_from_config(&EdgeSession::bootstrap(), &document);
    assert_eq!(salad(&session, SalesChannel::Takeaway), Some(89_000));
}
