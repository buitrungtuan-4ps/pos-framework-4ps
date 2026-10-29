// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Rasterised text: tickets and receipts drawn where the printer cannot spell, the font size in
//! force, the paper's width and cutter.

use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};

use pos_proto::devices::{DeviceKind, PaperWidth, PublishedDevice, PublishedDevices};
use pos_proto::fees::PublishedFees;
use pos_proto::ids::MenuItemId;
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::printing::{PublishedPrinting, ReceiptLanguage, ReceiptSecondLanguage};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::app::EdgeSession;
use crate::config::ValueSource;
use crate::printing::tests::language::a_bilingual_bill;
use crate::printing::tests::receipts::sold;
use crate::printing::tests::tickets::{a_kitchen, fired_at, oven};
use crate::printing::tests::{
    Recorder, Recorders, STORE, device, event_id, recorder, session_with, store_id, totals_of,
};
use crate::printing::{PrintOutcome, Printers, assumed_capabilities};

/// The fonts a test box renders with, or `None` where the machine running the suite has no
/// font packages — the same condition a store hits, and the reason this is a skip rather than a
/// failure. `pos-render`'s own suite covers the rendering itself.
fn fonts() -> Option<pos_render::TextRenderer> {
    fonts_at(24)
}

/// [`fonts`] loaded at `size` dots per em: the renderer a box whose file set that size built
/// before ADR-0160 decision 6 made the size a setting.
fn fonts_at(size: u16) -> Option<pos_render::TextRenderer> {
    let mut library = pos_render::FontLibrary::new();
    for directory in [
        "/usr/share/fonts/truetype",
        "/usr/share/fonts/opentype",
        "C:\\Windows\\Fonts",
    ] {
        let _ = library.add_directory(std::path::Path::new(directory));
    }
    if library.is_empty() || !library.covers('ở') {
        return None;
    }
    Some(pos_render::TextRenderer::new(
        library,
        NonZeroU16::new(size).expect("positive"),
    ))
}

fn recorder_with_fonts(reachable: bool) -> Option<(Printers, Arc<Recorder>)> {
    let renderer = fonts()?;
    let recorder = Arc::new(Recorder {
        written: Mutex::new(Vec::new()),
        reachable,
    });
    let printers = Printers::over(Arc::new(Recorders {
        recorder: Arc::clone(&recorder),
    }))
    .with_fonts(renderer);
    Some((printers, recorder))
}

#[tokio::test]
async fn a_vietnamese_kitchen_ticket_prints_when_a_font_is_installed() {
    // The whole point of ADR-0102, end to end: the same ticket that is refused above comes out
    // of the printer here, because the line the firmware cannot spell is drawn and sent as a
    // raster instead.
    let Some((printers, recorder)) = recorder_with_fonts(true) else {
        return;
    };
    // A station that sets no language: the dish prints as the menu names it.
    let session = a_kitchen(None, ReceiptLanguage::Display, ReceiptLanguage::Display);
    let outcome = printers
        .print_ticket(
            &session,
            store_id(),
            event_id(1),
            "A1",
            &fired_at(oven(), vec![]),
            None,
        )
        .await;

    assert_eq!(outcome, PrintOutcome::Printed);
    let written = recorder.written.lock().expect("the recorder");
    assert_eq!(written.len(), 1, "one ticket");
    let bytes = written.first().expect("a ticket");
    // `GS v 0` — the raster command. Its presence is what says the diacritics went out as an
    // image rather than as bytes the printer would have mangled.
    assert!(
        bytes
            .windows(4)
            .any(|window| window == [0x1D, 0x76, 0x30, 0x00]),
        "the ticket should carry a raster image"
    );
}

#[tokio::test]
async fn an_ascii_line_still_goes_out_as_text_when_fonts_are_loaded() {
    // Rasterising everything would work and would be wrong: text is a fraction of the bytes and
    // comes out of the head faster, so the code page is still used where it is sufficient.
    let Some((printers, recorder)) = recorder_with_fonts(true) else {
        return;
    };
    let session = session_with(PublishedDevices::new(vec![device(
        2,
        DeviceKind::Printer,
        None,
    )]));
    let outcome = printers
        .print_receipt(
            &session,
            &STORE,
            store_id(),
            event_id(1),
            42,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 99_000)),
            None,
        )
        .await;

    assert_eq!(outcome, PrintOutcome::Printed);
    let written = recorder.written.lock().expect("the recorder");
    let bytes = written.first().expect("a receipt");
    assert!(
        !bytes
            .windows(4)
            .any(|window| window == [0x1D, 0x76, 0x30, 0x00]),
        "an ASCII receipt needs no raster"
    );
}

