// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The receipt: its language, what was sold, each fee under its own name, the pre-bill and the
//! copy.

use pos_core::billing::{BillTotals, FeeLine};
use pos_ports::printer::PrintBlock;
use pos_proto::fees::{FeeCode, PublishedFees};
use pos_proto::ids::{FeeId, MenuItemId};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::printing::{PublishedPrinting, ReceiptLanguage};
use pos_proto::quantity::Quantity;
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::app::{EdgeSession, ReceiptLine};
use crate::paper_labels::{ENGLISH, PaperLabels, VIETNAMESE};
use crate::printing::tests::{STORE, lines_of, style, totals_of};
use crate::printing::{
    pre_bill_document, printed_totals, receipt_copy_document, receipt_document, receipt_language,
    receipt_lines_in,
};

pub(super) fn sold(name: &str, milli: i64, unit: Money, total: Money) -> ReceiptLine {
    ReceiptLine {
        menu_item_id: MenuItemId::new(Ulid::from_u128(0x17E5)),
        display_name: DisplayName::new(name),
        quantity: Quantity::from_milli(milli),
        unit_price: unit,
        line_total: total,
        modifier_display_names: Vec::new(),
        modifier_menu_item_ids: Vec::new(),
    }
}

fn printing_in(language: ReceiptLanguage) -> PublishedPrinting {
    PublishedPrinting {
        receipt_language: Open::from_known(language),
        ..PublishedPrinting::default()
    }
}

#[test]
fn a_receipt_follows_the_display_language_until_the_store_chooses_its_own() {
    let mut session = EdgeSession {
        display_language: Some("vi".to_owned()),
        ..EdgeSession::bootstrap()
    };
    assert_eq!(receipt_language(&session, &STORE).as_deref(), Some("vi"));
    session.printing = printing_in(ReceiptLanguage::English);
    assert_eq!(receipt_language(&session, &STORE).as_deref(), Some("en"));
    session.printing = printing_in(ReceiptLanguage::Country);
    assert_eq!(
        receipt_language(&session, &STORE).as_deref(),
        Some("vi"),
        "a store whose locale names no country language prints in its display language"
    );
    session.country_language = Some("ja".to_owned());
    assert_eq!(
        receipt_language(&session, &STORE).as_deref(),
        Some("vi"),
        "and so does one whose country's language the edge has no labels in"
    );
    session.display_language = Some("en".to_owned());
    session.country_language = Some("vi".to_owned());
    let language = receipt_language(&session, &STORE);
    assert_eq!(language.as_deref(), Some("vi"), "whatever the tills show");
    assert_eq!(
        PaperLabels::for_store(language.as_deref(), true),
        &VIETNAMESE
    );
    // No fonts on this box: a label is never the line that stops a receipt printing.
    assert_eq!(PaperLabels::for_store(language.as_deref(), false), &ENGLISH);
}

#[test]
fn a_receipt_in_another_language_names_what_the_menu_translates_and_keeps_the_rest() {
    let item = |seed: u128| MenuItemId::new(Ulid::from_u128(seed));
    let entry = |seed: u128, name: &str, english: Option<&str>| {
        let entry = MenuEntry::new(
            item(seed),
            DisplayName::new(name),
            Money::new(CurrencyCode::VND, 65_000),
            EdgeSession::standard_tax_class(),
        );
        match english {
            Some(english) => {
                entry.with_name_translations([("en".to_owned(), DisplayName::new(english))].into())
            }
            None => entry,
        }
    };
    // The pho was renamed after it was rung up; the rice has no English name.
    let session = EdgeSession {
        display_language: Some("vi".to_owned()),
        menu: MenuCatalog::new()
            .with(entry(1, "Phở bò đặc biệt", Some("Beef pho")))
            .with(entry(2, "Cơm tấm", None))
            .with(entry(3, "Trứng", Some("Egg"))),
        printing: printing_in(ReceiptLanguage::English),
        ..EdgeSession::bootstrap()
    };
    let price = Money::new(CurrencyCode::VND, 65_000);
    let lines = [
        ReceiptLine {
            menu_item_id: item(1),
            modifier_display_names: vec![DisplayName::new("Trứng")],
            modifier_menu_item_ids: vec![item(3)],
            ..sold("Phở bò", 1_000, price, price)
        },
        ReceiptLine {
            menu_item_id: item(2),
            ..sold("Cơm tấm", 1_000, price, price)
        },
    ];

    let english = receipt_lines_in(&session, &STORE, &lines);
    assert_eq!(english[0].display_name.as_str(), "Beef pho");
    assert_eq!(english[0].modifier_display_names[0].as_str(), "Egg");
    assert_eq!(
        english[1].display_name.as_str(),
        "Cơm tấm",
        "the name it was rung up as"
    );

    // In the display language a receipt prints the names its lines captured, as it always did.
    let as_before = EdgeSession {
        printing: PublishedPrinting::default(),
        ..session
    };
    assert!(matches!(
        receipt_lines_in(&as_before, &STORE, &lines),
        std::borrow::Cow::Borrowed(same) if same == lines
    ));
}

