// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Keeping personal data out of the event log, by construction.
//!
//! # Why this is a type and not a policy
//!
//! The event log is immutable. That is what makes it trustworthy for reconciliation,
//! and it is also what makes personal data inside it a permanent problem: there would
//! be no way to erase one person without rewriting all history and every backup, in
//! every store's ninety-day retention and every archived partition.
//!
//! So `docs/pos-spec.md` §15 calls this a **mandatory design consequence**: events
//! carry a [`SubjectId`](crate::ids::SubjectId) and nothing more. The personal record
//! lives in a separate table, and anonymising somebody is deleting one row. The one
//! technical guarantee the framework makes about it is that **financial figures still
//! reconcile afterwards**.
//!
//! A rule this consequential should not depend on every reviewer remembering it. So
//! [`NoPii`] is a sealed marker trait, every event payload asserts it field by field,
//! and putting a `String` in a payload becomes a compile error with an instruction
//! attached:
//!
//! ```text
//! error[E0277]: `String` may not appear inside an event payload:
//!               it is not proven free of personal data
//!   = help: carry a `SubjectId` referencing a separately stored personal-data
//!           record instead
//! ```
//!
//! # What the type system cannot do
//!
//! It cannot tell whether a newtype over `Box<str>` holds a translation key or a
//! guest's name. Two further layers cover that: the event-schema snapshot puts every
//! field name in front of a reviewer on every change, and a test rejects field names
//! drawn from a deny-list (`email`, `phone`, `name`, `address`, and so on). Neither is
//! airtight; together with the sealed trait they make the honest mistake very hard and
//! the deliberate one visible.

use crate::enums;
use crate::ids;
use crate::money::{CurrencyCode, Money, Ratio};
use crate::quantity::Quantity;
use crate::text::{DisplayName, PermissionKey, ReleaseTag, TranslationKey};
use crate::time::{BusinessDate, CalendarDate, Timestamp};
use crate::ulid::Ulid;
use crate::wire_enum::{Open, WireEnum};

mod sealed {
    /// Prevents `NoPii` from being implemented outside this module.
    pub trait Sealed {}
}

/// Marks a type as proven free of personal data, and therefore admissible inside an
/// event payload.
///
/// Sealed on purpose. A new implementation means editing this file, which means a
/// reviewer sees it and — per `AGENTS.md` §7, since it touches how personal data is
/// handled — an ADR precedes it.
#[diagnostic::on_unimplemented(
    message = "`{Self}` may not appear inside an event payload: it is not proven free of personal data",
    label = "not proven free of personal data",
    note = "the event log is immutable, so anything personal inside it could never be erased",
    note = "carry a `SubjectId` referencing a separately stored personal-data record instead"
)]
pub trait NoPii: sealed::Sealed {}

/// Implements the marker for types that carry no personal data.
macro_rules! no_pii {
    ($($type:ty),+ $(,)?) => {
        $(
            impl sealed::Sealed for $type {}
            impl NoPii for $type {}
        )+
    };
}

// Scalars. Note the deliberate absence of `String`, `&str` and `Box<str>`: free text
// is exactly where a phone number ends up.
no_pii!(
    bool, i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, usize, isize
);

// Product text. `GuestNote` is deliberately absent — see `crate::text`, which
// explains why a free-text note stays on the local order record and never enters the
// immutable log.
no_pii!(DisplayName, TranslationKey, PermissionKey, ReleaseTag);

// Value types.
no_pii!(
    Ulid,
    Money,
    CurrencyCode,
    Ratio,
    Quantity,
    Timestamp,
    BusinessDate,
    CalendarDate,
);