/// The bytes a Vietnamese kitchen ticket goes out as, from fonts loaded at `loaded`, with `local`
/// this box's own size and `published` the store's `printing.font_size_dots`. `None` where the
/// machine running the suite has no fonts.
async fn ticket_drawn(loaded: u16, local: Option<u16>, published: Option<i64>) -> Option<Vec<u8>> {
    let recorder = Arc::new(Recorder {
        written: Mutex::new(Vec::new()),
        reachable: true,
    });
    let printers = Printers::over(Arc::new(Recorders {
        recorder: Arc::clone(&recorder),
    }))
    .with_fonts(fonts_at(loaded)?)
    .with_local_font_size(local);
    let session = EdgeSession {
        printing: PublishedPrinting {
            font_size_dots: published,
            ..PublishedPrinting::default()
        },
        ..a_kitchen(None, ReceiptLanguage::Display, ReceiptLanguage::Display)
    };
    let outcome = printers
        .print_ticket(
            &session,
            store_id(),
            event_id(1),
            "A1",
            &fired_at(oven(), vec![]),
            None,
        )
        .await;
    assert_eq!(outcome, PrintOutcome::Printed);
    let written = recorder.written.lock().expect("the recorder");
    written.first().cloned()
}

/// A store that publishes a font size has its rasterised lines drawn at it, exactly as a box
/// whose file set that size drew them before the setting; a store that publishes none has them
/// drawn at the box's own size, and at 24 where the box sets none; and a published size out of
/// bounds is not read (ADR-0160 decision 6).
#[tokio::test]
async fn a_rasterised_line_is_drawn_at_the_published_size_then_the_files_then_twenty_four() {
    // What a box printed before the setting: its fonts loaded at its file's size.
    let Some(file_was_32) = ticket_drawn(32, None, None).await else {
        return;
    };
    let file_was_30 = ticket_drawn(30, None, None).await.expect("fonts");
    let neither = ticket_drawn(24, None, None).await.expect("fonts");
    assert_ne!(neither, file_was_32, "the size is on the paper");

    assert_eq!(
        ticket_drawn(24, None, Some(32)).await.expect("fonts"),
        file_was_32,
        "a published 32 draws at 32"
    );
    assert_eq!(
        ticket_drawn(24, Some(30), Some(32)).await.expect("fonts"),
        file_was_32,
        "and wins over the box's own"
    );
    assert_eq!(
        ticket_drawn(24, Some(30), None).await.expect("fonts"),
        file_was_30,
        "nothing published: the box's own size"
    );
    assert_eq!(
        ticket_drawn(24, Some(30), Some(64)).await.expect("fonts"),
        file_was_30,
        "a published size out of bounds is not read"
    );
    assert_eq!(
        ticket_drawn(24, None, Some(12)).await.expect("fonts"),
        neither,
        "and with neither, 24"
    );
}

#[test]
fn the_font_size_in_force_is_the_stores_then_the_boxs_then_the_default() {
    let renderer = pos_render::TextRenderer::new(
        pos_render::FontLibrary::new(),
        NonZeroU16::new(24).expect("positive"),
    );
    let session = |published: Option<i64>| EdgeSession {
        printing: PublishedPrinting {
            font_size_dots: published,
            ..PublishedPrinting::default()
        },
        ..EdgeSession::bootstrap()
    };
    let size = |local: Option<u16>, published: Option<i64>| {
        let (dots, source) = Printers::tcp()
            .with_local_font_size(local)
            .font_size(&session(published), &renderer);
        (dots.get(), source)
    };
    assert_eq!(size(None, Some(32)), (32, ValueSource::Published));
    assert_eq!(size(Some(30), Some(32)), (32, ValueSource::Published));
    assert_eq!(size(Some(30), None), (30, ValueSource::LocalFile));
    assert_eq!(size(Some(30), Some(64)), (30, ValueSource::LocalFile));
    assert_eq!(size(None, None), (24, ValueSource::Default));
    assert_eq!(
        size(Some(0), Some(12)),
        (24, ValueSource::Default),
        "a zero in the file draws nothing, so it is not read"
    );
}