#[test]
fn a_receipt_lists_what_was_sold() {
    // ADR-0129 decision 1, and the shape Decree 123/2020 Art. 10 asks for: the name, then how
    // many at what each, with the extended amount. The rows come before the totals and sum to
    // the subtotal printed under them.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let lines = [
        sold("Phở bò đặc biệt", 2_000, vnd(99_000), vnd(198_000)),
        sold("Margherita", 1_000, vnd(149_000), vnd(149_000)),
    ];

    let document = receipt_document(
        &StoreProfile::default(),
        // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
        &style(0),
        &ENGLISH,
        7,
        &lines,
        &totals_of(vnd(347_000)),
        None,
        None,
    );

    assert_eq!(
        lines_of(&document),
        vec![
            "#7",
            "Phở bò đặc biệt",
            "  2 x VND 99,000  VND 198,000",
            "Margherita",
            "  1 x VND 149,000  VND 149,000",
            "Subtotal  VND 347,000",
            "VND 347,000",
        ]
    );
}

/// A fee as `assemble` reports one, under the name its bill froze.
pub(super) fn a_fee(seed: u128, code: &str, name: &str, minor: i64) -> FeeLine {
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    FeeLine {
        fee_id: FeeId::new(Ulid::from_u128(seed)),
        code: FeeCode::new(code),
        display_name: DisplayName::new(name),
        amount: vnd(minor),
        class_shares: Vec::new(),
        tax: vnd(0),
        tax_components: Vec::new(),
        waivable: false,
    }
}

#[test]
fn a_receipt_prints_each_fee_under_its_own_name_and_no_service_charge_line() {
    // ADR-0159 decision 4. The fees take the place of the one service-charge line their sum
    // used to print under, in the order of their rules; a fee that charged nothing is not
    // printed, as a discount of nothing is not.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let totals = BillTotals {
        subtotal: vnd(100_000),
        service_charge: vnd(15_000),
        fee_lines: vec![
            a_fee(1, "SERVICE", "Service charge", 5_000),
            a_fee(2, "COVER", "Cover charge", 10_000),
            a_fee(3, "BAG", "Bag", 0),
        ],
        ..totals_of(vnd(115_000))
    };
    let document = receipt_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        7,
        &[],
        &totals,
        None,
        None,
    );
    assert_eq!(
        lines_of(&document),
        vec![
            "#7",
            "Subtotal  VND 100,000",
            "Service charge  VND 5,000",
            "Cover charge  VND 10,000",
            "VND 115,000",
        ]
    );

    // A bill with no fee line prints its service charge as every receipt did.
    let lump = BillTotals {
        fee_lines: Vec::new(),
        ..totals
    };
    let document = receipt_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        7,
        &[],
        &lump,
        None,
        None,
    );
    assert!(lines_of(&document).contains(&"Service charge  VND 15,000"));
}

#[test]
fn a_fee_prints_in_the_receipts_language_where_its_rule_translates_it() {
    let rules: PublishedFees = serde_json::from_str(
        r#"{"fees": [
            {"fee_id": "00000000000000000000000001", "code": "SERVICE",
             "display_name": "Service fee", "display_name_translations": {"vi": "Phí phục vụ"}},
            {"fee_id": "00000000000000000000000002", "code": "COVER",
             "display_name": "Cover"}
        ]}"#,
    )
    .expect("a fees node");
    let mut session = EdgeSession {
        display_language: Some("vi".to_owned()),
        fees: rules,
        ..EdgeSession::bootstrap()
    };
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    // The bill froze the rule's own names; the service fee was renamed since, the cover is not
    // translated, and a third rule is no longer published.
    let totals = BillTotals {
        fee_lines: vec![
            a_fee(1, "SERVICE", "Service charge", 5_000),
            a_fee(2, "COVER", "Cover charge", 10_000),
            a_fee(3, "BAG", "Bag", 2_000),
        ],
        ..totals_of(vnd(117_000))
    };
    let names = |totals: &BillTotals| -> Vec<String> {
        totals
            .fee_lines
            .iter()
            .map(|fee| fee.display_name.as_str().to_owned())
            .collect()
    };
    assert_eq!(
        names(&printed_totals(&session, &STORE, &totals)),
        ["Phí phục vụ", "Cover charge", "Bag"]
    );
    // In English nothing is translated, so the receipt prints the names the bill froze.
    session.printing = printing_in(ReceiptLanguage::English);
    assert!(matches!(
        printed_totals(&session, &STORE, &totals),
        std::borrow::Cow::Borrowed(same) if same == &totals
    ));
}

