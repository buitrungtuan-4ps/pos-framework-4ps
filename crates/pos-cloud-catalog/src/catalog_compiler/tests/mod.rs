use std::collections::BTreeMap;

use pos_proto::display::GridPosition;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{
    CourseId, DisplayCategoryId, DisplaySubcategoryId, MenuItemId, ModifierGroupId, TaxClassId,
    TenantId,
};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use super::{CompileError, compile_layout_book, compile_menu, would_cycle};
use crate::catalog::{
    CatalogItem, ChannelPrice, Course, DisplayCategory, DisplaySubcategory, LayoutButton, Menu,
    MenuId, MenuPlacement, ModifierGroup,
};
use crate::registry::EntityStatus;

fn tenant() -> TenantId {
    TenantId::new(Ulid::from_u128(1))
}

fn item_id(n: u128) -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(n))
}

fn menu_id(n: u128) -> MenuId {
    MenuId::new(Ulid::from_u128(n))
}

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn item(id: u128, name: &str) -> CatalogItem {
    CatalogItem {
        menu_item_id: item_id(id),
        tenant_id: tenant(),
        name: name.to_owned(),
        name_translations: BTreeMap::new(),
        tax_class_id: TaxClassId::new(Ulid::from_u128(7)),
        item_category_id: None,
        item_subcategory_id: None,
        course_id: None,
        image_ref: None,
        status: EntityStatus::Active,
    }
}

fn menu(id: u128, parent: Option<u128>) -> Menu {
    Menu {
        menu_id: menu_id(id),
        tenant_id: tenant(),
        name: format!("menu-{id}"),
        parent_menu_id: parent.map(menu_id),
        status: EntityStatus::Active,
    }
}

fn price(channel: SalesChannel, minor: i64) -> ChannelPrice {
    ChannelPrice {
        sales_channel: Open::from_known(channel),
        unit_price: vnd(minor),
    }
}

fn placement(menu: u128, item: u128, prices: Vec<ChannelPrice>, available: bool) -> MenuPlacement {
    MenuPlacement {
        tenant_id: tenant(),
        menu_id: menu_id(menu),
        menu_item_id: item_id(item),
        menu_section_id: None,
        prices,
        available,
    }
}

fn course_id(n: u128) -> CourseId {
    CourseId::new(Ulid::from_u128(n))
}

/// A course at a position in the service sequence.
fn course(id: u128, name: &str, sort: i32) -> Course {
    Course {
        course_id: course_id(id),
        tenant_id: tenant(),
        name: name.to_owned(),
        sort,
        status: EntityStatus::Active,
    }
}

/// The same item, declaring the course it goes out on.
fn item_on_course(id: u128, name: &str, course: u128) -> CatalogItem {
    CatalogItem {
        course_id: Some(course_id(course)),
        ..item(id, name)
    }
}

#[test]
fn a_flat_menu_compiles_to_per_channel_catalogs() {
    let items = [item(500, "Margherita")];
    let menus = [menu(10, None)];
    let placements = [placement(
        10,
        500,
        vec![
            price(SalesChannel::DineIn, 150_000),
            price(SalesChannel::Delivery, 180_000),
        ],
        true,
    )];

    let book = compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)).expect("compile");

    let dine_in = book
        .catalog_for(SalesChannel::DineIn)
        .get(item_id(500))
        .expect("priced dine-in");
    assert_eq!(dine_in.unit_price, vnd(150_000));
    assert_eq!(dine_in.display_name.as_str(), "Margherita");
    assert!(dine_in.available);

    assert_eq!(
        book.catalog_for(SalesChannel::Delivery)
            .get(item_id(500))
            .expect("priced delivery")
            .unit_price,
        vnd(180_000),
        "the same item is a different price on delivery"
    );

    assert!(
        book.catalog_for(SalesChannel::Takeaway).is_empty(),
        "a channel the item is not priced on sells nothing"
    );
}

#[test]
fn per_locale_item_names_travel_onto_the_compiled_entry() {
    // The item's per-locale names compile onto the `MenuEntry`, with the default `name` still the
    // fallback (ADR-0074). Selection happens later, at the edge, from the store's language.
    let mut translated = item(500, "Margherita");
    translated
        .name_translations
        .insert("vi".to_owned(), "Bánh Margherita".to_owned());
    let items = [translated];
    let menus = [menu(10, None)];
    let placements = [placement(
        10,
        500,
        vec![price(SalesChannel::DineIn, 150_000)],
        true,
    )];

    let book = compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)).expect("compile");
    let entry = book
        .catalog_for(SalesChannel::DineIn)
        .get(item_id(500))
        .expect("priced dine-in")
        .clone();
    assert_eq!(
        entry.display_name.as_str(),
        "Margherita",
        "default is the fallback"
    );
    assert_eq!(entry.localized_name("vi").as_str(), "Bánh Margherita");
    assert_eq!(
        entry.localized_name("en").as_str(),
        "Margherita",
        "an untranslated locale falls back to the default name"
    );
}

