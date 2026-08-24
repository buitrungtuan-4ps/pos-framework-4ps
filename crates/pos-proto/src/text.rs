// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The two kinds of text that may cross a boundary, and the one that may not.
//!
//! [`pii`](crate::pii) bars `String` and `&str` from event payloads, because free text
//! is where a phone number ends up. But `docs/pos-spec.md` §14.2 requires a line
//! snapshot to capture the item's **display name** at the moment the line was added,
//! and that is text. So the bar cannot be absolute — it has to distinguish text that
//! describes a *product* from text that might describe a *person*.
//!
//! | Type | In an event payload | Why |
//! |---|---|---|
//! | [`DisplayName`] | yes | Tenant content naming a product or category. Never about a person |
//! | [`TranslationKey`] | yes | An identifier drawn from a closed, append-only namespace |
//! | [`GuestNote`] | **no** | A free-text request that can and does name people |
//!
//! # Why `GuestNote` is deliberately excluded
//!
//! A line note is exactly the field where "for Mr Nguyễn, severe peanut allergy" gets
//! typed. Both halves of that are personal data, and one is health data. Putting it in
//! an immutable log would mean it could never be erased.
//!
//! The note is still needed — the kitchen has to read it — but it is needed **at the
//! store**, not in the chain-wide event stream. So it lives on the local order record
//! and the event carries only [`GuestNote::is_present`]'s answer. That costs the cloud
//! nothing real: no report, reconciliation or ERP posting has any use for a kitchen
//! note, and keeping it out of the log is the same reasoning that keeps full logs at
//! the store and ships only errors and metrics.
//!
//! This is a judgement, not a derivation, so it is written down here rather than left
//! implicit in a type signature.

use serde::{Deserialize, Serialize};

/// A human-readable name for a product, category, modifier or campaign.
///
/// Tenant content, resolved per language in the configuration tree. Admissible in an
/// event payload because a line snapshot must capture the name shown to the guest at
/// the moment the line was added — changing or deleting a menu item afterwards must
/// not alter an open order or a settled bill.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DisplayName(Box<str>);

impl DisplayName {
    /// Wraps a display name.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for DisplayName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A key into the translation catalogue, shaped `domain.screen.element`.
///
/// Keys are append-only, so one may be referenced from an event without pinning the
/// text it resolves to. This is how a reason code or a status message travels without
/// carrying a language with it — and why kitchen tickets can print in the station's
/// language while the receipt prints in the guest's.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TranslationKey(Box<str>);

impl TranslationKey {
    /// Wraps a key.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// The key as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for TranslationKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A permission identifier, shaped `domain.resource.action`.
///
/// Admissible in an event payload for the same reason as [`TranslationKey`]: it is
/// drawn from a closed, append-only namespace the framework owns, so it names a
/// capability rather than describing anybody. An audit trail for a manager override
/// needs it (`docs/pos-spec.md` §11).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PermissionKey(Box<str>);

impl PermissionKey {
    /// Wraps a permission identifier.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// The identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for PermissionKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A release identifier, such as `v1.4.0`.
///
/// Admissible in an event payload: it names a build, not a person. Fleet rollout
/// events carry it so the dashboard can show which store runs which version.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReleaseTag(Box<str>);

impl ReleaseTag {
    /// Wraps a release identifier.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// The identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for ReleaseTag {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A free-text note from or about a guest.
///
/// **Deliberately not admissible in an event payload.** See the module documentation:
/// this is the field where a name and a health condition get typed, so it stays on the
/// local order record and never enters the immutable log.
///
/// It carries no `NoPii` implementation, so attempting to put one in a payload is a
/// compile error rather than a review finding.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GuestNote(Box<str>);

impl GuestNote {
    /// Wraps a note.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// The note as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether a note was written at all.
    ///
    /// This — and not the text — is what an event may carry, so the kitchen display
    /// can show that a note exists while the chain-wide log stays free of personal
    /// data.
    #[must_use]
    pub fn is_present(&self) -> bool {
        !self.0.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayName, GuestNote, TranslationKey};

    #[test]
    fn a_display_name_serialises_as_a_bare_string() {
        let name = DisplayName::new("Pizza 4 Cheeses");
        assert_eq!(
            serde_json::to_string(&name).expect("serialise"),
            r#""Pizza 4 Cheeses""#
        );
    }

    #[test]
    fn a_display_name_preserves_diacritics() {
        // Which matters twice: on screen, and when the printer has to fall back to a
        // bitmap because its code page cannot render them.
        let name = DisplayName::new("Bánh mì thịt nướng");
        let json = serde_json::to_string(&name).expect("serialise");
        let back: DisplayName = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.as_str(), "Bánh mì thịt nướng");
    }

    #[test]
    fn a_translation_key_round_trips() {
        let key = TranslationKey::new("billing.payment.declined");
        assert_eq!(key.as_str(), "billing.payment.declined");
        assert_eq!(
            serde_json::from_str::<TranslationKey>(
                &serde_json::to_string(&key).expect("serialise")
            )
            .expect("deserialise"),
            key
        );
    }

    #[test]
    fn a_note_reports_presence_without_revealing_itself() {
        // The only thing about a note that an event may carry.
        assert!(GuestNote::new("no chilli please").is_present());
        assert!(!GuestNote::new("   ").is_present());
        assert!(!GuestNote::new("").is_present());
    }

    #[test]
    fn a_guest_note_is_not_admissible_in_a_payload() {
        // Cannot be asserted positively — the point is that it does not compile:
        //
        //   crate::pii::assert_no_pii::<GuestNote>();
        //
        // whereas DisplayName and TranslationKey do.
        crate::pii::assert_no_pii::<DisplayName>();
        crate::pii::assert_no_pii::<TranslationKey>();
    }
}
