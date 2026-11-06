// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's own checks on a `receipt.templates` value
//! ([ADR-0207](../../../docs/adr/0207-a-receipt-is-a-structured-template-per-store-and-channel.md)
//! decision 3), beyond the register's structural one ([`pos_proto::receipt::check_templates`]):
//!
//! - every header and footer key is in the tenant's translation grid with an `en` value, so a
//!   receipt can always print it in English;
//! - no template turns off a section a country's law requires, as each compiled-in country module
//!   states it (`pos_country::CountryModule::receipt_required_sections`). A cell serves one
//!   country (ADR-0011), so the countries compiled into the cloud are the countries of the stores a
//!   value can reach.

use pos_proto::locale::CountryCode;
use pos_proto::receipt::{ReceiptSection, ReceiptTemplate, check_templates};

use crate::settings::SettingRefusal;

/// The key of the setting these checks are for.
pub const TEMPLATES_KEY: &str = "receipt.templates";

/// A country and the optional sections its law requires.
pub type RequiredSections = (CountryCode, Vec<ReceiptSection>);

/// The first refusal a `receipt.templates` value earns, or `None` when the cloud may write it.
///
/// `has_fallback` answers whether a translation key is in the tenant's grid with a non-empty `en`
/// value. `required` lists each compiled-in country and the sections it requires. A value the
/// register refuses is not this function's to judge, and earns `None` here: the register's own
/// check has refused it already.
#[must_use]
pub fn refusal(
    value: &serde_json::Value,
    has_fallback: impl Fn(&str) -> bool,
    required: &[RequiredSections],
) -> Option<SettingRefusal> {
    let templates = check_templates(value).ok()?;
    let every: Vec<&ReceiptTemplate> = templates
        .base
        .iter()
        .chain(templates.channels.values())
        .collect();
    for template in &every {
        for key in template.header_keys.iter().chain(&template.footer_keys) {
            if !has_fallback(key.as_str()) {
                return Some(SettingRefusal::UnknownTranslationKey {
                    setting_key: TEMPLATES_KEY.to_owned(),
                    translation_key: key.as_str().to_owned(),
                });
            }
        }
    }
    for template in &every {
        for section in template.sections.turned_off() {
            if let Some((country, _)) = required
                .iter()
                .find(|(_, sections)| sections.contains(&section))
            {
                return Some(SettingRefusal::SectionRequired {
                    setting_key: TEMPLATES_KEY.to_owned(),
                    section,
                    country_code: *country,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use pos_proto::locale::CountryCode;
    use pos_proto::receipt::ReceiptSection;
    use serde_json::json;

    use super::{TEMPLATES_KEY, refusal};
    use crate::settings::SettingRefusal;

    fn grid(key: &str) -> bool {
        matches!(key, "receipt.footer.thanks" | "receipt.header.welcome")
    }

    fn vietnam() -> Vec<(CountryCode, Vec<ReceiptSection>)> {
        vec![(CountryCode::VN, vec![ReceiptSection::Modifiers])]
    }

    #[test]
    fn keys_in_the_grid_and_optional_sections_off_are_written() {
        let value = json!({
            "base": {
                "header_keys": ["receipt.header.welcome"],
                "footer_keys": ["receipt.footer.thanks"],
                "sections": { "payments": false, "change_line": false, "second_language": false }
            },
            "channels": { "SALES_CHANNEL_TAKEAWAY": { "footer_keys": ["receipt.footer.thanks"] } }
        });
        assert_eq!(refusal(&value, grid, &vietnam()), None);
        assert_eq!(refusal(&json!({}), grid, &vietnam()), None);
    }

    #[test]
    fn a_key_without_an_english_value_is_refused_in_any_template() {
        for value in [
            json!({ "base": { "header_keys": ["receipt.header.missing"] } }),
            json!({ "channels": { "SALES_CHANNEL_QR": { "footer_keys": ["receipt.header.missing"] } } }),
        ] {
            assert_eq!(
                refusal(&value, grid, &vietnam()),
                Some(SettingRefusal::UnknownTranslationKey {
                    setting_key: TEMPLATES_KEY.to_owned(),
                    translation_key: "receipt.header.missing".to_owned(),
                }),
                "{value}"
            );
        }
    }

    #[test]
    fn a_section_the_country_requires_cannot_be_turned_off() {
        let value = json!({
            "channels": { "SALES_CHANNEL_DELIVERY": { "sections": { "modifiers": false } } }
        });
        assert_eq!(
            refusal(&value, grid, &vietnam()),
            Some(SettingRefusal::SectionRequired {
                setting_key: TEMPLATES_KEY.to_owned(),
                section: ReceiptSection::Modifiers,
                country_code: CountryCode::VN,
            })
        );
        // A country that requires nothing lets it go.
        assert_eq!(
            refusal(&value, grid, &[(CountryCode::ZZ, Vec::new())]),
            None
        );
        // On is never refused.
        let on = json!({ "base": { "sections": { "modifiers": true } } });
        assert_eq!(refusal(&on, grid, &vietnam()), None);
    }
}
