// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A rollback is a write: the setting values a rollback restores are written back as the store's
//! own values, so the next settings publish keeps them
//! ([ADR-0246](../../../docs/adr/0246-a-setting-has-one-source-and-publish-and-rollback-take-a-scope.md)
//! decision 1).
//!
//! A rollback restores a store's effective document, but the values the console wrote live in the
//! settings store, and a settings publish recompiles them onto the store. Without this the next
//! publish, of any setting, silently put back what the rollback took away.

use pos_proto::settings::{SettingScope, register};
use pos_proto::time::Timestamp;
use pos_proto::wire_enum::Open;
use serde_json::Value;

use crate::settings::{ResolvedSetting, SettingValue, validate};

/// What a rollback writes so its settings hold, and what it cannot hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RollbackHolds {
    /// The store-scope values to write, one per setting the restored version runs differently from
    /// what the settings store resolves for the store now.
    pub writes: Vec<SettingValue>,
    /// The settings that differ but cannot be held at the store: the setting takes no store-scope
    /// value, or the register no longer takes the restored one. The next settings publish puts the
    /// resolved value back for these, so the console names them.
    pub not_held: Vec<String>,
}

/// The values to write at the store's own scope so that `restored`, the effective document a
/// rollback restored, is what the settings resolve to for that store.
///
/// For each registered setting, the restored value is the one at its node and field, or the
/// register default where the restored version had none (the store ran its default). Where that
/// differs from `resolved`, today's values for the store, it is written for `store_id` at
/// `now`. A setting that already resolves to the restored value is left alone, so a rollback pins
/// the store only where it has to.
#[must_use]
pub fn rollback_holds(
    restored: &Value,
    resolved: &[ResolvedSetting],
    store_id: &str,
    now: Timestamp,
) -> RollbackHolds {
    let mut holds = RollbackHolds::default();
    for setting in register() {
        let restored_value = restored
            .get(setting.node)
            .and_then(|node| node.get(setting.field));
        let resolved_value = resolved
            .iter()
            .find(|value| value.node == setting.node && value.field == setting.field)
            .map(|value| &value.value);
        let target = restored_value
            .cloned()
            .unwrap_or_else(|| setting.default_value());
        let runs_now = resolved_value
            .cloned()
            .unwrap_or_else(|| setting.default_value());
        if target == runs_now {
            continue;
        }
        let key = setting.key();
        if validate(&key, SettingScope::Store, &target).is_err() {
            holds.not_held.push(key);
            continue;
        }
        holds.writes.push(SettingValue {
            setting_key: key,
            scope: Open::from_known(SettingScope::Store),
            scope_id: store_id.to_owned(),
            value: target,
            update_time: now,
            pack_country_code: None,
        });
    }
    holds
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const STORE: &str = "01KSTORE00000000000000000A";

    fn now() -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(1_000).expect("a valid instant")
    }

    fn resolved(node: &'static str, field: &'static str, value: Value) -> ResolvedSetting {
        ResolvedSetting {
            node,
            field,
            value,
            scope: SettingScope::Tenant,
            scope_id: "01KTENANT0000000000000000A".to_owned(),
        }
    }

    #[test]
    fn a_restored_value_the_settings_no_longer_give_is_written_at_the_store() {
        let restored =
            json!({ "einvoice": { "issue_moment": "EINVOICE_ISSUE_MOMENT_ON_REQUEST" } });
        let today = [resolved(
            "einvoice",
            "issue_moment",
            json!("EINVOICE_ISSUE_MOMENT_IMMEDIATE"),
        )];
        let holds = rollback_holds(&restored, &today, STORE, now());
        assert_eq!(holds.not_held, Vec::<String>::new());
        assert_eq!(holds.writes.len(), 1);
        let write = &holds.writes[0];
        assert_eq!(write.setting_key, "einvoice.issue_moment");
        assert_eq!(write.scope.known(), SettingScope::Store);
        assert_eq!(write.scope_id, STORE);
        assert_eq!(write.value, json!("EINVOICE_ISSUE_MOMENT_ON_REQUEST"));
        assert_eq!(write.update_time, now());
    }

    #[test]
    fn a_setting_the_restored_version_did_not_set_comes_back_as_its_default() {
        let today = [resolved(
            "einvoice",
            "issue_moment",
            json!("EINVOICE_ISSUE_MOMENT_ON_REQUEST"),
        )];
        let holds = rollback_holds(&json!({}), &today, STORE, now());
        let default = register()
            .into_iter()
            .find(|setting| setting.key() == "einvoice.issue_moment")
            .expect("registered")
            .default_value();
        assert_eq!(holds.writes.len(), 1);
        assert_eq!(holds.writes[0].value, default);
    }

    #[test]
    fn a_setting_that_already_resolves_to_the_restored_value_is_left_alone() {
        let restored =
            json!({ "einvoice": { "issue_moment": "EINVOICE_ISSUE_MOMENT_ON_REQUEST" } });
        let today = [resolved(
            "einvoice",
            "issue_moment",
            json!("EINVOICE_ISSUE_MOMENT_ON_REQUEST"),
        )];
        assert_eq!(
            rollback_holds(&restored, &today, STORE, now()),
            RollbackHolds::default()
        );
        assert_eq!(
            rollback_holds(&json!({}), &[], STORE, now()),
            RollbackHolds::default(),
            "nothing set before or now: every store runs its defaults"
        );
    }

    #[test]
    fn a_restored_value_the_register_refuses_today_is_named_not_written() {
        let restored =
            json!({ "einvoice": { "issue_moment": "EINVOICE_ISSUE_MOMENT_NEVER_HEARD_OF" } });
        let holds = rollback_holds(&restored, &[], STORE, now());
        assert_eq!(holds.writes, Vec::new());
        assert_eq!(holds.not_held, vec!["einvoice.issue_moment".to_owned()]);
    }
}
