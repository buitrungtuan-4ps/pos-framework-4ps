// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::money::{CurrencyCode, Money};

use super::{
    Gstr1Section, PlaceOfSupply, RESTAURANT_SAC, UpiError, gstr1_section, is_valid_hsn_sac,
    place_of_supply, upi_intent,
};

// Fake GSTINs, well formed: Karnataka (29) and Maharashtra (27).
const SELLER: &str = "29ABCDE1234F1Z5";
const BUYER_SAME_STATE: &str = "29PQRSX6789K1Z2";
const BUYER_OTHER_STATE: &str = "27PQRSX6789K1Z2";

fn inr(paise: i64) -> Money {
    Money::new(CurrencyCode::INR, paise)
}

#[test]
fn a_buyer_in_another_state_makes_the_supply_inter_state() {
    assert_eq!(place_of_supply(SELLER, None), PlaceOfSupply::IntraState);
    assert_eq!(
        place_of_supply(SELLER, Some(BUYER_SAME_STATE)),
        PlaceOfSupply::IntraState
    );
    assert_eq!(
        place_of_supply(SELLER, Some(BUYER_OTHER_STATE)),
        PlaceOfSupply::InterState
    );
    assert_eq!(
        place_of_supply(SELLER, Some("27-not-a-gstin")),
        PlaceOfSupply::IntraState,
        "a malformed GSTIN names no state"
    );
}

#[test]
fn an_invoice_is_filed_b2b_b2cl_or_b2cs() {
    let threshold = inr(10_000_000);
    let intra = PlaceOfSupply::IntraState;
    let inter = PlaceOfSupply::InterState;
    assert_eq!(
        gstr1_section(Some(BUYER_OTHER_STATE), inter, inr(50_000), threshold),
        Gstr1Section::B2b
    );
    assert_eq!(
        gstr1_section(None, intra, inr(50_000_000), threshold),
        Gstr1Section::B2cs
    );
    assert_eq!(
        gstr1_section(None, inter, inr(50_000), threshold),
        Gstr1Section::B2cs
    );
    assert_eq!(
        gstr1_section(None, inter, inr(10_000_001), threshold),
        Gstr1Section::B2cl
    );
}

#[test]
fn an_hsn_or_sac_is_four_six_or_eight_digits() {
    assert!(is_valid_hsn_sac(RESTAURANT_SAC));
    assert!(is_valid_hsn_sac("996331"));
    assert!(is_valid_hsn_sac("22030000"));
    assert!(!is_valid_hsn_sac("99633"));
    assert!(!is_valid_hsn_sac("99A3"));
    assert!(!is_valid_hsn_sac(""));
}

#[test]
fn a_upi_request_carries_the_exact_amount_and_the_bills_reference() {
    assert_eq!(
        upi_intent("cafe.demo@bank", inr(52_550), "01J0BILL0000000000000001"),
        Ok("upi://pay?pa=cafe.demo@bank&am=525.50&cu=INR&tr=01J0BILL0000000000000001".to_owned())
    );
    assert_eq!(
        upi_intent("no-at-sign", inr(100), "R1"),
        Err(UpiError::PayeeAddress)
    );
    assert_eq!(
        upi_intent("a b@bank", inr(100), "R1"),
        Err(UpiError::PayeeAddress)
    );
    assert_eq!(upi_intent("cafe@bank", inr(0), "R1"), Err(UpiError::Amount));
    assert_eq!(
        upi_intent("cafe@bank", inr(100), "R-1"),
        Err(UpiError::Reference)
    );
    assert_eq!(
        upi_intent("cafe@bank", inr(100), &"R".repeat(36)),
        Err(UpiError::Reference)
    );
}
