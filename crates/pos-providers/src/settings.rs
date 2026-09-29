// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A provider's settings schema, and the one check every write goes through.
//!
//! The schema is deliberately small (ADR-0153, *Consequences*): typed fields with no conditional
//! logic and no scripting. A vendor that needs more gets a new [`FieldKind`], added once for every
//! vendor, rather than a form of its own.
//!
//! # Secrets travel separately
//!
//! A connection's settings and its secrets are two maps, never one. The settings are stored and
//! returned in the clear. The secrets are sealed the moment they arrive and are never returned. A
//! value keyed by a secret field in the *settings* map is refused rather than quietly moved across,
//! because the caller that sent it has already put a secret somewhere it gets echoed back.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ProviderDescriptor;

/// The longest URL a setting may hold. Long enough for any real endpoint, short enough that a pasted
/// document is refused rather than stored.
const URL_MAX_LEN: usize = 2048;

/// One field a tenant fills in.
#[derive(Debug)]
pub struct SettingField {
    /// The field's key in a connection's settings, `snake_case`. Stable, like the provider id: a
    /// renamed key is a stored value nobody reads.
    pub key: &'static str,
    /// The translation key for the field's label in the console.
    pub label_key: &'static str,
    /// What the field holds.
    pub kind: FieldKind,
    /// Whether a connection is incomplete without it.
    pub required: bool,
}

/// What a field holds, and so how the console draws it and how [`ProviderDescriptor::check`] reads
/// it.
#[derive(Debug)]
pub enum FieldKind {
    /// Free text, at most `max_len` characters.
    Text {
        /// The longest value accepted, in characters.
        max_len: usize,
    },
    /// An `https://` URL. Plain `http` is refused: a connector sends a tenant's credentials to it.
    Url,
    /// A whole number in `min..=max`.
    Number {
        /// The smallest value accepted.
        min: i64,
        /// The largest value accepted.
        max: i64,
    },
    /// One of a fixed list of values.
    Choice {
        /// The values accepted, in the order the console lists them.
        options: &'static [&'static str],
    },
    /// On or off.
    Flag,
    /// A credential: write-only, sealed on arrival, never returned (ADR-0153 decision 4).
    Secret {
        /// The longest value accepted, in characters.
        max_len: usize,
    },
}

impl FieldKind {
    /// The kind as the admin API spells it, so the console knows which input to draw.
    #[must_use]
    pub const fn wire(&self) -> &'static str {
        match self {
            Self::Text { .. } => "SETTING_KIND_TEXT",
            Self::Url => "SETTING_KIND_URL",
            Self::Number { .. } => "SETTING_KIND_NUMBER",
            Self::Choice { .. } => "SETTING_KIND_CHOICE",
            Self::Flag => "SETTING_KIND_FLAG",
            Self::Secret { .. } => "SETTING_KIND_SECRET",
        }
    }
}

/// A setting's value as it arrives from the console and as it is stored.
///
/// Untagged, so the JSON is the plain value an operator typed: `true`, `30`, `"https://…"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    /// A [`FieldKind::Flag`].
    Flag(bool),
    /// A [`FieldKind::Number`].
    Number(i64),
    /// Every other kind.
    Text(String),
}

/// A connection's non-secret settings, by field key.
pub type Settings = BTreeMap<String, SettingValue>;

/// One thing wrong with what was submitted, named so the console can put it under the right field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingError {
    /// The schema has no field with this key.
    Unknown {
        /// The key that was sent.
        key: String,
    },
    /// A required field has no value.
    Missing {
        /// The field's key.
        key: &'static str,
    },
    /// A secret field's value was sent with the settings, where it would be stored in the clear.
    SecretInSettings {
        /// The field's key.
        key: &'static str,
    },
    /// A non-secret field was sent with the secrets.
    NotASecret {
        /// The field's key.
        key: &'static str,
    },
    /// The value is not the kind the field holds, or not one of its choices.
    Invalid {
        /// The field's key.
        key: &'static str,
    },
    /// The value is longer than the field accepts.
    TooLong {
        /// The field's key.
        key: &'static str,
    },
    /// The number is outside the field's range.
    OutOfRange {
        /// The field's key.
        key: &'static str,
    },
}

