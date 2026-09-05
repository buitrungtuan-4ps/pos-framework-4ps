// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The three classes a country pack keys its default rate table on.
//!
//! # Why a convention rather than a lookup
//!
//! A [`TaxClassId`] is authored in the configuration tree: a store decides that its wines are one
//! class and its pizzas another, and the identifiers are ULIDs the console minted. A country module
//! runs *before* any of that — it is interrogated at start-up, before configuration exists
//! ([`CountryModule`](crate::CountryModule)) — so it cannot look a class up. It has to name one.
//!
//! So the packs share three, fixed here and documented so a console seeding a fresh store's classes
//! can mint them with these identifiers and have the country's table apply on day one.
//!
//! The three are the smallest set that makes the packs *true* rather than merely present. Two of
//! them exist because a country genuinely taxes them apart:
//!
//! - **Japan** charges 8 % on takeaway food and 10 % on takeaway alcohol, because alcohol is
//!   excluded from the reduced rate. One class could not express that.
//! - **India** does not tax alcoholic liquor for human consumption under GST at all — it is state
//!   excise and state VAT, which varies by state — so `countries/in` publishes no row for it and a
//!   store that sells it must say what its state charges.
//!
//! # What a store does instead
//!
//! Whatever it likes. These are the identifiers a *pack's default* table is keyed on; the moment a
//! store publishes its own `tax` node, that node's classes are the ones in force and these become
//! irrelevant. A store with eleven classes is not doing anything wrong — it is doing the thing
//! [ADR-0027](../../../docs/adr/0027-country-modules.md) said configuration is for.

use pos_proto::{TaxClassId, Ulid};

/// Food and non-alcoholic drink — the class most items fall in.
#[must_use]
pub fn food() -> TaxClassId {
    TaxClassId::new(Ulid::from_u128(1))
}

/// Alcohol, which several countries tax apart from food and one does not tax under the same regime
/// at all.
#[must_use]
pub fn alcohol() -> TaxClassId {
    TaxClassId::new(Ulid::from_u128(2))
}

/// Zero-rated or exempt.
#[must_use]
pub fn exempt() -> TaxClassId {
    TaxClassId::new(Ulid::from_u128(3))
}

/// The three classes with the English labels a console seeds them under.
///
/// Present so the seeder and the packs read from one list: a fourth class added here appears in the
/// console without a second edit, and a label changed here cannot drift from the identifier it names.
#[must_use]
pub fn all() -> [(&'static str, TaxClassId); 3] {
    [
        ("Food and non-alcoholic drink", food()),
        ("Alcohol", alcohol()),
        ("Zero-rated or exempt", exempt()),
    ]
}

#[cfg(test)]
mod tests {
    use super::{alcohol, all, exempt, food};

    #[test]
    fn the_three_classes_are_distinct() {
        // They key one table. Two of them colliding would make a pack's second row shadow its first,
        // and the symptom would be alcohol taxed as food — which is the exact mistake the split
        // exists to prevent.
        assert_ne!(food(), alcohol());
        assert_ne!(food(), exempt());
        assert_ne!(alcohol(), exempt());
    }

    #[test]
    fn the_labelled_list_names_the_same_three() {
        let labelled = all();
        assert_eq!(labelled.len(), 3);
        for (label, id) in labelled {
            assert!(
                !label.is_empty(),
                "a class a console cannot name is unusable"
            );
            assert!([food(), alcohol(), exempt()].contains(&id));
        }
    }
}
