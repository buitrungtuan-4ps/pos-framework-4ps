// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The Vietnamese invoice model (ADR-0225 decisions 1 to 4): symbol, numbering, kind, buyer, totals.

use pos_country_vn::einvoice::{
    BuyerError, InvoiceKind, InvoiceSymbol, IssueMoment, MAX_SEQUENCE, NumberingError,
    RECOMMENDED_ISSUE_MOMENT, RateTotal, SymbolError, TotalsError, check_buyer, check_totals,
    invoice_number, next_number,
};
use pos_ports::fiscalization::{InvoiceBuyer, InvoiceNumber};
use pos_proto::{CalendarDate, SubjectId, Ulid};

#[cfg(test)]
fn day(year: i16, month: u8, date: u8) -> CalendarDate {
    CalendarDate::from_ymd(year, month, date).expect("a real date")
}

#[cfg(test)]
fn symbol(text: &str) -> InvoiceSymbol {
    InvoiceSymbol::parse(text).expect("a well-formed symbol")
}

#[test]
fn a_symbol_is_a_form_digit_then_six_characters() {
    assert_eq!(symbol("1C26MAA").as_str(), "1C26MAA");
    assert_eq!(symbol("2K25TZZ").year(), 2025);
    assert_eq!(InvoiceSymbol::parse(""), Err(SymbolError::Length));
    assert_eq!(InvoiceSymbol::parse("1C26MAAA"), Err(SymbolError::Length));
    assert_eq!(InvoiceSymbol::parse("0C26MAA"), Err(SymbolError::Form));
    assert_eq!(InvoiceSymbol::parse("1X26MAA"), Err(SymbolError::Code));
    assert_eq!(InvoiceSymbol::parse("1C2AMAA"), Err(SymbolError::Year));
    assert_eq!(InvoiceSymbol::parse("1C26maa"), Err(SymbolError::Letters));
    assert_eq!(InvoiceSymbol::parse("1C26M1A"), Err(SymbolError::Letters));
}

#[test]
fn each_issuing_point_gets_its_own_seller_characters() {
    let first = InvoiceSymbol::for_issuing_point(1, 'C', 2026, 'M', 0).expect("AA");
    let second = InvoiceSymbol::for_issuing_point(1, 'C', 2026, 'M', 1).expect("AB");
    let last = InvoiceSymbol::for_issuing_point(1, 'C', 2026, 'M', 675).expect("ZZ");
    assert_eq!(first.as_str(), "1C26MAA");
    assert_eq!(second.as_str(), "1C26MAB");
    assert_eq!(last.as_str(), "1C26MZZ");
    assert_ne!(first, second, "a replacement box never shares a sequence");
    assert_eq!(
        InvoiceSymbol::for_issuing_point(1, 'C', 2026, 'M', 676),
        Err(SymbolError::Letters)
    );
    assert_eq!(
        InvoiceSymbol::for_issuing_point(1, 'C', 1999, 'M', 0),
        Err(SymbolError::Year)
    );
    assert_eq!(
        InvoiceSymbol::for_issuing_point(0, 'C', 2026, 'M', 0),
        Err(SymbolError::Form)
    );
}

#[test]
fn the_sequence_is_gapless_from_one_and_stops_at_eight_digits() {
    let symbol = symbol("1C26MAA");
    let on = day(2026, 11, 24);
    assert_eq!(next_number(&symbol, None, on), Ok(1));
    assert_eq!(next_number(&symbol, Some(1), on), Ok(2));
    assert_eq!(
        next_number(&symbol, Some(MAX_SEQUENCE - 1), on),
        Ok(MAX_SEQUENCE)
    );
    assert_eq!(
        next_number(&symbol, Some(MAX_SEQUENCE), on),
        Err(NumberingError::Exhausted),
        "no ninth digit is invented"
    );
    assert_eq!(
        invoice_number(&symbol, 42),
        InvoiceNumber::new("1C26MAA/00000042")
    );
}