#[test]
fn a_child_menu_overrides_a_parents_price_and_inherits_the_rest() {
    let items = [item(500, "Margherita"), item(501, "Marinara")];
    let menus = [menu(10, None), menu(11, Some(10))];
    let placements = [
        // Parent prices both.
        placement(10, 500, vec![price(SalesChannel::DineIn, 150_000)], true),
        placement(10, 501, vec![price(SalesChannel::DineIn, 120_000)], true),
        // Child overrides only 500.
        placement(11, 500, vec![price(SalesChannel::DineIn, 175_000)], true),
    ];

    let book = compile_menu(&items, &menus, &placements, &[], &[], menu_id(11)).expect("compile");
    let dine_in = book.catalog_for(SalesChannel::DineIn);
    assert_eq!(
        dine_in.get(item_id(500)).expect("overridden").unit_price,
        vnd(175_000),
        "the child's price wins over the parent's"
    );
    assert_eq!(
        dine_in.get(item_id(501)).expect("inherited").unit_price,
        vnd(120_000),
        "an item the child does not touch is inherited from the parent"
    );
}

#[test]
fn an_unavailable_placement_compiles_to_an_86d_entry() {
    let items = [item(500, "Margherita")];
    let menus = [menu(10, None)];
    let placements = [placement(
        10,
        500,
        vec![price(SalesChannel::DineIn, 150_000)],
        false,
    )];

    let book = compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)).expect("compile");
    let entry = book
        .catalog_for(SalesChannel::DineIn)
        .get(item_id(500))
        .expect("present");
    assert!(
        !entry.available,
        "a paused item is present but not for sale, not absent"
    );
}

#[test]
fn an_archived_item_is_omitted_entirely() {
    let items = [CatalogItem {
        status: EntityStatus::Archived,
        ..item(500, "Retired")
    }];
    let menus = [menu(10, None)];
    let placements = [placement(
        10,
        500,
        vec![price(SalesChannel::DineIn, 150_000)],
        true,
    )];

    let book = compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)).expect("compile");
    assert!(
        book.catalog_for(SalesChannel::DineIn)
            .get(item_id(500))
            .is_none(),
        "an archived item is not sold at all"
    );
}

#[test]
fn a_placement_for_an_unknown_item_is_refused() {
    let items = [item(500, "Margherita")];
    let menus = [menu(10, None)];
    let placements = [placement(
        10,
        999,
        vec![price(SalesChannel::DineIn, 1)],
        true,
    )];

    assert_eq!(
        compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)),
        Err(CompileError::UnknownItem(item_id(999)))
    );
}

#[test]
fn an_inheritance_cycle_is_rejected() {
    let items: [CatalogItem; 0] = [];
    let menus = [menu(10, Some(11)), menu(11, Some(10))];
    assert_eq!(
        compile_menu(&items, &menus, &[], &[], &[], menu_id(10)),
        Err(CompileError::InheritanceCycle(menu_id(10)))
    );
}

#[test]
fn an_unknown_menu_is_rejected() {
    let items: [CatalogItem; 0] = [];
    assert_eq!(
        compile_menu(&items, &[], &[], &[], &[], menu_id(42)),
        Err(CompileError::UnknownMenu(menu_id(42)))
    );
}

#[test]
fn compiling_is_deterministic() {
    let items = [item(500, "Margherita"), item(501, "Marinara")];
    let menus = [menu(10, None)];
    let placements = [
        placement(
            10,
            501,
            vec![
                price(SalesChannel::Delivery, 2),
                price(SalesChannel::DineIn, 1),
            ],
            true,
        ),
        placement(10, 500, vec![price(SalesChannel::DineIn, 3)], true),
    ];
    let first = compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)).expect("compile");
    let second = compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)).expect("compile");
    assert_eq!(
        serde_json::to_string(&first).expect("serialise"),
        serde_json::to_string(&second).expect("serialise"),
        "the same authoring compiles to a byte-identical snapshot"
    );
}

fn modifier_group_id(n: u128) -> ModifierGroupId {
    ModifierGroupId::new(Ulid::from_u128(n))
}

fn group(
    id: u128,
    name: &str,
    min_select: u16,
    max_select: u16,
    members: &[u128],
    attached: &[u128],
) -> ModifierGroup {
    ModifierGroup {
        modifier_group_id: modifier_group_id(id),
        tenant_id: tenant(),
        name: name.to_owned(),
        min_select,
        max_select,
        member_item_ids: members.iter().copied().map(item_id).collect(),
        attached_item_ids: attached.iter().copied().map(item_id).collect(),
        status: EntityStatus::Active,
    }
}

