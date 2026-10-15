// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A receipt's layout: what each line was made with, the store's name and cut, the buyer block, tax
//! by component, numbers grouped the store's way, and a registration label with no number.

use pos_core::billing::BillTotals;
use pos_ports::printer::PrintBlock;
use pos_proto::locale::NumberFormat;
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

use crate::app::BuyerDetails;
use crate::paper_labels::ENGLISH;
use crate::printing::tests::receipts::sold;
use crate::printing::tests::{lines_of, style, totals_of};
use crate::printing::{MoneyStyle, receipt_document};

#[test]
fn a_receipt_prints_what_each_line_was_made_with() {
    // ADR-0144: the names the line recorded, under the item and above its amount, whose unit
    // price already includes them. A line recorded before the event carried names has none, and
    // prints exactly what it printed before.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let mut pizza = sold("Margherita", 1_000, vnd(189_000), vnd(189_000));
    pizza.modifier_display_names = vec![DisplayName::new("30 cm"), DisplayName::new("Burrata")];
    let lines = [pizza, sold("Phở bò", 1_000, vnd(99_000), vnd(99_000))];

    let document = receipt_document(
        &StoreProfile::default(),
        // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
        &style(0),
        &ENGLISH,
        9,
        &lines,
        &totals_of(vnd(288_000)),
        None,
        None,
    );

    assert_eq!(
        lines_of(&document),
        vec![
            "#9",
            "Margherita",
            "  + 30 cm",
            "  + Burrata",
            "  1 x VND 189,000  VND 189,000",
            "Phở bò",
            "  1 x VND 99,000  VND 99,000",
            "Subtotal  VND 288,000",
            "VND 288,000",
        ]
    );
}

#[test]
fn a_half_prints_as_a_half_and_not_as_five_hundred() {
    // `Quantity` counts thousandths, so the raw figure for one half is 500. A receipt reading
    // "500 x" is a rounding bug nobody would guess at from the paper, which is why the rows
    // share `format_quantity` with the kitchen ticket rather than printing the integer.
    let jpy = |minor| Money::new(CurrencyCode::JPY, minor);
    let lines = [sold("Hokkaido melon", 500, jpy(4_000), jpy(2_000))];

    let document = receipt_document(
        &StoreProfile::default(),
        // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
        &style(0),
        &ENGLISH,
        8,
        &lines,
        &totals_of(jpy(2_000)),
        None,
        None,
    );

    assert!(
        lines_of(&document).contains(&"  0.5 x JPY 4,000  JPY 2,000"),
        "one half of an item is half of its unit price"
    );
}

#[test]
fn a_store_that_has_filled_nothing_in_prints_what_it_always_printed() {
    // The never-blank rule applied to a legal document: an empty label reads as a value somebody
    // forgot to type, so an unfilled profile shortens the receipt rather than padding it
    // (ADR-0106).
    let document = receipt_document(
        &StoreProfile::default(),
        // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
        &style(0),
        &ENGLISH,
        7,
        &[],
        &totals_of(Money::new(CurrencyCode::VND, 50_000)),
        None,
        None,
    );
    assert_eq!(
        lines_of(&document),
        vec!["#7", "Subtotal  VND 50,000", "VND 50,000"]
    );
}

#[test]
fn a_receipt_names_the_store_and_ends_in_a_cut() {
    let profile = StoreProfile {
        legal_name: "Pizza 4P's Vietnam Co., Ltd".to_owned(),
        trading_name: Some("Bến Thành".to_owned()),
        address_lines: vec!["8 Thủ Khoa Huân, Quận 1".to_owned()],
        tax_registration_number: Some("0101243150".to_owned()),
        tax_registration_label: Some("MST".to_owned()),
        contact_lines: vec!["028 3822 9838".to_owned()],
        footer_lines: vec!["Cảm ơn quý khách".to_owned()],
    };
    let document = receipt_document(
        &profile,
        // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
        &style(0),
        &ENGLISH,
        42,
        &[],
        &totals_of(Money::new(CurrencyCode::VND, 99_000)),
        None,
        None,
    );
    assert_eq!(
        lines_of(&document),
        vec![
            "Bến Thành",
            "8 Thủ Khoa Huân, Quận 1",
            "MST: 0101243150",
            "#42",
            "Subtotal  VND 99,000",
            "VND 99,000",
            "028 3822 9838",
            "Cảm ơn quý khách",
        ]
    );
    assert_eq!(
        document.blocks.last(),
        Some(&PrintBlock::Cut),
        "the paper is cut, or the next receipt starts on this one"
    );
}

