// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Reading a variable-measure code
//! ([ADR-0222](../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
//! decision 3).
//!
//! A labelling scale prints an EAN-13 whose GS1 prefix is 20 to 29 and which carries an item's
//! reference and the package's price or weight. Which prefix carries which is each national GS1
//! body's to assign, so the layout is the store's setting, seeded by its country pack, and this
//! one pure function reads a code by it. The item reference is the item's PLU; a code that does not
//! read is a plain code, looked up as it is.

use pos_proto::item_codes::{VariableMeasureLayout, gtin_valid};

/// What a variable-measure code carries besides its item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// The package's price, in the currency's minor unit.
    PriceMinor(i64),
    /// The package's net weight, in grams.
    Grams(u32),
}

/// A variable-measure code, read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VariableMeasure {
    /// The item's reference, which names the item by its PLU.
    pub item_reference: u32,
    /// The price or weight the code carries.
    pub measure: Measure,
}

impl VariableMeasure {
    /// Whether this code names the item whose PLU is `plu`: the same number, whatever its leading
    /// zeros, since a five-digit reference is the PLU written out to five places.
    #[must_use]
    pub fn names_plu(&self, plu: &str) -> bool {
        plu.parse::<u32>()
            .is_ok_and(|number| number == self.item_reference)
    }
}

/// Reads `code` by `layout`: `None` for a layout that reads none, and for a code that is not an
/// EAN-13 with a GS1 prefix of 20 to 29 and a right check digit.
#[must_use]
pub fn read(code: &str, layout: VariableMeasureLayout) -> Option<VariableMeasure> {
    let weight = match layout {
        VariableMeasureLayout::Price5 => false,
        VariableMeasureLayout::Weight5 => true,
        VariableMeasureLayout::Unspecified | VariableMeasureLayout::None => return None,
    };
    if code.len() != 13 || !code.starts_with('2') || !gtin_valid(code) {
        return None;
    }
    let item_reference = code.get(2..7)?.parse::<u32>().ok()?;
    let value = code.get(7..12)?.parse::<u32>().ok()?;
    let measure = if weight {
        Measure::Grams(value)
    } else {
        Measure::PriceMinor(i64::from(value))
    };
    Some(VariableMeasure {
        item_reference,
        measure,
    })
}

#[cfg(test)]
mod tests {
    use super::{Measure, VariableMeasure, read};
    use pos_proto::item_codes::VariableMeasureLayout;

    /// `body` with its GS1 check digit appended.
    fn with_check(body: &str) -> String {
        let sum: u32 = body
            .bytes()
            .rev()
            .enumerate()
            .map(|(position, byte)| {
                let digit = u32::from(byte - b'0');
                if position % 2 == 0 { digit * 3 } else { digit }
            })
            .sum();
        format!("{body}{}", (10 - sum % 10) % 10)
    }

    #[test]
    fn a_price_code_reads_its_item_and_price() {
        let code = with_check("210012304500");
        assert_eq!(
            read(&code, VariableMeasureLayout::Price5),
            Some(VariableMeasure {
                item_reference: 123,
                measure: Measure::PriceMinor(4_500),
            })
        );
    }

    #[test]
    fn a_weight_code_reads_its_item_and_grams() {
        let code = with_check("290004200375");
        let read = read(&code, VariableMeasureLayout::Weight5);
        assert_eq!(
            read,
            Some(VariableMeasure {
                item_reference: 42,
                measure: Measure::Grams(375),
            })
        );
        assert!(read.is_some_and(|measure| measure.names_plu("42")));
        assert!(read.is_some_and(|measure| measure.names_plu("00042")));
        assert!(!read.is_some_and(|measure| measure.names_plu("421")));
    }

    #[test]
    fn a_code_that_is_not_variable_measure_is_a_plain_code() {
        let plain = with_check("893123456789");
        let wrong_check = format!("{}0", &with_check("210012304500")[..12]);
        for (code, layout) in [
            (with_check("210012304500"), VariableMeasureLayout::None),
            (plain, VariableMeasureLayout::Price5),
            ("2100123045".to_owned(), VariableMeasureLayout::Price5),
            ("21001230450A".to_owned(), VariableMeasureLayout::Price5),
        ] {
            assert_eq!(read(&code, layout), None, "{code}");
        }
        if wrong_check != with_check("210012304500") {
            assert_eq!(read(&wrong_check, VariableMeasureLayout::Price5), None);
        }
    }
}