/// Two pizzas, both sizes, all four priced dine-in. Enough authoring for every test below to vary
/// one thing and leave the rest alone.
fn pizzas_and_sizes() -> ([CatalogItem; 4], [Menu; 1], Vec<MenuPlacement>) {
    let items = [
        item(500, "Margherita"),
        item(501, "Marinara"),
        item(600, "25cm"),
        item(601, "30cm"),
    ];
    let menus = [menu(10, None)];
    let placements = vec![
        placement(10, 500, vec![price(SalesChannel::DineIn, 150_000)], true),
        placement(10, 501, vec![price(SalesChannel::DineIn, 140_000)], true),
        placement(10, 600, vec![price(SalesChannel::DineIn, 0)], true),
        placement(10, 601, vec![price(SalesChannel::DineIn, 40_000)], true),
    ];
    (items, menus, placements)
}

#[test]
fn a_group_is_inverted_onto_every_item_it_is_attached_to() {
    // ADR-0127 decision 3: the operator pins one group to two pizzas; the till asks the opposite
    // question, so each entry names the group and the catalog carries the rule exactly once.
    let (items, menus, placements) = pizzas_and_sizes();
    let groups = [group(900, "Size", 1, 1, &[600, 601], &[500, 501])];

    let book =
        compile_menu(&items, &menus, &placements, &groups, &[], menu_id(10)).expect("compile");
    let dine_in = book.catalog_for(SalesChannel::DineIn);

    assert_eq!(
        dine_in.modifier_groups().len(),
        1,
        "one rule, not one copy per pizza"
    );
    let published = dine_in.modifier_groups().first().expect("the group");
    assert_eq!(published.modifier_group_id, modifier_group_id(900));
    assert_eq!(published.display_name.as_str(), "Size");
    assert!(published.required(), "min_select 1 is what required means");
    assert_eq!(published.max_select, 1);
    assert_eq!(
        published.member_menu_item_ids,
        vec![item_id(600), item_id(601)]
    );
    assert!(
        published.display_name_translations.is_empty(),
        "authoring has no per-locale group name yet"
    );

    for pizza in [500, 501] {
        assert_eq!(
            dine_in
                .groups_for(item_id(pizza))
                .first()
                .expect("the pizza asks")
                .modifier_group_id,
            modifier_group_id(900)
        );
    }
    assert!(
        dine_in.groups_for(item_id(600)).is_empty(),
        "a size is an ordinary item and attaches nothing"
    );
}

#[test]
fn an_entry_names_its_groups_in_id_order_however_they_were_authored() {
    // The order an operator attached them in is not a fact the book should carry: a re-compile of
    // unchanged authoring has to stay byte-identical, so the inversion sorts.
    let (items, menus, placements) = pizzas_and_sizes();
    let groups = [
        group(902, "Extras", 0, 1, &[601], &[500]),
        group(901, "Size", 1, 1, &[600, 601], &[500]),
    ];

    let book =
        compile_menu(&items, &menus, &placements, &groups, &[], menu_id(10)).expect("compile");
    assert_eq!(
        book.catalog_for(SalesChannel::DineIn)
            .get(item_id(500))
            .expect("the pizza")
            .modifier_group_ids,
        vec![modifier_group_id(901), modifier_group_id(902)]
    );
}

#[test]
fn an_archived_group_is_not_published() {
    // ADR-0127 decision 2: the compiled view has no `status` to reason about, so a retired group
    // simply does not cross — the same posture an archived item gets.
    let (items, menus, placements) = pizzas_and_sizes();
    let mut retired = group(900, "Size", 1, 1, &[600, 601], &[500]);
    retired.status = EntityStatus::Archived;

    let book =
        compile_menu(&items, &menus, &placements, &[retired], &[], menu_id(10)).expect("compile");
    let dine_in = book.catalog_for(SalesChannel::DineIn);

    assert!(dine_in.modifier_groups().is_empty());
    assert!(
        dine_in
            .get(item_id(500))
            .expect("the pizza still sells")
            .modifier_group_ids
            .is_empty(),
        "and no entry is left naming it"
    );
}