// Identifiers. `SubjectId` is included precisely because it is the sanctioned way to
// *reference* a person without carrying anything about them.
no_pii!(
    ids::BillId,
    ids::BrandId,
    ids::CampaignId,
    ids::ConfigVersionId,
    ids::DeviceId,
    ids::EmployeeId,
    ids::EventId,
    ids::IngredientId,
    ids::MenuItemId,
    ids::OrderId,
    ids::OrderLineId,
    ids::PaymentId,
    ids::QrSessionId,
    ids::ShiftId,
    ids::StockLedgerEntryId,
    ids::CourseId,
    ids::ReasonCodeId,
    ids::ShipmentId,
    ids::StationId,
    ids::StoreId,
    ids::SubjectId,
    ids::TableId,
    ids::TaxClassId,
    ids::TenantId,
    ids::VoucherId,
);

// Closed vocabularies.
no_pii!(
    enums::BillState,
    enums::OrderLineState,
    enums::OrderState,
    enums::PaymentMethod,
    enums::PaymentOutcome,
    enums::ReductionKind,
    enums::SalesChannel,
    enums::ShipmentStatus,
    enums::ShiftState,
    enums::StockLedgerEntryKind,
    enums::TableState,
);

impl<T: NoPii> sealed::Sealed for Option<T> {}
impl<T: NoPii> NoPii for Option<T> {}

impl<T: NoPii> sealed::Sealed for Vec<T> {}
impl<T: NoPii> NoPii for Vec<T> {}

impl<T: NoPii, const N: usize> sealed::Sealed for [T; N] {}
impl<T: NoPii, const N: usize> NoPii for [T; N] {}

impl<E: WireEnum> sealed::Sealed for Open<E> {}
impl<E: WireEnum> NoPii for Open<E> {}

/// Compile-time assertion that `T` may appear in an event payload.
///
/// Event payload declarations call this once per field, so the check happens whether
/// or not anybody remembers to think about it.
pub const fn assert_no_pii<T: NoPii>() {}

/// Field-name fragments barred from an event payload.
///
/// The second layer described in the module documentation: a name-based net for what
/// the type system cannot see, such as a newtype over text.
///
/// Matched **token-wise**, not as substrings. A substring match would both miss
/// `buyerName` and falsely accuse `company_id` of containing `pan`, so a fragment
/// matches only when its words line up with consecutive words of the field name.
///
/// Note what is deliberately absent: a bare `name`. A line snapshot legitimately
/// captures `display_name` — the menu item's name at the moment the line was added
/// (`docs/pos-spec.md` §14.2) — and that is not personal data.
pub const FORBIDDEN_FIELD_FRAGMENTS: &[&str] = &[
    "email",
    "phone",
    "mobile",
    "address",
    "birth",
    "dob",
    "passport",
    "national_id",
    "tax_code",
    "card_number",
    "pan",
    "cvv",
    "iban",
    "full_name",
    "first_name",
    "last_name",
    "surname",
    "given_name",
    "buyer_name",
    "customer_name",
    "guest_name",
];

