// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The kitchen's ticket: what it leads with, the guest's note, the menu's names, a half, the
//! station's language and its backup's, and a reference short enough to read.

use pos_ports::printer::PrintBlock;
use pos_proto::devices::{DeviceKind, PublishedDevices};
use pos_proto::floor::{KitchenStation, StationPlan};
use pos_proto::ids::{MenuItemId, StationId};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::printing::ReceiptLanguage;
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::app::{EdgeSession, FiredLine};
use crate::paper_labels::{ENGLISH, PaperLabels, VIETNAMESE};
use crate::printing::tests::{device, event_id, lines_of, recorder, station, store_id};
use crate::printing::{
    PrintOutcome, TicketLine, TicketNote, short_reference, ticket_at, ticket_document,
    ticket_language, ticket_line,
};

#[test]
fn a_ticket_leads_with_the_reference_then_the_item_then_its_modifiers() {
    let document = ticket_document(
        "A1",
        &TicketLine {
            item: "Margherita".to_owned(),
            quantity: "2".to_owned(),
            modifiers: vec!["Extra cheese".to_owned()],
            note: TicketNote::None,
        },
        &ENGLISH,
    );
    assert_eq!(
        lines_of(&document),
        vec!["A1", "2 x Margherita", "  + Extra cheese"]
    );
    assert_eq!(document.blocks.last(), Some(&PrintBlock::Cut));
}

#[test]
fn a_ticket_prints_the_guests_note_last_and_emphasised() {
    let document = ticket_document(
        "A1",
        &TicketLine {
            item: "Margherita".to_owned(),
            quantity: "1".to_owned(),
            modifiers: vec!["Extra cheese".to_owned()],
            note: TicketNote::Text("no peanuts".to_owned()),
        },
        &ENGLISH,
    );
    assert_eq!(
        lines_of(&document),
        vec!["A1", "1 x Margherita", "  + Extra cheese", "  ! no peanuts"]
    );
    let note = document
        .blocks
        .iter()
        .find_map(|block| match block {
            PrintBlock::Text { line, style } if line.contains("no peanuts") => Some(style),
            _ => None,
        })
        .expect("the note is printed");
    assert!(
        note.emphasised,
        "a note about somebody's health is not small print"
    );
}

#[test]
fn a_note_the_edge_lost_is_printed_as_lost_in_the_stores_language() {
    let fired = FiredLine {
        station_id: station(1),
        menu_item_id: MenuItemId::new(Ulid::from_u128(11)),
        quantity: Quantity::ONE,
        modifier_menu_item_ids: Vec::new(),
        note_present: true,
    };
    let line = ticket_line(&EdgeSession::bootstrap(), &fired, None, None);
    assert_eq!(line.note, TicketNote::Lost);
    let english = ticket_document("A1", &line, &ENGLISH);
    assert_eq!(
        lines_of(&english).last().copied(),
        Some("  ! A note was written - ask the server")
    );
    let vietnamese = ticket_document("A1", &line, &VIETNAMESE);
    assert_eq!(
        lines_of(&vietnamese).last().copied(),
        Some("  ! Có ghi chú - hỏi nhân viên phục vụ")
    );

    let unnoted = FiredLine {
        note_present: false,
        ..fired
    };
    assert_eq!(
        ticket_line(&EdgeSession::bootstrap(), &unnoted, None, None).note,
        TicketNote::None
    );
}

#[test]
fn a_ticket_takes_its_names_from_the_published_menu_and_falls_back_to_the_id() {
    // A blank line on a kitchen ticket is how the wrong dish gets made; the identifier at least
    // matches what the KDS is showing.
    let margherita = MenuItemId::new(Ulid::from_u128(11));
    let unknown = MenuItemId::new(Ulid::from_u128(12));
    let session = EdgeSession {
        menu: MenuCatalog::default().with(MenuEntry::new(
            margherita,
            DisplayName::new("Margherita"),
            Money::new(CurrencyCode::VND, 150_000),
            EdgeSession::standard_tax_class(),
        )),
        ..EdgeSession::bootstrap()
    };

    let line = ticket_line(
        &session,
        &FiredLine {
            station_id: station(1),
            menu_item_id: margherita,
            quantity: Quantity::from_milli(2_000),
            modifier_menu_item_ids: vec![unknown],
            note_present: false,
        },
        None,
        None,
    );

    assert_eq!(line.item, "Margherita");
    assert_eq!(line.quantity, "2");
    assert_eq!(line.modifiers, vec![unknown.to_string()]);
}

