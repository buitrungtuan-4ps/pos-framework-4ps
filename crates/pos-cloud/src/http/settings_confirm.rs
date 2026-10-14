// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A setting value written only with a recorded confirmation
//! ([ADR-0169](../../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)):
//! what the settings catalogue tells the console about it.

use pos_proto::settings::Confirm;

/// A catalogue entry's `confirm`: the value that needs confirming, and the translation key of the
/// notice the Settings screen shows beside it.
#[derive(Debug, Clone, serde::Serialize)]
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

#[cfg(test)]
mod tests {
    use pos_proto::settings::{Confirm, Setting, SettingScope, SettingShape};
    use serde_json::json;

    use super::super::SettingCatalogueEntry;

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
}