#[test]
fn a_member_a_channel_does_not_price_is_not_offered_on_it() {
    // A modifier is an ordinary item, so it is on a channel only if it is priced there. Delivery
    // sells one size; publishing the other would give the till a button it cannot price.
    let items = [
        item(500, "Margherita"),
        item(600, "25cm"),
        item(601, "30cm"),
    ];
    let menus = [menu(10, None)];
    let placements = [
        placement(
            10,
            500,
            vec![
                price(SalesChannel::DineIn, 150_000),
                price(SalesChannel::Delivery, 180_000),
            ],
            true,
        ),
        placement(
            10,
            600,
            vec![
                price(SalesChannel::DineIn, 0),
                price(SalesChannel::Delivery, 0),
            ],
            true,
        ),
        placement(10, 601, vec![price(SalesChannel::DineIn, 40_000)], true),
    ];
    let groups = [group(900, "Size", 1, 1, &[600, 601], &[500])];

    let book =
        compile_menu(&items, &menus, &placements, &groups, &[], menu_id(10)).expect("compile");

    assert_eq!(
        book.catalog_for(SalesChannel::DineIn)
            .modifier_groups()
            .first()
            .expect("dine-in asks")
            .member_menu_item_ids,
        vec![item_id(600), item_id(601)]
    );
    assert_eq!(
        book.catalog_for(SalesChannel::Delivery)
            .modifier_groups()
            .first()
            .expect("delivery still asks")
            .member_menu_item_ids,
        vec![item_id(600)],
        "only the size delivery prices"
    );
}

#[test]
fn a_group_a_channel_cannot_satisfy_is_dropped_there() {
    // Delivery prices the pizza and neither size. Publishing a `min_select = 1` rule with nothing
    // to select would not make the store ask a better question — it would take the pizza off
    // delivery altogether, which is worse than the answer the till gives today.
    let items = [item(500, "Margherita"), item(600, "25cm")];
    let menus = [menu(10, None)];
    let placements = [
        placement(
            10,
            500,
            vec![
                price(SalesChannel::DineIn, 150_000),
                price(SalesChannel::Delivery, 180_000),
            ],
            true,
        ),
        placement(10, 600, vec![price(SalesChannel::DineIn, 0)], true),
    ];
    let groups = [group(900, "Size", 1, 1, &[600], &[500])];

    let book =
        compile_menu(&items, &menus, &placements, &groups, &[], menu_id(10)).expect("compile");

    assert_eq!(
        book.catalog_for(SalesChannel::DineIn)
            .modifier_groups()
            .len(),
        1,
        "dine-in prices a size, so dine-in asks"
    );
    let delivery = book.catalog_for(SalesChannel::Delivery);
    assert!(delivery.modifier_groups().is_empty());
    assert!(
        delivery
            .get(item_id(500))
            .expect("the pizza still sells on delivery")
            .modifier_group_ids
            .is_empty(),
        "the id goes with the rule, so the book stays self-consistent"
    );
}

#[test]
fn a_group_attached_to_nothing_this_menu_sells_is_not_published() {
    // A group is a rule about this menu's items. One pinned only to an item this menu does not
    // place is not part of it, and carrying it down would be noise the till scans past.
    let (items, menus, placements) = pizzas_and_sizes();
    let groups = [group(900, "Size", 1, 1, &[600, 601], &[999])];

    let book =
        compile_menu(&items, &menus, &placements, &groups, &[], menu_id(10)).expect("compile");
    assert!(
        book.catalog_for(SalesChannel::DineIn)
            .modifier_groups()
            .is_empty()
    );
}

#[test]
fn an_optional_group_with_nothing_left_to_offer_is_dropped_too() {
    // `min_select = 0` makes the rule satisfiable by choosing nothing, so the survivor count does
    // not trip the comparison — but a picker with no buttons is still a picker nobody can use.
    let (items, menus, placements) = pizzas_and_sizes();
    let groups = [group(900, "Extras", 0, 2, &[999], &[500])];

    let book =
        compile_menu(&items, &menus, &placements, &groups, &[], menu_id(10)).expect("compile");
    assert!(
        book.catalog_for(SalesChannel::DineIn)
            .modifier_groups()
            .is_empty()
    );
}

fn display_category_id(n: u128) -> DisplayCategoryId {
    DisplayCategoryId::new(Ulid::from_u128(n))
}

fn display_subcategory_id(n: u128) -> DisplaySubcategoryId {
    DisplaySubcategoryId::new(Ulid::from_u128(n))
}

fn display_category(n: u128, name: &str, status: EntityStatus) -> DisplayCategory {
    DisplayCategory {
        display_category_id: display_category_id(n),
        tenant_id: tenant(),
        name: name.to_owned(),
        status,
    }
}

fn display_subcategory(n: u128, parent: u128, name: &str) -> DisplaySubcategory {
    DisplaySubcategory {
        display_subcategory_id: display_subcategory_id(n),
        tenant_id: tenant(),
        display_category_id: display_category_id(parent),
        name: name.to_owned(),
        status: EntityStatus::Active,
    }
}

fn layout_button(
    channel: SalesChannel,
    category: u128,
    subcategory: Option<u128>,
    item: u128,
    label: &str,
    position: Option<GridPosition>,
    sort: i32,
) -> LayoutButton {
    LayoutButton {
        tenant_id: tenant(),
        sales_channel: Open::from_known(channel),
        display_category_id: display_category_id(category),
        display_subcategory_id: subcategory.map(display_subcategory_id),
        menu_item_id: item_id(item),
        label: label.to_owned(),
        position,
        sort,
    }
}

