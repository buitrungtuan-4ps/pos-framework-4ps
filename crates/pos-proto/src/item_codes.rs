// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a till scans or types to find an item, and an open price's bounds
//! ([ADR-0222](../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decisions 1 and 4).
//!
//! An item carries up to [`BARCODES_MAX`] barcodes, each a GTIN-8, -12, -13 or -14 whose check
//! digit is right, and at most one PLU of one to five digits. Each is unique within a menu; the
//! cloud refuses a duplicate at the write, naming both items. An open-price item is sold at a price
//! the cashier types within its published bounds. These are fixed data-integrity rules, so they live
//! here, once, for the cloud that refuses and the till that indexes.

use serde::{Deserialize, Serialize};

use crate::money::Money;

/// The most barcodes one item carries.
pub const BARCODES_MAX: usize = 8;

/// The digit counts a GTIN comes in: GTIN-8, -12, -13 and -14.
const GTIN_LENGTHS: [usize; 4] = [8, 12, 13, 14];

/// The longest PLU, in digits.
const PLU_DIGITS_MAX: usize = 5;

/// The bounds a cashier types an open-price item's price within, both inclusive and above zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OpenPrice {
    /// The least the price may be.
    pub minimum: Money,
    /// The most the price may be.
    pub maximum: Money,
}

impl OpenPrice {
    /// Whether these bounds can be sold within: both above zero, one currency, and the least no
    /// more than the most.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.minimum.currency_code == self.maximum.currency_code
            && self.minimum.amount_minor > 0
            && self.minimum.amount_minor <= self.maximum.amount_minor
    }

    /// Whether `price` lies within the bounds, in their currency.
    #[must_use]
    pub fn admits(&self, price: Money) -> bool {
        price.currency_code == self.minimum.currency_code
            && price.amount_minor >= self.minimum.amount_minor
            && price.amount_minor <= self.maximum.amount_minor
    }
}

/// One item's codes and open price as the catalogue authors and keeps them: the three fields a
/// [`crate::menu::MenuEntry`] carries, together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ItemCodes {
    /// Up to [`BARCODES_MAX`] GTINs with valid check digits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub barcodes: Vec<String>,
    /// One to five digits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub plu: Option<String>,
    /// The bounds of a price the cashier types, for an open-price item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub open_price: Option<OpenPrice>,
}

impl ItemCodes {
    /// Whether the item has no code and no open price, as every item had before them.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.barcodes.is_empty() && self.plu.is_none() && self.open_price.is_none()
    }

    /// The first thing wrong with these codes, or `None` (see [`codes_fault`]).
    #[must_use]
    pub fn fault(&self) -> Option<String> {
        codes_fault(
            &self.barcodes,
            self.plu.as_deref(),
            self.open_price.as_ref(),
        )
    }

    /// Every code here, barcodes then the PLU, for a uniqueness check across a menu.
    pub fn codes(&self) -> impl Iterator<Item = &str> {
        self.barcodes
            .iter()
            .map(String::as_str)
            .chain(self.plu.as_deref())
    }
}

/// Whether `code` is a GTIN-8, -12, -13 or -14 whose last digit is its check digit (GS1 mod 10).
#[must_use]
pub fn gtin_valid(code: &str) -> bool {
    if !GTIN_LENGTHS.contains(&code.len()) || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let digits: Vec<u32> = code.bytes().map(|byte| u32::from(byte - b'0')).collect();
    let Some((check, body)) = digits.split_last() else {
        return false;
    };
    // From the right of the body, weights alternate 3, 1, 3, ...
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(position, digit)| if position % 2 == 0 { digit * 3 } else { *digit })
        .sum();
    (10 - sum % 10) % 10 == *check
}

/// Whether `plu` is one to five digits.
#[must_use]
pub fn plu_valid(plu: &str) -> bool {
    (1..=PLU_DIGITS_MAX).contains(&plu.len()) && plu.bytes().all(|byte| byte.is_ascii_digit())
}

/// The first thing wrong with one item's codes, as a sentence for the person authoring them, or
/// `None` when they may be published.
#[must_use]
pub fn codes_fault(
    barcodes: &[String],
    plu: Option<&str>,
    open_price: Option<&OpenPrice>,
) -> Option<String> {
    if barcodes.len() > BARCODES_MAX {
        return Some(format!("an item carries at most {BARCODES_MAX} barcodes"));
    }
    if let Some(bad) = barcodes.iter().find(|code| !gtin_valid(code)) {
        return Some(format!(
            "`{bad}` is not a GTIN-8, -12, -13 or -14 with a valid check digit"
        ));
    }
    if barcodes
        .iter()
        .enumerate()
        .any(|(index, code)| barcodes.iter().take(index).any(|earlier| earlier == code))
    {
        return Some("a barcode is listed twice".to_owned());
    }
    if plu.is_some_and(|plu| !plu_valid(plu)) {
        return Some("a PLU is one to five digits".to_owned());
    }
    if open_price.is_some_and(|bounds| !bounds.is_valid()) {
        return Some(
            "an open price's minimum is above zero and no more than its maximum, in one currency"
                .to_owned(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::CurrencyCode;

    #[test]
    fn a_gtin_is_valid_only_with_its_check_digit() {
        // A fake EAN-13, UPC-A, EAN-8 and GTIN-14, each with its check digit.
        for code in [
            "4006381333931",
            "036000291452",
            "96385074",
            "10012345678902",
        ] {
            assert!(gtin_valid(code), "{code}");
        }
        for code in ["4006381333932", "12345", "40063813339a1", ""] {
            assert!(!gtin_valid(code), "{code}");
        }
    }

    #[test]
    fn a_plu_is_one_to_five_digits() {
        assert!(plu_valid("4011") && plu_valid("1") && plu_valid("99999"));
        assert!(!plu_valid("") && !plu_valid("123456") && !plu_valid("40a1"));
    }

    #[test]
    fn an_items_codes_are_refused_with_the_first_fault() {
        let vnd = |minor| Money::new(CurrencyCode::VND, minor);
        let codes = |list: &[&str]| {
            list.iter()
                .map(|code| (*code).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(codes_fault(&codes(&["96385074"]), Some("4011"), None), None);
        assert!(codes_fault(&codes(&["96385075"]), None, None).is_some());
        assert!(codes_fault(&codes(&["96385074", "96385074"]), None, None).is_some());
        assert!(codes_fault(&codes(&["96385074"; 9]), None, None).is_some());
        assert!(codes_fault(&[], Some("123456"), None).is_some());
        let bounds = OpenPrice {
            minimum: vnd(10_000),
            maximum: vnd(5_000),
        };
        assert!(codes_fault(&[], None, Some(&bounds)).is_some());
        let bounds = OpenPrice {
            minimum: vnd(5_000),
            maximum: vnd(50_000),
        };
        assert_eq!(codes_fault(&[], None, Some(&bounds)), None);
        assert!(bounds.admits(vnd(5_000)) && bounds.admits(vnd(50_000)));
        assert!(!bounds.admits(vnd(4_999)) && !bounds.admits(vnd(50_001)));
    }
}