#[test]
fn a_new_year_needs_a_new_symbol() {
    let symbol = symbol("1C26MAA");
    assert_eq!(next_number(&symbol, Some(7), day(2026, 12, 31)), Ok(8));
    assert_eq!(
        next_number(&symbol, Some(8), day(2027, 1, 1)),
        Err(NumberingError::WrongYear),
        "the sequence restarts under the next year's symbol, published by the cloud"
    );
    assert_eq!(
        next_number(&self::symbol("1C27MAA"), None, day(2027, 1, 1)),
        Ok(1)
    );
}

#[test]
fn the_recommended_moment_issues_with_the_settle() {
    assert_eq!(RECOMMENDED_ISSUE_MOMENT, IssueMoment::Immediate);
    assert!(IssueMoment::Immediate.issues(false));
    assert!(IssueMoment::Immediate.issues(true));
    assert!(!IssueMoment::OnRequest.issues(false));
    assert!(IssueMoment::OnRequest.issues(true));
    assert!(!IssueMoment::Off.issues(true));
}

fn buyer(name: &str, tax_code: Option<&str>) -> InvoiceBuyer {
    InvoiceBuyer {
        subject_id: SubjectId::new(Ulid::from_u128(9)),
        buyer_name: name.to_owned(),
        buyer_tax_code: tax_code.map(str::to_owned),
        buyer_email: None,
        buyer_address: None,
    }
}

#[test]
fn a_buyer_needs_a_name_and_a_well_formed_tax_code() {
    assert_eq!(
        check_buyer(&buyer("Cong ty Mau", Some("0101243150"))),
        Ok(())
    );
    assert_eq!(
        check_buyer(&buyer("Cong ty Mau", Some("0101243150-001"))),
        Ok(())
    );
    assert_eq!(check_buyer(&buyer("Nguyen Van Mau", None)), Ok(()));
    assert_eq!(check_buyer(&buyer("   ", None)), Err(BuyerError::NoName));
    assert_eq!(
        check_buyer(&buyer("Cong ty Mau", Some("12345"))),
        Err(BuyerError::TaxCode)
    );
    let error = format!("{:?}", check_buyer(&buyer("Cong ty Mau", Some("12345"))));
    assert!(
        !error.contains("Cong ty") && !error.contains("12345"),
        "{error}"
    );
}

fn rate(rate_basis_points: u32, base: i64, tax: i64) -> RateTotal {
    RateTotal {
        rate_basis_points,
        taxable_base_minor: base,
        tax_minor: tax,
    }
}

#[test]
fn an_invoice_states_each_rate_once_and_adds_up() {
    let original = InvoiceKind::Original;
    // 185,000 at 10 % and 50,000 at 8 %, rounded up by 500 to 258,000.
    let rates = [rate(1_000, 185_000, 18_500), rate(800, 50_000, 4_000)];
    assert_eq!(check_totals(&original, &rates, 500, 258_000), Ok(()));
    assert_eq!(
        check_totals(&original, &rates, 500, 259_000),
        Err(TotalsError::DoesNotSum)
    );
    let repeated = [rate(1_000, 100_000, 10_000), rate(1_000, 85_000, 8_500)];
    assert_eq!(
        check_totals(&original, &repeated, 0, 203_500),
        Err(TotalsError::RepeatedRate)
    );
}

#[test]
fn only_an_adjustment_may_go_down() {
    let down = [rate(1_000, -10_000, -1_000)];
    assert_eq!(
        check_totals(&InvoiceKind::Original, &down, 0, -11_000),
        Err(TotalsError::Negative)
    );
    let corrects = InvoiceNumber::new("1C26MAA/00000001");
    assert_eq!(
        check_totals(
            &InvoiceKind::Replacement {
                corrects: corrects.clone()
            },
            &down,
            0,
            -11_000
        ),
        Err(TotalsError::Negative)
    );
    assert_eq!(
        check_totals(&InvoiceKind::Adjustment { corrects }, &down, 0, -11_000),
        Ok(())
    );
}

#[test]
fn the_pack_accepts_only_its_own_symbol_shape_for_the_cloud() {
    use pos_country::CountryModule;
    let vietnam = pos_country_vn::Vietnam;
    assert!(vietnam.is_valid_invoice_symbol("1C26MAA"));
    assert!(!vietnam.is_valid_invoice_symbol("1C26MA"));
    assert!(!vietnam.is_valid_invoice_symbol("ZZ0001"));
}
