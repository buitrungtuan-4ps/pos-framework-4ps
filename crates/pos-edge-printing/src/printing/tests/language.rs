// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The bilingual receipt: labels paired where they fit, the pre-bill and the copy in both
//! languages, and what a second language names.

use pos_core::billing::BillTotals;
use pos_proto::fees::PublishedFees;
use pos_proto::ids::MenuItemId;
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::printing::{PublishedPrinting, ReceiptLanguage, ReceiptSecondLanguage};
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

use crate::app::{EdgeSession, ReceiptLine};
use crate::paper_labels::{ENGLISH, VIETNAMESE};
use crate::printing::tests::receipts::{a_fee, sold};
use crate::printing::tests::{STORE, lines_of, style, totals_of};
use crate::printing::{
    SecondLanguage, SecondNames, pre_bill_document, receipt_copy_document, receipt_document,
    receipt_lines_in,
};

/// The bill the bilingual cases print: a pho with an egg, which the menu names in English, an
/// iced tea it does not, a service fee whose rule names it in English, and 8 % tax.
pub(super) fn a_bilingual_bill() -> ([ReceiptLine; 2], BillTotals) {
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let lines = [
        ReceiptLine {
            menu_item_id: MenuItemId::new(Ulid::from_u128(1)),
            modifier_display_names: vec![DisplayName::new("Thêm trứng")],
            modifier_menu_item_ids: vec![MenuItemId::new(Ulid::from_u128(3))],
            ..sold("Phở bò", 2_000, vnd(50_000), vnd(100_000))
        },
        ReceiptLine {
            menu_item_id: MenuItemId::new(Ulid::from_u128(2)),
            ..sold("Trà đá", 1_000, vnd(10_000), vnd(10_000))
        },
    ];
    let totals = BillTotals {
        subtotal: vnd(110_000),
        service_charge: vnd(5_500),
        fee_lines: vec![a_fee(1, "SERVICE", "Phí dịch vụ", 5_500)],
        tax_lines: vec![pos_core::billing::TaxLine {
            tax_class_id: EdgeSession::standard_tax_class(),
            taxable_base: vnd(115_500),
            rate_basis_points: 800,
            tax: vnd(9_240),
            components: Vec::new(),
        }],
        tax_total: vnd(9_240),
        ..totals_of(vnd(124_740))
    };
    (lines, totals)
}

/// English as [`a_bilingual_bill`]'s second language, on paper `columns` characters wide.
fn in_english(columns: usize) -> SecondLanguage {
    SecondLanguage {
        labels: &ENGLISH,
        columns,
        rows: vec![
            SecondNames {
                item: Some(DisplayName::new("Beef pho")),
                modifiers: vec![Some(DisplayName::new("Extra egg"))],
            },
            // The iced tea has no English name, so nothing prints under it.
            SecondNames::default(),
        ],
        fees: vec![Some(DisplayName::new("Service charge"))],
    }
}

#[test]
fn a_bilingual_receipt_pairs_a_label_where_it_fits_and_puts_it_under_where_not() {
    // ADR-0160 decision 2, on the three widths a receipt printer is: 58 mm paper's 32
    // characters, and 80 mm paper's 42 and 48.
    let (lines, totals) = a_bilingual_bill();
    let receipt = |columns| -> Vec<String> {
        let second = in_english(columns);
        let document = receipt_document(
            &StoreProfile::default(),
            &style(0),
            &VIETNAMESE,
            7,
            &lines,
            &totals,
            None,
            Some(&second),
        );
        lines_of(&document).into_iter().map(str::to_owned).collect()
    };
    let rows = [
        "#7",
        "Phở bò",
        "  Beef pho",
        "  + Thêm trứng",
        "    Extra egg",
        "  2 x VND 50,000  VND 100,000",
        "Trà đá",
        "  1 x VND 10,000  VND 10,000",
        // Thirty-two characters, so it fits even the narrowest paper, exactly.
        "Tạm tính / Subtotal  VND 110,000",
    ];
    let wide: Vec<&str> = rows
        .into_iter()
        .chain([
            "Phí dịch vụ / Service charge  VND 5,500",
            "Thuế / Tax 8.00%  VND 9,240",
            "VND 124,740",
        ])
        .collect();
    assert_eq!(receipt(48), wide);
    assert_eq!(receipt(42), wide);
    // On 58 mm paper the fee and its English name do not fit on one line, so the English goes
    // under it, and the line above is the one a Vietnamese receipt prints.
    let narrow: Vec<&str> = rows
        .into_iter()
        .chain([
            "Phí dịch vụ  VND 5,500",
            "  Service charge",
            "Thuế / Tax 8.00%  VND 9,240",
            "VND 124,740",
        ])
        .collect();
    assert_eq!(receipt(32), narrow);
}