#[test]
fn a_b2b_invoice_names_its_buyer_between_the_seller_and_the_figures() {
    // Without the buyer's name and registration number a Japanese qualified invoice does not
    // let the buyer claim input tax, which is the whole reason a business asks for one
    // (ADR-0107). The block sits after the seller's and before the money, which is the order
    // both Japan's qualified invoice and India's Rule 46 read in.
    let profile = StoreProfile {
        legal_name: "Pizza 4P's Japan".to_owned(),
        tax_registration_number: Some("T9876543210987".to_owned()),
        tax_registration_label: Some("登録番号".to_owned()),
        ..StoreProfile::default()
    };
    let buyer = BuyerDetails {
        name: "Kabushiki Kaisha Reiwa".to_owned(),
        tax_code: Some("T1234567890123".to_owned()),
        address: Some("1-1 Marunouchi, Chiyoda".to_owned()),
        email: Some("accounts@example.co.jp".to_owned()),
    };
    let document = receipt_document(
        &profile,
        // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
        &style(0),
        &ENGLISH,
        11,
        &[],
        &totals_of(Money::new(CurrencyCode::JPY, 1_100)),
        Some(&buyer),
        None,
    );
    let lines = lines_of(&document);
    assert!(
        lines.contains(&"Bill to: Kabushiki Kaisha Reiwa"),
        "the buyer is named: {lines:?}"
    );
    assert!(lines.contains(&"T1234567890123"));
    assert!(lines.contains(&"1-1 Marunouchi, Chiyoda"));
    assert!(
        !lines.iter().any(|line| line.contains("example.co.jp")),
        "an email is where to send the copy, not part of the document"
    );
}

#[test]
fn a_retail_receipt_has_no_buyer_block_at_all() {
    // The overwhelming majority of bills. A blank "Bill to:" on a receipt reads as a value
    // somebody forgot to type, so the block is absent rather than empty.
    let document = receipt_document(
        &StoreProfile::default(),
        // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
        &style(0),
        &ENGLISH,
        12,
        &[],
        &totals_of(Money::new(CurrencyCode::VND, 50_000)),
        None,
        None,
    );
    assert!(
        !lines_of(&document)
            .iter()
            .any(|line| line.starts_with("Bill to")),
        "an ordinary sale prints the receipt it always did"
    );
}

#[test]
fn an_indian_receipt_prints_cgst_and_sgst_under_the_rate_they_explain() {
    // The document ADR-0104 exists for: the halves go to different governments, so an invoice
    // printing their sum is not a lesser rendering of the same fact — it is not a valid invoice.
    use pos_core::billing::{TaxComponentLine, TaxLine};
    use pos_proto::ids::TaxClassId;
    use pos_proto::locale::TaxComponentName;

    let inr = CurrencyCode::parse("INR").expect("INR is three upper-case letters");
    let money = |amount| Money::new(inr, amount);
    let totals = BillTotals {
        subtotal: money(100_000),
        discount_total: money(0),
        comp_total: money(0),
        service_charge: money(0),
        fee_lines: Vec::new(),
        tax_lines: vec![TaxLine {
            tax_class_id: TaxClassId::new(Ulid::from_u128(1)),
            taxable_base: money(100_000),
            rate_basis_points: 500,
            tax: money(5_000),
            components: vec![
                TaxComponentLine {
                    name: TaxComponentName::new("CGST").expect("a token"),
                    rate_basis_points: 250,
                    tax: money(2_500),
                },
                TaxComponentLine {
                    name: TaxComponentName::new("SGST").expect("a token"),
                    rate_basis_points: 250,
                    tax: money(2_500),
                },
            ],
        }],
        tax_total: money(5_000),
        rounding_adjustment: money(-100),
        total_due: money(104_900),
    };
    let profile = StoreProfile {
        legal_name: "Pizza 4P's India".to_owned(),
        tax_registration_number: Some("29ABCDE1234F1Z5".to_owned()),
        tax_registration_label: Some("GSTIN".to_owned()),
        ..StoreProfile::default()
    };
    // Two, because this fixture is denominated in **paise** and the rupee has a hundred of them
    // (ADR-0134). This is the receipt that record is about: every figure below used to print as
    // its own minor units, so a guest who paid ₹1,049.00 was handed a document reading
    // `INR 104900` — on the one piece of paper India's Rule 46 makes a tax invoice.
    //
    // The rounding line is the other half of why the exponent is not the same fact as the
    // increment: the invoice still rounds to the whole rupee, which is what `-1.00` is, while
    // the figures it rounds are quoted in paise.
    let document = receipt_document(&profile, &style(2), &ENGLISH, 9, &[], &totals, None, None);
    assert_eq!(
        lines_of(&document),
        vec![
            "Pizza 4P's India",
            "GSTIN: 29ABCDE1234F1Z5",
            "#9",
            "Subtotal  INR 1,000.00",
            "Tax 5.00%  INR 50.00",
            "  CGST 2.50%  INR 25.00",
            "  SGST 2.50%  INR 25.00",
            "Rounding  INR -1.00",
            "INR 1,049.00",
        ]
    );
}

