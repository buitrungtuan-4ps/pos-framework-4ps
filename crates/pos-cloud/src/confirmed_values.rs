// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The one check that keeps a value that needs a recorded confirmation off every write but the
//! settings write
//! ([ADR-0169](../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)
//! decision 3).
//!
//! `PUT /admin/settings` records who confirmed such a value (decision 2). Every other write of a
//! store's configuration — the authored-layer write, the rollback, and the scheduled publish a
//! release uses — compares what the store runs before and after it with [`check`], and is refused
//! when it would make the store run such a value that it did not run before. A write that leaves
//! the value as it was passes, so a layer that holds a value confirmed earlier can be rewritten.
//!
//! The settings republish is the one writer that does not ask: every value in the settings store
//! passed decision 2 when it was written.

use pos_proto::settings::Setting;
use serde_json::Value;

/// A write that would make a store run a value that needs confirming, which it did not run before.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "this would make the store run `{setting_key}` = {value}, which is set only with its notice \
     confirmed: set it on Settings, which records who confirmed it"
)]
pub struct Unconfirmed {
    /// The setting.
    pub setting_key: String,
    /// The value that needs confirming.
    pub value: &'static str,
}

/// Whether a write that turns the effective document `before` into `after` makes the store run a
/// value one of `register`'s settings names in its `confirm`, which it did not run before.
///
/// `register` is [`pos_proto::settings::register`] on every route; a test passes its own.
///
/// # Errors
///
/// [`Unconfirmed`] for the first such setting, in the register's order.
pub fn check(register: &[Setting], before: &Value, after: &Value) -> Result<(), Unconfirmed> {
    for setting in register {
        let Some(confirm) = setting.confirm else {
            continue;
        };
        let runs = field(after, setting);
        if runs.is_some_and(|value| setting.needs_confirming(value))
            && field(before, setting) != runs
        {
            return Err(Unconfirmed {
                setting_key: setting.key(),
                value: confirm.value,
            });
        }
    }
    Ok(())
}

/// The setting's field in an effective document, if the document carries it.
fn field<'d>(document: &'d Value, setting: &Setting) -> Option<&'d Value> {
    document.get(setting.node)?.get(setting.field)
}

#[cfg(test)]
pub(crate) mod tests {
    use pos_proto::settings::{Confirm, Setting, SettingScope, SettingShape, register};
    use serde_json::json;

    use super::{Unconfirmed, check};

    /// A register with one setting whose `OWNER_ONE_PERSON` needs confirming, built here so the
    /// tests do not wait for the register's first such entry. The activator's tests use it too.
    pub(crate) fn confirming_register() -> Vec<Setting> {
        vec![Setting {
            node: "example",
            field: "drawer_owner",
            shape: SettingShape::Choice {
                values: vec!["OWNER_SHARED", "OWNER_ONE_PERSON"],
                default: "OWNER_SHARED",
                preset: None,
            },
            scopes: &[SettingScope::Tenant, SettingScope::Store],
            since: "0.15.0",
            confirm: Some(Confirm {
                value: "OWNER_ONE_PERSON",
                notice: "settings.item.example.drawer_owner.confirm",
            }),
            summary: "Whose a drawer is.",
        }]
    }

    fn running(owner: &str) -> serde_json::Value {
        json!({ "example": { "drawer_owner": owner, "other": 1 } })
    }

    #[test]
    fn a_write_that_starts_running_the_value_is_refused() {
        let refused = Err(Unconfirmed {
            setting_key: "example.drawer_owner".to_owned(),
            value: "OWNER_ONE_PERSON",
        });
        let register = confirming_register();
        assert_eq!(
            check(
                &register,
                &running("OWNER_SHARED"),
                &running("OWNER_ONE_PERSON")
            ),
            refused
        );
        assert_eq!(
            check(&register, &json!({}), &running("OWNER_ONE_PERSON")),
            refused,
            "a store that ran no such node yet"
        );
    }

    #[test]
    fn a_write_that_leaves_the_value_as_it_was_passes() {
        let register = confirming_register();
        let mut rewritten = running("OWNER_ONE_PERSON");
        rewritten["example"]["other"] = json!(2);
        assert_eq!(
            check(&register, &running("OWNER_ONE_PERSON"), &rewritten),
            Ok(())
        );
    }

    #[test]
    fn any_other_value_and_turning_it_off_pass() {
        let register = confirming_register();
        assert_eq!(
            check(&register, &json!({}), &running("OWNER_SHARED")),
            Ok(())
        );
        assert_eq!(
            check(
                &register,
                &running("OWNER_ONE_PERSON"),
                &running("OWNER_SHARED")
            ),
            Ok(())
        );
        assert_eq!(
            check(&register, &running("OWNER_ONE_PERSON"), &json!({})),
            Ok(())
        );
    }

    #[test]
    fn today_s_register_refuses_nothing() {
        assert_eq!(
            check(&register(), &json!({}), &running("OWNER_ONE_PERSON")),
            Ok(())
        );
    }
}