#[test]
fn a_bilingual_pre_bill_and_copy_head_themselves_in_both_languages() {
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let headings_only = |columns| SecondLanguage {
        rows: Vec::new(),
        fees: Vec::new(),
        ..in_english(columns)
    };
    let pre_bill = |columns| -> Vec<String> {
        let second = headings_only(columns);
        let document = pre_bill_document(
            &StoreProfile::default(),
            &style(0),
            &VIETNAMESE,
            "3K9Q2Z",
            &[],
            &totals_of(vnd(99_000)),
            Some(&second),
        );
        lines_of(&document).into_iter().map(str::to_owned).collect()
    };
    assert_eq!(
        pre_bill(48),
        [
            "PHIẾU TẠM TÍNH / PRE-BILL",
            "3K9Q2Z",
            "Tạm tính / Subtotal  VND 99,000",
            "VND 99,000",
            "Không phải hóa đơn thanh toán / Not a receipt",
        ]
    );
    // Forty-five characters: on 80 mm paper's 42 the English is centred under the Vietnamese.
    assert_eq!(
        pre_bill(42)[4..],
        ["Không phải hóa đơn thanh toán", "Not a receipt"]
    );

    let second = headings_only(32);
    let copy = receipt_copy_document(
        &StoreProfile::default(),
        &style(0),
        &VIETNAMESE,
        7,
        2,
        "09:15",
        &[],
        &totals_of(vnd(99_000)),
        None,
        Some(&second),
    );
    assert_eq!(
        lines_of(&copy)[..3],
        ["#7", "BẢN SAO / COPY", "In lại lần / Reprint 2 - 09:15"]
    );
}

#[test]
fn a_second_language_names_what_the_menu_and_the_fees_rule_translate_and_nothing_else() {
    let entry = |seed: u128, name: &str, english: Option<&str>| {
        let entry = MenuEntry::new(
            MenuItemId::new(Ulid::from_u128(seed)),
            DisplayName::new(name),
            Money::new(CurrencyCode::VND, 50_000),
            EdgeSession::standard_tax_class(),
        );
        match english {
            Some(english) => {
                entry.with_name_translations([("en".to_owned(), DisplayName::new(english))].into())
            }
            None => entry,
        }
    };
    let rules: PublishedFees = serde_json::from_str(
        r#"{"fees": [{"fee_id": "00000000000000000000000001", "code": "SERVICE",
             "display_name": "Phí dịch vụ", "display_name_translations": {"en": "Service charge"}}]}"#,
    )
    .expect("a fees node");
    let second_in = |language| PublishedPrinting {
        receipt_second_language: Open::from_known(language),
        ..PublishedPrinting::default()
    };
    let session = EdgeSession {
        display_language: Some("vi".to_owned()),
        menu: MenuCatalog::new()
            .with(entry(1, "Phở bò", Some("Beef pho")))
            .with(entry(2, "Trà đá", None))
            .with(entry(3, "Thêm trứng", Some("Egg"))),
        fees: rules,
        printing: second_in(ReceiptSecondLanguage::English),
        ..EdgeSession::bootstrap()
    };
    let (lines, totals) = a_bilingual_bill();
    // At the store's own choices, on paper 42 characters wide.
    let second = |session: &EdgeSession, first, rasterise, printed: &[ReceiptLine]| {
        SecondLanguage::for_receipt(
            session, &STORE, first, rasterise, 42, &lines, printed, &totals,
        )
    };
    let english = second(&session, &VIETNAMESE, true, &lines).expect("a bilingual receipt");
    assert_eq!(english.labels, &ENGLISH);
    assert_eq!(
        english.rows,
        [
            SecondNames {
                item: Some(DisplayName::new("Beef pho")),
                modifiers: vec![Some(DisplayName::new("Egg"))],
            },
            SecondNames::default(),
        ]
    );
    assert_eq!(english.fees, [Some(DisplayName::new("Service charge"))]);

    // A receipt in English with Vietnamese under it: the rows were rung up in Vietnamese, so
    // those are its Vietnamese names, printed where the English differs from them.
    let session = EdgeSession {
        printing: PublishedPrinting {
            receipt_language: Open::from_known(ReceiptLanguage::English),
            ..second_in(ReceiptSecondLanguage::Vietnamese)
        },
        ..session
    };
    let printed = receipt_lines_in(&session, &STORE, &lines);
    let vietnamese = second(&session, &ENGLISH, true, &printed).expect("a bilingual receipt");
    assert_eq!(
        vietnamese.rows,
        [
            SecondNames {
                item: Some(DisplayName::new("Phở bò")),
                modifiers: vec![Some(DisplayName::new("Thêm trứng"))],
            },
            // Rung up as "Trà đá" and printed as that, so it prints once.
            SecondNames {
                item: None,
                modifiers: Vec::new(),
            },
        ]
    );

    // One language: on a box with no fonts, which cannot draw Vietnamese labels; where the
    // second language is the receipt's own; and where none is set.
    assert_eq!(second(&session, &ENGLISH, false, &printed), None);
    let same = EdgeSession {
        printing: second_in(ReceiptSecondLanguage::Vietnamese),
        ..session.clone()
    };
    assert_eq!(second(&same, &VIETNAMESE, true, &lines), None);
    let none = EdgeSession {
        printing: PublishedPrinting::default(),
        ..session
    };
    assert_eq!(second(&none, &VIETNAMESE, true, &lines), None);
}