#[test]
fn a_pre_bill_is_the_receipts_rows_under_a_reference_and_no_number() {
    // Roadmap-v3 B2.1. The same rows and figures the receipt will print, so the guest checks
    // what they will pay; headed as a pre-bill under the order's reference, with no receipt
    // number (nothing is settled, and a number here would leave a hole in the gapless series);
    // and signed off "Not a receipt", because on the table it looks like one.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let lines = [
        sold("Phở bò đặc biệt", 2_000, vnd(99_000), vnd(198_000)),
        sold("Margherita", 1_000, vnd(149_000), vnd(149_000)),
    ];

    let document = pre_bill_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        "3K9Q2Z",
        &lines,
        &totals_of(vnd(347_000)),
        None,
    );

    assert_eq!(
        lines_of(&document),
        vec![
            "PRE-BILL",
            "3K9Q2Z",
            "Phở bò đặc biệt",
            "  2 x VND 99,000  VND 198,000",
            "Margherita",
            "  1 x VND 149,000  VND 149,000",
            "Subtotal  VND 347,000",
            "VND 347,000",
            "Not a receipt",
        ]
    );
    assert!(
        !lines_of(&document).iter().any(|line| line.starts_with('#')),
        "a pre-bill carries no receipt number"
    );
    assert!(
        matches!(
            document.blocks.first(),
            Some(PrintBlock::Text { line, style })
                if line == "PRE-BILL" && style.emphasised && style.centred
        ),
        "the heading is centred in bold"
    );
    assert_eq!(document.blocks.last(), Some(&PrintBlock::Cut));
}

#[test]
fn a_copy_is_the_receipt_under_its_own_number_marked_as_a_copy() {
    // ADR-0164 decision 5. The same rows and figures under the same number, so the copy matches
    // the paper the guest already holds; under the number, COPY in bold and which copy it is,
    // so a copy taken away is never read as a second sale.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let lines = [sold("Margherita", 1_000, vnd(149_000), vnd(149_000))];
    let totals = totals_of(vnd(149_000));
    let copy = |labels| {
        receipt_copy_document(
            &StoreProfile::default(),
            &style(0),
            labels,
            42,
            2,
            "2026-09-30 19:05",
            &lines,
            &totals,
            None,
            None,
        )
    };

    let english = copy(&ENGLISH);
    assert_eq!(
        lines_of(&english),
        vec![
            "#42",
            "COPY",
            "Reprint 2 - 2026-09-30 19:05",
            "Margherita",
            "  1 x VND 149,000  VND 149,000",
            "Subtotal  VND 149,000",
            "VND 149,000",
        ]
    );
    assert!(
        english.blocks.iter().any(|block| matches!(
            block,
            PrintBlock::Text { line, style } if line == "COPY" && style.emphasised
        )),
        "COPY is printed in bold"
    );
    let original = receipt_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        42,
        &lines,
        &totals,
        None,
        None,
    );
    let mut unmarked = lines_of(&english);
    unmarked.retain(|line| *line != "COPY" && !line.starts_with("Reprint "));
    assert_eq!(
        unmarked,
        lines_of(&original),
        "apart from the marking, the copy is the original"
    );
    assert_eq!(english.blocks.last(), Some(&PrintBlock::Cut));

    let vietnamese = copy(&VIETNAMESE);
    assert_eq!(
        lines_of(&vietnamese)[..3],
        ["#42", "BẢN SAO", "In lại lần 2 - 2026-09-30 19:05"]
    );
}

#[test]
fn a_copy_without_its_per_rate_lines_prints_the_recorded_tax_as_one_figure() {
    // ADR-0164 decision 5: when the rates in force no longer give the tax the settle recorded,
    // the copy prints that tax as one figure rather than a line at a rate that did not apply,
    // and the paper still adds up to what the guest paid.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let lines = [sold("Margherita", 1_000, vnd(150_000), vnd(150_000))];
    let totals = BillTotals {
        tax_total: vnd(15_000),
        total_due: vnd(165_000),
        ..totals_of(vnd(150_000))
    };

    let document = receipt_copy_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        7,
        1,
        "2026-09-30 19:05",
        &lines,
        &totals,
        None,
        None,
    );
    assert_eq!(
        lines_of(&document)[5..],
        ["Subtotal  VND 150,000", "Tax  VND 15,000", "VND 165,000"]
    );
}