#[test]
fn a_vietnamese_receipt_is_grouped_the_way_vietnam_writes_a_number() {
    // ADR-0136. The figures were already right after ADR-0134 — the đồng has no subunit, so the
    // arithmetic was never the problem here — but they were *spelled* `VND 1,234,567`, which is
    // not how anyone in the country writes a number. The store's own pack says `.` groups and
    // `,` points, and now the paper says it too.
    let profile = StoreProfile {
        legal_name: "Pizza 4P's Bến Thành".to_owned(),
        ..StoreProfile::default()
    };
    let vietnam = MoneyStyle {
        // The đồng has no subunit, so nothing here exercises the decimal mark. The mark is still
        // published and still differs from the default, which is why the two-decimal case below
        // exists: a store on a fractional currency in a comma-decimal country is where getting
        // only the group separator right would still print a figure nobody writes.
        exponent: 0,
        format: NumberFormat {
            decimal_separator: ',',
            group_separator: '.',
            digits_per_group: 3,
        },
    };
    let document = receipt_document(
        &profile,
        &vietnam,
        &ENGLISH,
        5,
        &[],
        &totals_of(Money::new(CurrencyCode::VND, 1_234_567)),
        None,
        None,
    );
    assert!(
        lines_of(&document).contains(&"VND 1.234.567"),
        "the total is grouped the way the store's country writes numbers: {:?}",
        lines_of(&document)
    );
}

#[test]
fn both_marks_are_the_store_s_own_where_the_currency_has_a_fraction() {
    // The case a group-separator-only fix would still get wrong: two decimals *and* a country
    // that points with a comma. `1.234,56` is one figure; `1.234.56` and `1,234,56` are both
    // nonsense, and each is what you get from honouring one mark and not the other.
    let style = MoneyStyle {
        exponent: 2,
        format: NumberFormat {
            decimal_separator: ',',
            group_separator: '.',
            digits_per_group: 3,
        },
    };
    let document = receipt_document(
        &StoreProfile::default(),
        &style,
        &ENGLISH,
        6,
        &[],
        &totals_of(Money::new(CurrencyCode::USD, 123_456)),
        None,
        None,
    );
    assert!(
        lines_of(&document).contains(&"USD 1.234,56"),
        "both marks come from the store: {:?}",
        lines_of(&document)
    );
}

#[test]
fn a_registration_label_with_no_number_prints_no_line() {
    // A label alone reads as a number somebody forgot to type, which on a tax invoice is worse
    // than a shorter receipt.
    let profile = StoreProfile {
        legal_name: "Pizza 4P's Ginza".to_owned(),
        tax_registration_label: Some("登録番号".to_owned()),
        ..StoreProfile::default()
    };
    let document = receipt_document(
        &profile,
        // Zero: these fixtures are denominated in đồng and in yen, and neither has a subunit.
        &style(0),
        &ENGLISH,
        3,
        &[],
        &totals_of(Money::new(CurrencyCode::JPY, 1_100)),
        None,
        None,
    );
    assert!(
        !lines_of(&document)
            .iter()
            .any(|line| line.contains("登録番号")),
        "a label with nothing after it is not printed"
    );
}