#[test]
fn a_layout_compiles_to_a_per_channel_plan_grouped_by_category_and_subcategory() {
    let categories = [display_category(10, "Pizza", EntityStatus::Active)];
    let subcategories = [display_subcategory(20, 10, "Vegetarian")];
    let buttons = [
        layout_button(
            SalesChannel::DineIn,
            10,
            None,
            500,
            "Margherita",
            Some(GridPosition { column: 0, row: 0 }),
            0,
        ),
        layout_button(
            SalesChannel::DineIn,
            10,
            Some(20),
            501,
            "Marinara",
            Some(GridPosition { column: 1, row: 0 }),
            1,
        ),
    ];

    let book = compile_layout_book(&categories, &subcategories, &buttons);
    let plan = book.plan_for(SalesChannel::DineIn);
    assert_eq!(plan.categories().len(), 1);
    let pizza = plan.categories().first().expect("a category");
    assert_eq!(pizza.name.as_str(), "Pizza");
    assert_eq!(pizza.buttons.len(), 1, "the direct button");
    assert_eq!(pizza.buttons[0].label.as_str(), "Margherita");
    assert_eq!(pizza.subcategories.len(), 1);
    assert_eq!(pizza.subcategories[0].buttons[0].label.as_str(), "Marinara");
    // A channel with no button of its own gets the empty fallback.
    assert!(book.plan_for(SalesChannel::Delivery).is_empty());
}

#[test]
fn a_layout_button_under_an_archived_category_is_skipped() {
    let categories = [display_category(10, "Retired", EntityStatus::Archived)];
    let buttons = [layout_button(
        SalesChannel::DineIn,
        10,
        None,
        500,
        "Ghost",
        None,
        0,
    )];
    let book = compile_layout_book(&categories, &[], &buttons);
    assert!(
        book.plan_for(SalesChannel::DineIn).is_empty(),
        "an archived display category drops its buttons"
    );
}

// --- an archived menu is refused, and a cycle is caught before it is stored ----------------

#[test]
fn an_archived_menu_is_refused_rather_than_compiled_empty() {
    // The asymmetry this closes: archived *items* were already skipped, and an archived menu
    // published anyway. An operator who retires a menu and then publishes it was, until now,
    // shipping it.
    let items = [item(1, "Margherita")];
    let mut retired = menu(100, None);
    retired.status = EntityStatus::Archived;
    let placements = [placement(
        100,
        1,
        vec![price(SalesChannel::DineIn, 100_000)],
        true,
    )];
    assert_eq!(
        compile_menu(&items, &[retired], &placements, &[], &[], menu_id(100)),
        Err(CompileError::ArchivedMenu(menu_id(100)))
    );
}

#[test]
fn an_archived_parent_is_refused_rather_than_silently_dropping_its_prices() {
    // The worse half. The child looks live and compiles; the prices it does not restate come
    // from the parent, so skipping an archived ancestor would take items off a till with
    // nothing said anywhere.
    let items = [item(1, "Margherita"), item(2, "Marinara")];
    let mut parent = menu(100, None);
    parent.status = EntityStatus::Archived;
    let child = menu(101, Some(100));
    let placements = [
        placement(100, 1, vec![price(SalesChannel::DineIn, 100_000)], true),
        placement(101, 2, vec![price(SalesChannel::DineIn, 90_000)], true),
    ];
    assert_eq!(
        compile_menu(
            &items,
            &[parent, child],
            &placements,
            &[],
            &[],
            menu_id(101)
        ),
        Err(CompileError::ArchivedMenu(menu_id(100)))
    );
}

#[test]
fn an_active_chain_still_compiles() {
    // The guard above must not refuse the ordinary case, which is the whole point of the
    // inheritance model: a store special over a brand standard.
    let items = [item(1, "Margherita"), item(2, "Marinara")];
    let parent = menu(100, None);
    let child = menu(101, Some(100));
    let placements = [
        placement(100, 1, vec![price(SalesChannel::DineIn, 100_000)], true),
        placement(101, 2, vec![price(SalesChannel::DineIn, 90_000)], true),
    ];
    let book = compile_menu(
        &items,
        &[parent, child],
        &placements,
        &[],
        &[],
        menu_id(101),
    )
    .expect("an all-active chain compiles");
    for id in [item_id(1), item_id(2)] {
        assert!(
            book.catalog_for(SalesChannel::DineIn).get(id).is_some(),
            "both the parent's and the child's placements reach the book"
        );
    }
}

#[test]
fn re_parenting_onto_a_descendant_is_a_cycle() {
    // 100 <- 101 <- 102. Making 100 inherit from 102 closes the loop.
    let menus = [menu(100, None), menu(101, Some(100)), menu(102, Some(101))];
    assert!(would_cycle(&menus, menu_id(100), menu_id(102)));
    assert!(would_cycle(&menus, menu_id(101), menu_id(102)));
}

