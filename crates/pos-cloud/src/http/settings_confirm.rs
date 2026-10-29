// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A setting value written only with a recorded confirmation
//! ([ADR-0169](../../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)):
//! what the settings catalogue tells the console about it, and what the audit trail records when
//! one is written.

use pos_proto::settings::Confirm;
use serde_json::{Value, json};

/// A catalogue entry's `confirm`: the value that needs confirming, and the translation key of the
/// notice the Settings screen shows beside it.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub(super) struct CatalogueConfirm {
    value: &'static str,
    notice: &'static str,
}

impl From<Confirm> for CatalogueConfirm {
    fn from(confirm: Confirm) -> Self {
        Self {
            value: confirm.value,
            notice: confirm.notice,
        }
    }
}

/// A `setting.set` entry's `after`: the value, and, for a value written with its notice confirmed,
/// `notice_confirmed: true` and the notice's translation key (ADR-0169 decision 2). The entry
/// itself records who wrote it and when.
pub(super) fn audit_after(value: &Value, notice: Option<&'static str>) -> Value {
    match notice {
        None => json!({ "value": value }),
        Some(notice) => json!({ "value": value, "notice_confirmed": true, "notice": notice }),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use pos_proto::settings::{Confirm, Setting, SettingScope, SettingShape};
    use serde_json::json;

    use super::super::setting_refusal::setting_refusal_response;
    use super::super::settings::catalogue::SettingCatalogueEntry;
    use super::audit_after;
    use crate::settings::SettingRefusal;

    fn drawer_owner(confirm: Option<Confirm>) -> serde_json::Value {
        let setting = Setting {
            node: "example",
            field: "drawer_owner",
            shape: SettingShape::Choice {
                values: vec!["OWNER_SHARED", "OWNER_ONE_PERSON"],
                default: "OWNER_SHARED",
                preset: None,
            },
            scopes: &[SettingScope::Store],
            since: "0.15.0",
            confirm,
            summary: "Whose a drawer is.",
        };
        serde_json::to_value(SettingCatalogueEntry::of(&setting)).expect("an entry serialises")
    }

    #[test]
    fn a_value_that_needs_confirming_is_named_with_its_notice() {
        let entry = drawer_owner(Some(Confirm {
            value: "OWNER_ONE_PERSON",
            notice: "settings.item.example.drawer_owner.confirm",
        }));
        assert_eq!(
            entry["confirm"],
            json!({
                "value": "OWNER_ONE_PERSON",
                "notice": "settings.item.example.drawer_owner.confirm",
            })
        );
    }

    #[test]
    fn a_setting_with_nothing_to_confirm_carries_no_confirm() {
        assert!(drawer_owner(None).get("confirm").is_none());
    }

    #[test]
    fn a_confirmed_write_records_its_notice_beside_the_value() {
        assert_eq!(
            audit_after(
                &json!("OWNER_ONE_PERSON"),
                Some("settings.item.example.drawer_owner.confirm")
            ),
            json!({
                "value": "OWNER_ONE_PERSON",
                "notice_confirmed": true,
                "notice": "settings.item.example.drawer_owner.confirm",
            })
        );
    }

    #[test]
    fn a_write_that_confirms_nothing_records_the_value_alone() {
        assert_eq!(
            audit_after(&json!("OWNER_SHARED"), None),
            json!({ "value": "OWNER_SHARED" })
        );
    }

    #[tokio::test]
    async fn an_unconfirmed_write_is_a_400_naming_notice_confirmed() {
        let response = setting_refusal_response(&SettingRefusal::ConfirmationRequired {
            setting_key: "example.drawer_owner".to_owned(),
            value: "OWNER_ONE_PERSON",
        });
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("collect the refusal");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("a JSON envelope");
        assert_eq!(
            body["error"]["details"],
            json!([{ "field": "notice_confirmed", "reason": "CONFIRMATION_REQUIRED" }])
        );
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("example.drawer_owner"), "{message}");
    }

    /// The console the cloud embeds ships each notice in every language it ships (ADR-0169 decision
    /// 4): a confirmation is asked in words, never under a bare key.
    #[test]
    fn every_notice_is_in_every_console_language() {
        let packs = [
            ("en", include_str!("../../../../dashboard/src/i18n/en.json")),
            ("vi", include_str!("../../../../dashboard/src/i18n/vi.json")),
        ];
        for (language, pack) in packs {
            let pack: std::collections::BTreeMap<String, String> =
                serde_json::from_str(pack).expect("a console language pack");
            for setting in pos_proto::settings::register() {
                if let Some(confirm) = setting.confirm {
                    assert!(
                        pack.contains_key(confirm.notice),
                        "the console's {language} pack has no `{}`",
                        confirm.notice
                    );
                }
            }
        }
    }
}