/// Splits a field name into lowercase words, on underscores and case boundaries.
fn words(field_name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_was_lower = false;
    for character in field_name.chars() {
        if character == '_' || character == '-' {
            if !current.is_empty() {
                words.push(core::mem::take(&mut current));
            }
            previous_was_lower = false;
            continue;
        }
        // A lower-to-upper transition starts a new word, so `buyerName` splits the
        // same way `buyer_name` does.
        if character.is_uppercase() && previous_was_lower && !current.is_empty() {
            words.push(core::mem::take(&mut current));
        }
        previous_was_lower = character.is_lowercase() || character.is_numeric();
        current.extend(character.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Whether a field name is barred from an event payload.
///
/// # Errors
///
/// Returns the matching fragment when the name is barred.
pub fn check_field_name(field_name: &str) -> Result<(), &'static str> {
    let actual = words(field_name);
    for fragment in FORBIDDEN_FIELD_FRAGMENTS {
        let wanted: Vec<&str> = fragment.split('_').collect();
        if actual.len() < wanted.len() {
            continue;
        }
        let matched = actual.windows(wanted.len()).any(|window| {
            window
                .iter()
                .zip(&wanted)
                .all(|(left, right)| left == right)
        });
        if matched {
            return Err(fragment);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{NoPii, assert_no_pii, check_field_name};
    use crate::enums::PaymentMethod;
    use crate::ids::{StoreId, SubjectId};
    use crate::money::Money;
    use crate::wire_enum::Open;

    #[test]
    fn the_types_an_event_payload_needs_are_admissible() {
        assert_no_pii::<i64>();
        assert_no_pii::<bool>();
        assert_no_pii::<Money>();
        assert_no_pii::<StoreId>();
        assert_no_pii::<Open<PaymentMethod>>();
        assert_no_pii::<Option<Money>>();
        assert_no_pii::<Vec<StoreId>>();
        assert_no_pii::<Option<Vec<Money>>>();
    }

    #[test]
    fn subject_id_is_admissible_because_that_is_the_whole_point() {
        // It references a person without carrying anything about them, so erasure is
        // deleting one row rather than rewriting history.
        assert_no_pii::<SubjectId>();
    }

    #[test]
    fn text_is_not_admissible() {
        // Cannot be asserted positively — the point is that it does not compile. Each
        // line below must fail with the diagnostic in the module documentation:
        //
        //   assert_no_pii::<String>();
        //   assert_no_pii::<&str>();
        //   assert_no_pii::<Box<str>>();
        //   assert_no_pii::<Vec<String>>();
        //
        // A `trybuild` fixture would let CI prove it rather than trusting a comment;
        // that arrives with the event catalogue, where the payload macro makes the
        // assertion automatic.
        fn accepts_only_no_pii<T: NoPii>() {}
        accepts_only_no_pii::<i64>();
    }

    #[test]
    fn the_name_net_catches_what_the_type_system_cannot() {
        assert_eq!(check_field_name("line_count"), Ok(()));
        assert_eq!(check_field_name("subject_id"), Ok(()));
        assert_eq!(check_field_name("guest_phone_number"), Err("phone"));
        assert_eq!(check_field_name("contact_email"), Err("email"));
        assert_eq!(check_field_name("buyer_tax_code"), Err("tax_code"));
        assert_eq!(check_field_name("shipping_address"), Err("address"));
    }

    #[test]
    fn matching_is_token_wise_so_camel_case_is_caught_too() {
        // Field names are snake_case by standard, but a net that only understands one
        // spelling is a net with a hole in it.
        assert_eq!(check_field_name("buyerName"), Err("buyer_name"));
        assert_eq!(check_field_name("BuyerName"), Err("buyer_name"));
        assert_eq!(check_field_name("guestPhone"), Err("phone"));
    }

    #[test]
    fn matching_is_token_wise_so_innocent_names_are_not_accused() {
        // Substring matching would fail all of these: `company` contains `pan`,
        // and `birthday_promotion_id` is about a campaign rather than a date of birth
        // only because `birth` stands alone as a word — which is exactly the
        // distinction token matching can make and substring matching cannot.
        for innocent in [
            "company_id",
            "expanded_view_enabled",
            "line_count",
            "prep_duration_ms",
            "display_name",
            "campaign_id",
        ] {
            assert_eq!(
                check_field_name(innocent),
                Ok(()),
                "{innocent} was wrongly accused"
            );
        }
    }

    #[test]
    fn display_name_stays_legal_because_a_line_snapshot_needs_it() {
        // `pos-spec.md` §14.2 requires a line to capture the item's display name at
        // add time, so a bare `name` must not be on the deny-list.
        assert_eq!(check_field_name("display_name"), Ok(()));
        assert_eq!(check_field_name("item_display_name"), Ok(()));
    }

    #[test]
    fn the_buyer_fields_are_barred_from_payloads() {
        // `buyer_name`, `buyer_tax_code` and `buyer_email` are real fields on a bill
        // and feed the corporate-invoice flow — but they are personal data, so they
        // live in the subject store and never in an event.
        for field in ["buyer_name", "buyer_tax_code", "buyer_email"] {
            assert!(
                check_field_name(field).is_err(),
                "{field} must be barred from an event payload"
            );
        }
    }
}