/// A receipt printer and a store printing English under its Vietnamese (ADR-0160 decision 2),
/// whose menu names the pho in English.
fn a_bilingual_store(second: ReceiptSecondLanguage, display: &str) -> EdgeSession {
    let pho = MenuEntry::new(
        MenuItemId::new(Ulid::from_u128(1)),
        DisplayName::new("Phở bò"),
        Money::new(CurrencyCode::VND, 50_000),
        EdgeSession::standard_tax_class(),
    )
    .with_name_translations([("en".to_owned(), DisplayName::new("Beef pho"))].into());
    EdgeSession {
        display_language: Some(display.to_owned()),
        menu: MenuCatalog::new().with(pho),
        printing: PublishedPrinting {
            receipt_second_language: Open::from_known(second),
            ..PublishedPrinting::default()
        },
        ..session_with(PublishedDevices::new(vec![device(
            2,
            DeviceKind::Printer,
            None,
        )]))
    }
}

#[tokio::test]
async fn a_bilingual_receipt_draws_what_its_printer_cannot_spell_and_sends_the_rest_as_text() {
    let Some((printers, recorder)) = recorder_with_fonts(true) else {
        return;
    };
    let session = a_bilingual_store(ReceiptSecondLanguage::English, "vi");
    let (lines, totals) = a_bilingual_bill();
    let outcome = printers
        .print_receipt(
            &session,
            &STORE,
            store_id(),
            event_id(1),
            7,
            &lines,
            &totals,
            None,
        )
        .await;

    assert_eq!(outcome, PrintOutcome::Printed);
    let written = recorder.written.lock().expect("the recorder");
    let bytes = written.first().expect("a receipt");
    // The Vietnamese lines, each label's English beside it included, go out as rasters...
    assert!(
        bytes
            .windows(4)
            .any(|window| window == [0x1D, 0x76, 0x30, 0x00]),
        "a bilingual receipt carries a raster"
    );
    // ...and the English the menu gave the pho, which the printer can spell, as text.
    assert!(bytes.windows(8).any(|window| window == b"Beef pho"));
}

#[tokio::test]
async fn a_box_with_no_fonts_prints_a_bilingual_store_s_receipt_in_one_language_as_before() {
    // Vietnamese labels need fonts this box does not have, so the receipt it prints is the one
    // it printed before the store chose a second language, byte for byte.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let lines = [sold("Beef pho", 1_000, vnd(50_000), vnd(50_000))];
    let totals = totals_of(vnd(50_000));
    let mut written = Vec::new();
    for second in [
        ReceiptSecondLanguage::Vietnamese,
        ReceiptSecondLanguage::None,
    ] {
        let (printers, recorder) = recorder(true);
        let session = a_bilingual_store(second, "en");
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(1),
                7,
                &lines,
                &totals,
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::Printed);
        written.push(recorder.written.lock().expect("the recorder").concat());
    }
    assert_eq!(written[0], written[1]);
}

#[test]
fn a_printer_only_claims_it_prints_bitmaps_when_this_box_can_make_one() {
    // `prints_bitmaps` describes the framework's ability, not the hardware's: `GS v 0` is
    // universal on ESC/POS, so the printer is never the constraint. Claiming it while no font is
    // loaded would mean asking for a document nothing can render.
    let printer = device(1, DeviceKind::Printer, None);
    assert!(!assumed_capabilities(&printer, false).prints_bitmaps);
    assert!(assumed_capabilities(&printer, true).prints_bitmaps);
    // And a printer the console names no paper for is the 80 mm printer every printer was taken
    // to be: 42 characters a line and 576 dots at 203 dpi, and a cutter.
    let assumed = assumed_capabilities(&printer, true);
    assert_eq!(
        (
            assumed.columns.get(),
            assumed.dots_per_line.get(),
            assumed.cuts_paper
        ),
        (42, 576, true)
    );
}