#[test]
fn half_of_a_split_item_reads_as_a_half_and_not_as_five_hundred() {
    // `Quantity` counts thousandths. A ticket saying "500 x Margherita" is not a rounding bug an
    // operator would ever guess at.
    let session = EdgeSession::bootstrap();
    let half = ticket_line(
        &session,
        &FiredLine {
            station_id: station(1),
            menu_item_id: MenuItemId::new(Ulid::from_u128(11)),
            quantity: Quantity::HALF,
            modifier_menu_item_ids: Vec::new(),
            note_present: false,
        },
        None,
        None,
    );
    assert_eq!(half.quantity, "0.5");
}

/// The oven, which has a printer of its own.
pub(super) fn oven() -> StationId {
    station(1)
}

/// The grill, which has no printer and fails over to the oven.
fn grill() -> StationId {
    station(2)
}

/// The menu's grilled pork, its herbs and its cheese.
fn pork() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(11))
}
fn herbs() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(12))
}
fn cheese() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(13))
}

/// The kitchen the ticket tests print in (ADR-0160 decision 2): the oven, with its printer,
/// printing in `oven_language`, and the grill, printing in `grill_language`, whose tickets fail
/// over to the oven's printer. The menu names its dishes in Vietnamese, the store's display
/// language where `display` says so, and translates the pork and the herbs into English but not
/// the cheese.
pub(super) fn a_kitchen(
    display: Option<&str>,
    oven_language: ReceiptLanguage,
    grill_language: ReceiptLanguage,
) -> EdgeSession {
    let dish = |id: MenuItemId, name: &str, english: Option<&str>| {
        MenuEntry::new(
            id,
            DisplayName::new(name),
            Money::new(CurrencyCode::VND, 50_000),
            EdgeSession::standard_tax_class(),
        )
        .with_name_translations(
            english
                .map(|english| ("en".to_owned(), DisplayName::new(english)))
                .into_iter()
                .collect(),
        )
    };
    let kitchen = |id: StationId, name: &str, backup, language| KitchenStation {
        station_id: id,
        name: DisplayName::new(name),
        backup_station_id: backup,
        late_after_seconds: None,
        ticket_language: Open::from_known(language),
    };
    EdgeSession {
        display_language: display.map(str::to_owned),
        menu: MenuCatalog::new()
            .with(dish(pork(), "Bún chả", Some("Grilled pork with noodles")))
            .with(dish(herbs(), "Thêm rau", Some("Extra herbs")))
            .with(dish(cheese(), "Phô mai", None)),
        devices: PublishedDevices::new(vec![device(3, DeviceKind::Printer, Some(oven()))]),
        stations: StationPlan::from_parts(
            vec![
                kitchen(oven(), "Oven", None, oven_language),
                kitchen(grill(), "Grill", Some(oven()), grill_language),
            ],
            Vec::new(),
            None,
        ),
        ..EdgeSession::bootstrap()
    }
}

/// Two of the grilled pork fired at `at`, with `modifiers`.
pub(super) fn fired_at(at: StationId, modifiers: Vec<MenuItemId>) -> FiredLine {
    FiredLine {
        station_id: at,
        menu_item_id: pork(),
        quantity: Quantity::from_milli(2_000),
        modifier_menu_item_ids: modifiers,
        note_present: false,
    }
}

#[test]
fn a_station_in_english_prints_its_ticket_in_english_while_the_display_language_is_vietnamese() {
    let session = a_kitchen(
        Some("vi"),
        ReceiptLanguage::English,
        ReceiptLanguage::Display,
    );
    let fired = FiredLine {
        note_present: true,
        ..fired_at(oven(), vec![herbs(), cheese()])
    };

    let english = ticket_at(&session, true, oven(), "A1", &fired, None);
    assert_eq!(
        lines_of(&english),
        vec![
            "A1",
            "2 x Grilled pork with noodles",
            "  + Extra herbs",
            // The menu gives the cheese no English name, so it keeps the one it has.
            "  + Phô mai",
            "  ! A note was written - ask the server",
        ]
    );

    // The same line where a station sets no language is the ticket every station printed.
    let display = ticket_at(&session, true, grill(), "A1", &fired, None);
    assert_eq!(
        lines_of(&display),
        vec![
            "A1",
            "2 x Bún chả",
            "  + Thêm rau",
            "  + Phô mai",
            "  ! Có ghi chú - hỏi nhân viên phục vụ",
        ]
    );
}