impl SettingError {
    /// The key of the field this is about.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Unknown { key } => key,
            Self::Missing { key }
            | Self::SecretInSettings { key }
            | Self::NotASecret { key }
            | Self::Invalid { key }
            | Self::TooLong { key }
            | Self::OutOfRange { key } => key,
        }
    }

    /// A stable token for the reason, for an API error body and the console's translation of it.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::Unknown { .. } => "SETTING_UNKNOWN",
            Self::Missing { .. } => "SETTING_MISSING",
            Self::SecretInSettings { .. } => "SETTING_IS_SECRET",
            Self::NotASecret { .. } => "SETTING_NOT_SECRET",
            Self::Invalid { .. } => "SETTING_INVALID",
            Self::TooLong { .. } => "SETTING_TOO_LONG",
            Self::OutOfRange { .. } => "SETTING_OUT_OF_RANGE",
        }
    }
}

impl ProviderDescriptor {
    /// Checks a connection's settings and secrets against this provider's schema.
    ///
    /// `secrets` are the secret values sent with *this* write. `stored_secrets` are the keys the
    /// connection already holds sealed, so an edit that changes an endpoint does not have to
    /// re-enter a password it cannot see. A required secret is satisfied by either.
    ///
    /// # Errors
    ///
    /// Every problem found, not only the first: the console marks each field at once rather than
    /// making an operator submit a form once per mistake. Unknown keys come first, then the rest in
    /// the schema's order.
    pub fn check(
        &self,
        settings: &Settings,
        secrets: &BTreeMap<String, String>,
        stored_secrets: &BTreeSet<String>,
    ) -> Result<(), Vec<SettingError>> {
        let mut errors: Vec<SettingError> = settings
            .keys()
            .chain(secrets.keys())
            .filter(|key| self.field(key).is_none())
            .map(|key| SettingError::Unknown { key: key.clone() })
            .collect();
        for field in self.fields {
            let error = match &field.kind {
                FieldKind::Secret { max_len } => {
                    check_secret(field, *max_len, settings, secrets, stored_secrets)
                }
                kind @ (FieldKind::Text { .. }
                | FieldKind::Url
                | FieldKind::Number { .. }
                | FieldKind::Choice { .. }
                | FieldKind::Flag) => check_setting(field, kind, settings, secrets),
            };
            errors.extend(error);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn check_secret(
    field: &'static SettingField,
    max_len: usize,
    settings: &Settings,
    secrets: &BTreeMap<String, String>,
    stored_secrets: &BTreeSet<String>,
) -> Option<SettingError> {
    let key = field.key;
    if settings.contains_key(key) {
        return Some(SettingError::SecretInSettings { key });
    }
    match secrets.get(key) {
        Some(value) if value.is_empty() => Some(SettingError::Invalid { key }),
        Some(value) if value.chars().count() > max_len => Some(SettingError::TooLong { key }),
        None if field.required && !stored_secrets.contains(key) => {
            Some(SettingError::Missing { key })
        }
        Some(_) | None => None,
    }
}

fn check_setting(
    field: &'static SettingField,
    kind: &FieldKind,
    settings: &Settings,
    secrets: &BTreeMap<String, String>,
) -> Option<SettingError> {
    let key = field.key;
    if secrets.contains_key(key) {
        return Some(SettingError::NotASecret { key });
    }
    let Some(value) = settings.get(key) else {
        return field.required.then_some(SettingError::Missing { key });
    };
    match (kind, value) {
        (FieldKind::Flag, SettingValue::Flag(_)) => None,
        (FieldKind::Number { min, max }, SettingValue::Number(number)) => {
            (number < min || number > max).then_some(SettingError::OutOfRange { key })
        }
        (FieldKind::Text { .. } | FieldKind::Url, SettingValue::Text(text))
            if text.trim().is_empty() =>
        {
            field.required.then_some(SettingError::Missing { key })
        }
        (FieldKind::Text { max_len }, SettingValue::Text(text)) => {
            (text.chars().count() > *max_len).then_some(SettingError::TooLong { key })
        }
        (FieldKind::Url, SettingValue::Text(text)) => url_error(key, text),
        (FieldKind::Choice { options }, SettingValue::Text(text)) => {
            (!options.contains(&text.as_str())).then_some(SettingError::Invalid { key })
        }
        _ => Some(SettingError::Invalid { key }),
    }
}

/// An `https://` URL with a host and no whitespace. Not a full parser: the connector that uses the
/// value parses it properly, and this only refuses what is plainly not an endpoint.
fn url_error(key: &'static str, text: &str) -> Option<SettingError> {
    if text.len() > URL_MAX_LEN {
        return Some(SettingError::TooLong { key });
    }
    let host = text
        .strip_prefix("https://")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .unwrap_or_default();
    let plausible = !host.is_empty() && !text.chars().any(char::is_whitespace);
    (!plausible).then_some(SettingError::Invalid { key })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{FieldKind, SettingError, SettingField, SettingValue, Settings};
    use crate::{Family, ProviderDescriptor};

    const GATEWAY: ProviderDescriptor = ProviderDescriptor {
        provider_id: "qr.example",
        family: Family::QrPayment,
        name_key: "provider.qr.example",
        countries: &[],
        fields: &[
            SettingField {
                key: "endpoint",
                label_key: "provider.field.endpoint",
                kind: FieldKind::Url,
                required: true,
            },
            SettingField {
                key: "merchant_code",
                label_key: "provider.field.merchant_code",
                kind: FieldKind::Text { max_len: 8 },
                required: true,
            },
            SettingField {
                key: "timeout_seconds",
                label_key: "provider.field.timeout_seconds",
                kind: FieldKind::Number { min: 1, max: 60 },
                required: false,
            },
            SettingField {
                key: "environment",
                label_key: "provider.field.environment",
                kind: FieldKind::Choice {
                    options: &["test", "live"],
                },
                required: false,
            },
            SettingField {
                key: "auto_confirm",
                label_key: "provider.field.auto_confirm",
                kind: FieldKind::Flag,
                required: false,
            },
            SettingField {
                key: "api_key",
                label_key: "provider.field.api_key",
                kind: FieldKind::Secret { max_len: 16 },
                required: true,
            },
        ],
        capabilities: &[],
        sandbox: false,
    };

    fn settings(pairs: &[(&str, SettingValue)]) -> Settings {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    fn text(value: &str) -> SettingValue {
        SettingValue::Text(value.to_owned())
    }

    fn secret(key: &str, value: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(key.to_owned(), value.to_owned())])
    }

    fn complete() -> Settings {
        settings(&[
            ("endpoint", text("https://gateway.example/v1")),
            ("merchant_code", text("M123")),
            ("timeout_seconds", SettingValue::Number(30)),
            ("environment", text("test")),
            ("auto_confirm", SettingValue::Flag(true)),
        ])
    }

    #[test]
    fn a_complete_connection_passes() {
        let checked = GATEWAY.check(&complete(), &secret("api_key", "k"), &BTreeSet::new());
        assert_eq!(checked, Ok(()));
    }

    #[test]
    fn an_edit_need_not_resend_a_secret_the_connection_already_holds() {
        let stored = BTreeSet::from(["api_key".to_owned()]);
        assert_eq!(
            GATEWAY.check(&complete(), &BTreeMap::new(), &stored),
            Ok(())
        );
        assert_eq!(
            GATEWAY.check(&complete(), &BTreeMap::new(), &BTreeSet::new()),
            Err(vec![SettingError::Missing { key: "api_key" }]),
        );
    }

    #[test]
    fn every_problem_is_reported_at_once_unknown_keys_first() {
        let sent = settings(&[
            ("endpoint", text("http://gateway.example")),
            ("merchant_code", text("TOO-LONG-CODE")),
            ("timeout_seconds", SettingValue::Number(0)),
            ("environment", text("staging")),
            ("auto_confirm", text("yes")),
            ("colour", text("red")),
        ]);
        let errors = GATEWAY
            .check(&sent, &BTreeMap::new(), &BTreeSet::new())
            .expect_err("every field is wrong");
        let reasons: Vec<(&str, &str)> = errors
            .iter()
            .map(|error| (error.key(), error.reason()))
            .collect();
        assert_eq!(
            reasons,
            vec![
                ("colour", "SETTING_UNKNOWN"),
                ("endpoint", "SETTING_INVALID"),
                ("merchant_code", "SETTING_TOO_LONG"),
                ("timeout_seconds", "SETTING_OUT_OF_RANGE"),
                ("environment", "SETTING_INVALID"),
                ("auto_confirm", "SETTING_INVALID"),
                ("api_key", "SETTING_MISSING"),
            ],
        );
    }

    #[test]
    fn a_secret_sent_as_a_setting_is_refused_rather_than_stored_in_the_clear() {
        let mut sent = complete();
        sent.insert("api_key".to_owned(), text("k"));
        let errors = GATEWAY
            .check(&sent, &BTreeMap::new(), &BTreeSet::new())
            .expect_err("the secret is in the wrong map");
        assert_eq!(
            errors,
            vec![SettingError::SecretInSettings { key: "api_key" }]
        );

        let errors = GATEWAY
            .check(
                &complete(),
                &BTreeMap::from([
                    ("api_key".to_owned(), "k".to_owned()),
                    ("merchant_code".to_owned(), "M123".to_owned()),
                ]),
                &BTreeSet::new(),
            )
            .expect_err("a setting is in the secrets");
        assert_eq!(
            errors,
            vec![SettingError::NotASecret {
                key: "merchant_code"
            }]
        );
    }

    #[test]
    fn a_blank_required_field_is_missing_and_a_blank_secret_is_invalid() {
        let mut sent = complete();
        sent.insert("merchant_code".to_owned(), text("   "));
        let errors = GATEWAY
            .check(&sent, &secret("api_key", ""), &BTreeSet::new())
            .expect_err("two blanks");
        assert_eq!(
            errors,
            vec![
                SettingError::Missing {
                    key: "merchant_code"
                },
                SettingError::Invalid { key: "api_key" },
            ],
        );
    }

    #[test]
    fn a_url_needs_https_and_a_host() {
        for (url, ok) in [
            ("https://gateway.example", true),
            ("https://gateway.example:8443/path?x=1", true),
            ("https://", false),
            ("https:///path", false),
            ("ftp://gateway.example", false),
            ("https://gate way.example", false),
        ] {
            let mut sent = complete();
            sent.insert("endpoint".to_owned(), text(url));
            let checked = GATEWAY.check(&sent, &secret("api_key", "k"), &BTreeSet::new());
            assert_eq!(checked.is_ok(), ok, "{url}");
        }
    }

    #[test]
    fn a_value_is_the_plain_json_an_operator_typed() {
        let parsed: Settings = serde_json::from_str(
            r#"{"auto_confirm":true,"timeout_seconds":30,"endpoint":"https://x"}"#,
        )
        .expect("plain values parse");
        assert_eq!(parsed.get("auto_confirm"), Some(&SettingValue::Flag(true)));
        assert_eq!(
            parsed.get("timeout_seconds"),
            Some(&SettingValue::Number(30))
        );
        assert_eq!(parsed.get("endpoint"), Some(&text("https://x")));
        assert!(
            serde_json::from_str::<Settings>(r#"{"timeout_seconds":1.5}"#).is_err(),
            "a fraction is not a whole number"
        );
    }
}