#[test]
fn a_menu_proposed_as_its_own_parent_is_a_cycle() {
    let menus = [menu(100, None)];
    assert!(would_cycle(&menus, menu_id(100), menu_id(100)));
}

#[test]
fn re_parenting_onto_an_unrelated_menu_is_not_a_cycle() {
    let menus = [menu(100, None), menu(101, Some(100)), menu(200, None)];
    assert!(!would_cycle(&menus, menu_id(101), menu_id(200)));
}

#[test]
fn a_pre_existing_loop_elsewhere_does_not_hang_the_walk() {
    // 200 and 201 already point at each other — data this check did not exist to prevent. The
    // walk must terminate, and must not blame the edit being made.
    let mut a = menu(200, Some(201));
    let b = menu(201, Some(200));
    a.parent_menu_id = Some(menu_id(201));
    let menus = [menu(100, None), a, b];
    assert!(!would_cycle(&menus, menu_id(100), menu_id(200)));
}

/// The whole point of ADR-0130 part two: a course authored in the console reaches a store.
///
/// Until this, `CatalogItem::course_id` was a column the compiler read past — a store received a
/// price book with no courses in it, so every `course_id` on the wire still pointed at nothing a
/// till could name.
#[test]
fn the_courses_a_menu_serves_reach_the_compiled_book_in_service_order() {
    let items = [
        item_on_course(500, "Margherita", 90),
        item_on_course(501, "Tiramisu", 91),
        item_on_course(502, "Bruschetta", 92),
    ];
    let menus = [menu(10, None)];
    let placements = [
        placement(10, 500, vec![price(SalesChannel::DineIn, 150_000)], true),
        placement(10, 501, vec![price(SalesChannel::DineIn, 80_000)], true),
        placement(10, 502, vec![price(SalesChannel::DineIn, 60_000)], true),
    ];
    // Authored out of sequence on purpose: dessert created first, starter last.
    let courses = [
        course(91, "Dessert", 30),
        course(90, "Main", 20),
        course(92, "Starter", 10),
    ];

    let book =
        compile_menu(&items, &menus, &placements, &[], &courses, menu_id(10)).expect("compile");
    let catalog = book.catalog_for(SalesChannel::DineIn);

    let served: Vec<&str> = catalog
        .courses()
        .iter()
        .map(|course| course.display_name.as_str())
        .collect();
    assert_eq!(
        served,
        ["Starter", "Main", "Dessert"],
        "the book lists courses in service order, not in the order they were authored"
    );
    assert_eq!(
        catalog
            .course_for(item_id(500))
            .expect("the pizza is on a course")
            .display_name
            .as_str(),
        "Main",
        "and an item resolves to the course it declares"
    );
}

/// Two courses a store considers interchangeable may share a position — the schema says so — and
/// the tie has to break somewhere stable, or a re-compile of unchanged authoring reorders the
/// till's headings and republishes a book that means the same thing.
#[test]
fn courses_at_the_same_position_break_the_tie_by_id() {
    let items = [
        item_on_course(500, "Margherita", 91),
        item_on_course(501, "Calzone", 90),
    ];
    let menus = [menu(10, None)];
    let placements = [
        placement(10, 500, vec![price(SalesChannel::DineIn, 150_000)], true),
        placement(10, 501, vec![price(SalesChannel::DineIn, 160_000)], true),
    ];
    let courses = [course(91, "Baked", 20), course(90, "Grilled", 20)];

    let book =
        compile_menu(&items, &menus, &placements, &[], &courses, menu_id(10)).expect("compile");
    let served: Vec<&str> = book
        .catalog_for(SalesChannel::DineIn)
        .courses()
        .iter()
        .map(|course| course.display_name.as_str())
        .collect();
    assert_eq!(
        served,
        ["Grilled", "Baked"],
        "equal sort falls back to the id, so the order is the same on every re-compile"
    );
}

/// An archived course is not published, and the item that still names it is emitted **without a
/// course** rather than refused — the posture an archived modifier group already gets, and the
/// one a store needs: a pizza does not stop being sellable because somebody retired a heading.
#[test]
fn an_archived_course_is_not_published_and_its_items_still_sell() {
    let items = [item_on_course(500, "Margherita", 90)];
    let menus = [menu(10, None)];
    let placements = [placement(
        10,
        500,
        vec![price(SalesChannel::DineIn, 150_000)],
        true,
    )];
    let retired = Course {
        status: EntityStatus::Archived,
        ..course(90, "Main", 20)
    };

    let book =
        compile_menu(&items, &menus, &placements, &[], &[retired], menu_id(10)).expect("compile");
    let catalog = book.catalog_for(SalesChannel::DineIn);

    assert!(
        catalog.courses().is_empty(),
        "the archived course is dropped"
    );
    let entry = catalog.get(item_id(500)).expect("still on the menu");
    assert!(entry.available, "and the item is still for sale");
    assert_eq!(
        entry.course_id, None,
        "the id goes wherever the course went, so no entry names a course the book does not carry"
    );
}