/// The receipt printer, on the paper the console says it takes (ADR-0160 decision 2).
fn on_paper(paper: PaperWidth, cuts_paper: Option<bool>) -> PublishedDevice {
    PublishedDevice {
        paper_width: Open::from_known(paper),
        cuts_paper,
        ..device(2, DeviceKind::Printer, None)
    }
}

#[test]
fn a_printer_is_assumed_to_be_as_wide_and_to_cut_as_the_console_says() {
    let narrow = assumed_capabilities(&on_paper(PaperWidth::Millimetres58, Some(false)), true);
    assert_eq!(
        (
            narrow.columns.get(),
            narrow.dots_per_line.get(),
            narrow.cuts_paper
        ),
        (32, 384, false)
    );
    let laid_out_at_48 =
        assumed_capabilities(&on_paper(PaperWidth::Millimetres80Columns48, None), true);
    assert_eq!(
        (
            laid_out_at_48.columns.get(),
            laid_out_at_48.dots_per_line.get(),
            laid_out_at_48.cuts_paper
        ),
        (48, 576, true)
    );
}

#[tokio::test]
async fn a_58_mm_printer_is_sent_a_384_dot_raster_and_a_bilingual_label_wrapped_at_32() {
    let Some((printers, recorder)) = recorder_with_fonts(true) else {
        return;
    };
    let rules: PublishedFees = serde_json::from_str(
        r#"{"fees": [{"fee_id": "00000000000000000000000001", "code": "SERVICE",
             "display_name": "Phí dịch vụ", "display_name_translations": {"en": "Service charge"}}]}"#,
    )
    .expect("a fees node");
    let (lines, totals) = a_bilingual_bill();
    let mut printed = Vec::new();
    for (seed, paper) in [
        (1, PaperWidth::Millimetres58),
        (2, PaperWidth::Millimetres80),
    ] {
        let session = EdgeSession {
            devices: PublishedDevices::new(vec![on_paper(paper, None)]),
            fees: rules.clone(),
            ..a_bilingual_store(ReceiptSecondLanguage::English, "vi")
        };
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(seed),
                7,
                &lines,
                &totals,
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::Printed);
        let written = recorder.written.lock().expect("the recorder");
        printed.push(written.last().cloned().expect("a receipt"));
    }
    // `GS v 0` names a raster's width in bytes: 384 dots is 48 a row, and 576 is 72.
    let drawn_at = |bytes: &[u8], width: u8| {
        bytes
            .windows(6)
            .any(|window| window == [0x1D, 0x76, 0x30, 0x00, width, 0x00])
    };
    assert!(drawn_at(&printed[0], 48) && !drawn_at(&printed[0], 72));
    assert!(drawn_at(&printed[1], 72) && !drawn_at(&printed[1], 48));
    // The fee and its English share 80 mm paper's line. On 58 mm the English goes under it, and
    // being ASCII it goes as text.
    let says = |bytes: &[u8], text: &[u8]| bytes.windows(text.len()).any(|window| window == text);
    assert!(says(&printed[0], b"  Service charge"));
    assert!(!says(&printed[1], b"  Service charge"));
}

#[tokio::test]
async fn a_printer_with_no_cutter_is_sent_no_cut_and_one_set_as_it_was_prints_as_it_did() {
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let lines = [sold("Beef pho", 1_000, vnd(50_000), vnd(50_000))];
    let totals = totals_of(vnd(50_000));
    let mut printed = Vec::new();
    for printer in [
        device(2, DeviceKind::Printer, None),
        on_paper(PaperWidth::Millimetres80, Some(true)),
        on_paper(PaperWidth::Millimetres80, Some(false)),
    ] {
        let (printers, recorder) = recorder(true);
        let session = session_with(PublishedDevices::new(vec![printer]));
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(1),
                7,
                &lines,
                &totals,
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::Printed);
        printed.push(recorder.written.lock().expect("the recorder").concat());
    }
    // `GS V 0`, the full cut.
    let cuts = |bytes: &[u8]| bytes.windows(3).any(|window| window == [0x1D, 0x56, 0x00]);
    assert!(
        cuts(&printed[0]),
        "a printer nobody set cuts, as every printer did"
    );
    assert_eq!(
        printed[0], printed[1],
        "and one set to what it was taken to be prints byte for byte as it did"
    );
    assert!(
        !cuts(&printed[2]),
        "a printer with no cutter is sent no cut"
    );
}