#[tokio::test]
async fn a_ticket_that_fails_over_prints_in_the_backup_stations_language() {
    // The grill reads Vietnamese and has no printer, so its ticket goes to the oven's, where the
    // cooks read English. On a box with no fonts a Vietnamese name cannot print at all, so the
    // ticket coming out at all says whose language it was printed in.
    let (printers, recorder) = recorder(true);
    let session = a_kitchen(
        Some("vi"),
        ReceiptLanguage::English,
        ReceiptLanguage::Vietnamese,
    );
    assert_eq!(ticket_language(&session, grill()).as_deref(), Some("vi"));
    assert_eq!(ticket_language(&session, oven()).as_deref(), Some("en"));

    let outcome = printers
        .print_ticket(
            &session,
            store_id(),
            event_id(1),
            "A1",
            &fired_at(grill(), vec![herbs()]),
            None,
        )
        .await;

    assert_eq!(outcome, PrintOutcome::Printed);
    let written = recorder.written.lock().expect("the recorder");
    let bytes = written.first().expect("a ticket at the oven");
    let printed = |text: &[u8]| bytes.windows(text.len()).any(|window| window == text);
    assert!(printed(b"2 x Grilled pork with noodles"));
    assert!(printed(b"  + Extra herbs"));
}

#[test]
fn a_stations_ticket_language_is_resolved_as_a_receipts_is() {
    let mut session = a_kitchen(
        Some("en"),
        ReceiptLanguage::Country,
        ReceiptLanguage::Display,
    );
    session.country_language = Some("vi".to_owned());
    assert_eq!(ticket_language(&session, oven()).as_deref(), Some("vi"));
    assert_eq!(ticket_language(&session, grill()).as_deref(), Some("en"));
    // A station the plan does not name prints in the display language, as every ticket did.
    assert_eq!(ticket_language(&session, station(9)).as_deref(), Some("en"));
    // A country whose language the edge has no labels in prints in the display language, as a
    // receipt does, rather than in labels and names from two languages.
    session.country_language = Some("ja".to_owned());
    assert_eq!(ticket_language(&session, oven()).as_deref(), Some("en"));
    // A language from a newer release reads as the display language.
    session.stations = StationPlan::from_parts(
        vec![KitchenStation {
            ticket_language: Open::parse("RECEIPT_LANGUAGE_JA"),
            ..session.stations.stations()[0].clone()
        }],
        Vec::new(),
        None,
    );
    assert_eq!(ticket_language(&session, oven()).as_deref(), Some("en"));
}

#[test]
fn a_station_that_sets_no_language_prints_the_ticket_it_always_did() {
    // What `print_ticket` printed before a station could choose: the menu's names, and the
    // labels of the store's display language, or English on a box that cannot draw them.
    for display in [Some("vi"), Some("en"), None] {
        for can_rasterise in [true, false] {
            let session = a_kitchen(display, ReceiptLanguage::Display, ReceiptLanguage::Display);
            let fired = FiredLine {
                note_present: true,
                ..fired_at(oven(), vec![herbs(), cheese()])
            };
            let before = ticket_document(
                "A1",
                &TicketLine {
                    item: "Bún chả".to_owned(),
                    quantity: "2".to_owned(),
                    modifiers: vec!["Thêm rau".to_owned(), "Phô mai".to_owned()],
                    note: TicketNote::Lost,
                },
                PaperLabels::for_store(display, can_rasterise),
            );
            for printing in [oven(), grill()] {
                assert_eq!(
                    ticket_at(&session, can_rasterise, printing, "A1", &fired, None),
                    before,
                    "{display:?}, fonts {can_rasterise}"
                );
            }
        }
    }
}

#[test]
fn a_ticket_reference_is_short_enough_to_read_across_a_kitchen() {
    assert_eq!(short_reference("01JQZ8N3K7RT9V0XW2YB4C6DEF"), "4C6DEF");
    // Shorter than six is returned whole rather than padded — a test id is not a crash.
    assert_eq!(short_reference("A1"), "A1");
}