/// A course nothing on this channel goes out on is not published on it. Delivery that prices no
/// dessert should not put an empty "Dessert" heading on the till and leave the till to learn to
/// hide it — the same per-channel reasoning that drops a modifier group with no priced member.
#[test]
fn a_channel_publishes_only_the_courses_it_actually_serves() {
    let items = [
        item_on_course(500, "Margherita", 90),
        item_on_course(501, "Tiramisu", 91),
    ];
    let menus = [menu(10, None)];
    let placements = [
        placement(
            10,
            500,
            vec![
                price(SalesChannel::DineIn, 150_000),
                price(SalesChannel::Delivery, 170_000),
            ],
            true,
        ),
        // Dessert is dine-in only.
        placement(10, 501, vec![price(SalesChannel::DineIn, 80_000)], true),
    ];
    let courses = [course(90, "Main", 20), course(91, "Dessert", 30)];

    let book =
        compile_menu(&items, &menus, &placements, &[], &courses, menu_id(10)).expect("compile");

    assert_eq!(book.catalog_for(SalesChannel::DineIn).courses().len(), 2);
    let delivery: Vec<&str> = book
        .catalog_for(SalesChannel::Delivery)
        .courses()
        .iter()
        .map(|course| course.display_name.as_str())
        .collect();
    assert_eq!(
        delivery,
        ["Main"],
        "delivery prices no dessert, so its book does not carry the heading"
    );
}

/// A course the authoring model does not carry at all — deleted, or never synced — is treated
/// exactly as an archived one, so the two cannot diverge into different behaviour later.
#[test]
fn an_item_naming_a_course_the_tenant_does_not_carry_compiles_without_one() {
    let items = [item_on_course(500, "Margherita", 90)];
    let menus = [menu(10, None)];
    let placements = [placement(
        10,
        500,
        vec![price(SalesChannel::DineIn, 150_000)],
        true,
    )];

    let book = compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)).expect("compile");
    let catalog = book.catalog_for(SalesChannel::DineIn);
    assert!(catalog.courses().is_empty());
    assert_eq!(catalog.get(item_id(500)).expect("priced").course_id, None);
    assert_eq!(
        catalog.course_for(item_id(500)),
        None,
        "and resolving one answers None rather than refusing the read"
    );
}

// --- the compile steps, pinned before `compile_menu` and `compile_layout_book` are split --------

#[test]
fn a_price_on_an_unrecognised_or_unspecified_channel_publishes_nothing() {
    let items = [item(500, "Margherita")];
    let menus = [menu(10, None)];
    let placements = [placement(
        10,
        500,
        vec![
            ChannelPrice {
                sales_channel: Open::parse("SALES_CHANNEL_HOVERCRAFT"),
                unit_price: vnd(1),
            },
            price(SalesChannel::Unspecified, 2),
            price(SalesChannel::DineIn, 3),
        ],
        true,
    )];

    let book = compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)).expect("compile");

    let channels: Vec<SalesChannel> = book
        .channels()
        .iter()
        .map(|channel| channel.sales_channel.known())
        .collect();
    assert_eq!(channels, [SalesChannel::DineIn]);
    assert_eq!(
        book.catalog_for(SalesChannel::DineIn)
            .get(item_id(500))
            .expect("priced dine-in")
            .unit_price,
        vnd(3)
    );
}

#[test]
fn a_cycle_is_refused_before_any_placement_is_read() {
    // The chain is walked first, so a broken chain is what the operator hears about even when a
    // placement also names an item the catalog does not carry.
    let menus = [menu(10, Some(11)), menu(11, Some(10))];
    let placements = [placement(
        10,
        999,
        vec![price(SalesChannel::DineIn, 1)],
        true,
    )];
    assert_eq!(
        compile_menu(&[], &menus, &placements, &[], &[], menu_id(10)),
        Err(CompileError::InheritanceCycle(menu_id(10)))
    );
}

