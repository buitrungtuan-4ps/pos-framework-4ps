// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Who a store legally is, as the receipt prints it
//! ([ADR-0106](../../../docs/adr/0106-the-store-is-a-legal-person.md)).
//!
//! # Why this is not part of the `locale` node
//!
//! They are different facts with different authors. `locale` is *how this store writes and taxes* —
//! operations changes it when a rate changes. This is *who this store legally is* — finance changes
//! it when a company is renamed or a registration issues. Sharing a node would make a rate change
//! and a legal-identity change one publish, reviewed by one person, rolled back together.
//!
//! # Why almost nothing here is validated
//!
//! A registered address is written differently in every country, and a framework that imposed a
//! shape on it would be wrong in most of them. The one field with a checkable shape is the tax
//! registration number, and the check belongs to the **country module**
//! (`CountryModule::is_valid_tax_code`) — format only, never registration, which is what lets a store
//! be provisioned with the line down.

use serde::{Deserialize, Serialize};

/// The store's registered identity, published to it and printed on every receipt.
///
/// Empty is a legitimate state and is what a store has before anyone fills this in: the receipt then
/// prints what it printed before, which is a number and a total. Every field is omitted from the
/// document when it is empty, so a half-filled profile produces a shorter receipt rather than a
/// receipt with blank labels — an empty label on a legal document reads as a value somebody forgot.
///
/// No `deny_unknown_fields`, for the same reason as every other published node: a store running an
/// older build must apply a profile carrying a field it does not understand rather than refusing it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StoreProfile {
    /// The registered name — what the law wants on the paper.
    #[serde(default)]
    pub legal_name: String,
    /// The sign over the door, when it differs from the registered name.
    ///
    /// A guest recognises the trading name and an auditor wants the legal one, so the receipt leads
    /// with this and falls back to [`Self::legal_name`]. A store whose two names are the same leaves
    /// it blank rather than typing it twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trading_name: Option<String>,
    /// The registered address, one line per printed line, as it is written locally.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub address_lines: Vec<String>,
    /// The seller's tax registration — Japan's 登録番号, India's GSTIN, Vietnam's mã số thuế.
    ///
    /// Without it a Japanese qualified invoice does not let its buyer claim input tax, and an Indian
    /// tax invoice is not one. `None` until a legal process has issued the store a number, which is
    /// the step no pull request can take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_registration_number: Option<String>,
    /// What the paper calls that number.
    ///
    /// Free text rather than an enum: the framework cannot know every jurisdiction's label, and a
    /// closed set would make the fourth country a code change — the thing country packs exist to
    /// avoid. The console offers the country module's own suggestion; this stores what was accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_registration_label: Option<String>,
    /// What a guest calls about a bill: a phone number, an e-mail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contact_lines: Vec<String>,
    /// The tail of the receipt: the thank-you, the return policy, a caption under a QR code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footer_lines: Vec<String>,
}

impl StoreProfile {
    /// The name to lead the receipt with: the trading name when there is one, else the legal name.
    ///
    /// `None` when neither is set, which is the state a store is in before anybody fills this in —
    /// and the receipt then starts at its number, exactly as it did before this node existed.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.trading_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .or_else(|| Some(self.legal_name.trim()).filter(|name| !name.is_empty()))
    }

    /// The registration as one printed line — `GSTIN: 29ABCDE1234F1Z5` — or `None`.
    ///
    /// `None` when there is no number, **whatever the label says**: a label with nothing after it is
    /// worse than no line, because it reads as a number somebody forgot to type. A number with no
    /// label prints alone, which is legible and honest.
    #[must_use]
    pub fn registration_line(&self) -> Option<String> {
        let number = self
            .tax_registration_number
            .as_deref()
            .map(str::trim)
            .filter(|number| !number.is_empty())?;
        match self
            .tax_registration_label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
        {
            Some(label) => Some(format!("{label}: {number}")),
            None => Some(number.to_owned()),
        }
    }

    /// Whether this profile says nothing at all — the state before anybody fills it in.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.display_name().is_none()
            && self.address_lines.is_empty()
            && self.registration_line().is_none()
            && self.contact_lines.is_empty()
            && self.footer_lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::StoreProfile;

    fn filled() -> StoreProfile {
        StoreProfile {
            legal_name: "Pizza 4P's Japan K.K.".to_owned(),
            trading_name: Some("Pizza 4P's Ginza".to_owned()),
            address_lines: vec!["中央区銀座 1-2-3".to_owned()],
            tax_registration_number: Some("T1234567890123".to_owned()),
            tax_registration_label: Some("登録番号".to_owned()),
            contact_lines: vec!["03-1234-5678".to_owned()],
            footer_lines: vec!["ありがとうございました".to_owned()],
        }
    }

    #[test]
    fn the_receipt_leads_with_the_trading_name_and_falls_back_to_the_legal_one() {
        assert_eq!(filled().display_name(), Some("Pizza 4P's Ginza"));

        let one_name = StoreProfile {
            trading_name: None,
            ..filled()
        };
        assert_eq!(one_name.display_name(), Some("Pizza 4P's Japan K.K."));

        // Blank is the same as absent: a store that typed a space into the field must not get a
        // receipt headed with one.
        let blank = StoreProfile {
            trading_name: Some("   ".to_owned()),
            ..filled()
        };
        assert_eq!(blank.display_name(), Some("Pizza 4P's Japan K.K."));
    }

    #[test]
    fn a_registration_line_needs_a_number_and_not_a_label() {
        assert_eq!(
            filled().registration_line().as_deref(),
            Some("登録番号: T1234567890123")
        );

        // A number with no label prints alone, which is legible.
        let unlabelled = StoreProfile {
            tax_registration_label: None,
            ..filled()
        };
        assert_eq!(
            unlabelled.registration_line().as_deref(),
            Some("T1234567890123")
        );

        // A label with no number prints nothing: an empty label on a legal document reads as a
        // number somebody forgot to type, which is worse than a shorter receipt.
        let no_number = StoreProfile {
            tax_registration_number: None,
            ..filled()
        };
        assert_eq!(no_number.registration_line(), None);
        let blank_number = StoreProfile {
            tax_registration_number: Some("  ".to_owned()),
            ..filled()
        };
        assert_eq!(blank_number.registration_line(), None);
    }

    #[test]
    fn an_unfilled_profile_says_so() {
        assert!(StoreProfile::default().is_empty());
        assert!(!filled().is_empty());
    }

    #[test]
    fn a_profile_round_trips_and_tolerates_a_field_from_the_future() {
        let profile = filled();
        let json = serde_json::to_string(&profile).expect("serialise");
        let back: StoreProfile = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, profile);

        // A newer cloud adds a field; an older store must still apply the node, or it stops being
        // manageable — the same rule the event envelope follows.
        let from_the_future = serde_json::json!({
            "legal_name": "Pizza 4P's",
            "registered_capital": "not a field this build knows",
        });
        let parsed: StoreProfile =
            serde_json::from_value(from_the_future).expect("an unknown field is ignored");
        assert_eq!(parsed.legal_name, "Pizza 4P's");

        // And a node published before this type existed — an empty object — is a valid empty
        // profile rather than a parse failure.
        let empty: StoreProfile = serde_json::from_str("{}").expect("an empty node parses");
        assert!(empty.is_empty());
    }
}