#[test]
fn an_ancestor_three_levels_up_still_prices_what_nothing_nearer_restates() {
    let items = [item(500, "Margherita"), item(501, "Marinara")];
    let menus = [menu(10, Some(11)), menu(11, Some(12)), menu(12, None)];
    let placements = [
        placement(12, 500, vec![price(SalesChannel::DineIn, 100)], true),
        placement(12, 501, vec![price(SalesChannel::DineIn, 200)], true),
        placement(11, 500, vec![price(SalesChannel::DineIn, 110)], true),
        placement(10, 500, vec![price(SalesChannel::DineIn, 120)], false),
    ];
    let book = compile_menu(&items, &menus, &placements, &[], &[], menu_id(10)).expect("compile");
    let dine_in = book.catalog_for(SalesChannel::DineIn);
    let margherita = dine_in.get(item_id(500)).expect("priced");
    assert_eq!(margherita.unit_price, vnd(120), "the requested menu wins");
    assert!(!margherita.available, "and its availability with it");
    assert_eq!(
        dine_in.get(item_id(501)).expect("inherited").unit_price,
        vnd(200)
    );
}

/// A plan rendered one line per category: `Name: direct | Sub: buttons | ...`.
fn rendered(plan: &pos_proto::display::DisplayPlan) -> Vec<String> {
    plan.categories()
        .iter()
        .map(|category| {
            let mut parts = vec![format!(
                "{}: {}",
                category.name.as_str(),
                labels(&category.buttons)
            )];
            for subcategory in &category.subcategories {
                parts.push(format!(
                    "{}: {}",
                    subcategory.name.as_str(),
                    labels(&subcategory.buttons)
                ));
            }
            parts.join(" | ")
        })
        .collect()
}

fn labels(buttons: &[pos_proto::display::DisplayButton]) -> String {
    buttons
        .iter()
        .map(|button| button.label.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn a_layout_orders_buttons_by_sort_then_item_and_groups_by_first_appearance() {
    let categories = [
        display_category(10, "Pizza", EntityStatus::Active),
        display_category(11, "Drinks", EntityStatus::Active),
    ];
    let subcategories = [
        display_subcategory(20, 10, "Red"),
        display_subcategory(21, 10, "White"),
    ];
    let dine_in = |category, subcategory, item, label, sort| {
        layout_button(
            SalesChannel::DineIn,
            category,
            subcategory,
            item,
            label,
            None,
            sort,
        )
    };
    let buttons = [
        dine_in(10, Some(21), 505, "Bianca", 5),
        dine_in(11, None, 600, "Tea", 1),
        dine_in(10, None, 502, "Special", 3),
        dine_in(10, Some(20), 501, "Marinara", 2),
        dine_in(10, Some(20), 500, "Margherita", 2),
        dine_in(10, None, 503, "Calzone", 7),
        layout_button(SalesChannel::Delivery, 11, None, 600, "Tea", None, 0),
    ];

    let book = compile_layout_book(&categories, &subcategories, &buttons);

    // Drinks first: its lowest sort (1) is below Pizza's (2). Margherita before Marinara: the
    // same sort, so the lower item id.
    assert_eq!(
        rendered(book.plan_for(SalesChannel::DineIn)),
        [
            "Drinks: Tea",
            "Pizza: Special,Calzone | Red: Margherita,Marinara | White: Bianca",
        ]
    );
    assert_eq!(
        rendered(book.plan_for(SalesChannel::Delivery)),
        ["Drinks: Tea"]
    );
    let channels: Vec<SalesChannel> = book
        .channels()
        .iter()
        .map(|channel| channel.sales_channel.known())
        .collect();
    assert_eq!(channels.len(), 2, "one plan per channel with a button");
}

#[test]
fn a_layout_skips_every_button_it_cannot_place() {
    let categories = [display_category(10, "Pizza", EntityStatus::Active)];
    let subcategories = [
        display_subcategory(20, 10, "Red"),
        DisplaySubcategory {
            status: EntityStatus::Archived,
            ..display_subcategory(21, 10, "Retired")
        },
        display_subcategory(22, 99, "Someone else's"),
    ];
    let mut unrecognised = layout_button(SalesChannel::DineIn, 10, None, 506, "Future", None, 0);
    unrecognised.sales_channel = Open::parse("SALES_CHANNEL_HOVERCRAFT");
    let buttons = [
        layout_button(SalesChannel::DineIn, 10, Some(20), 500, "Kept", None, 0),
        layout_button(
            SalesChannel::Unspecified,
            10,
            None,
            501,
            "Unspecified",
            None,
            0,
        ),
        unrecognised,
        layout_button(SalesChannel::DineIn, 98, None, 502, "No category", None, 0),
        layout_button(SalesChannel::DineIn, 10, Some(23), 503, "No sub", None, 0),
        layout_button(
            SalesChannel::DineIn,
            10,
            Some(21),
            504,
            "Archived sub",
            None,
            0,
        ),
        layout_button(
            SalesChannel::DineIn,
            10,
            Some(22),
            505,
            "Foreign sub",
            None,
            0,
        ),
    ];

    let book = compile_layout_book(&categories, &subcategories, &buttons);

    assert_eq!(
        rendered(book.plan_for(SalesChannel::DineIn)),
        ["Pizza:  | Red: Kept"]
    );
    assert_eq!(book.channels().len(), 1);
}
