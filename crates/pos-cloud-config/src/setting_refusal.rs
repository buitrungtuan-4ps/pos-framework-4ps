// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `400` a settings write the register refuses becomes: the field at fault and the reason the
//! console explains it by. And the `400` for any other write that would make a store run a value
//! that needs a recorded confirmation (ADR-0169 decision 3).

use axum::response::Response;

use pos_proto::ErrorStatus;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::settings::{Setting, ValueRefusal, register};
use serde_json::Value;

use pos_cloud_api::conditional::version_mismatch;
use pos_cloud_api::response::api_error_with_details;

use crate::config_tree::{ConfigTree, ConfigTreeStore, ConfigValidator};
use crate::confirmed_values::Unconfirmed;
use crate::route::config_store_error_response;
use crate::settings::SettingRefusal;
use crate::version::{UpdateOutcome, Version};

/// The `400` a value the register refuses becomes, naming the field at fault.
#[must_use]
pub fn setting_refusal_response(refusal: &SettingRefusal) -> Response {
    let (field, reason) = match refusal {
        SettingRefusal::UnknownSetting(_) => ("setting_key", "UNKNOWN_SETTING"),
        SettingRefusal::ScopeNotAllowed { .. } => ("scope", "SCOPE_NOT_ALLOWED"),
        SettingRefusal::ConfirmationRequired { .. } => {
            ("notice_confirmed", "CONFIRMATION_REQUIRED")
        }
        SettingRefusal::ValueNotAllowed { refusal, .. } => (
            "value",
            match refusal {
                ValueRefusal::NotOneOfItsValues => "INVALID_ENUM_VALUE",
                ValueRefusal::OutOfRange => "OUT_OF_RANGE",
                ValueRefusal::NotAWholeNumber | ValueRefusal::NotTrueOrFalse => "INVALID_VALUE",
            },
        ),
    };
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        refusal.to_string(),
        &[(field, reason)],
    )
}

/// The `400` a layer write, a rollback or a release's schedule answers when it would make a store run
/// a value that needs a recorded confirmation, which it did not run before: the setting's key, with
/// `CONFIRMATION_REQUIRED`, and a message that points to Settings. A release names the store, since
/// it reaches many.
#[must_use]
pub fn unconfirmed_response(unconfirmed: &Unconfirmed, store_id: Option<StoreId>) -> Response {
    let message = match store_id {
        Some(store_id) => format!("store {store_id}: {unconfirmed}"),
        None => unconfirmed.to_string(),
    };
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        message,
        &[(unconfirmed.setting_key.as_str(), "CONFIRMATION_REQUIRED")],
    )
}

/// Whether a write takes [`crate::confirmed_values::check`]: every write does but the settings
/// republish, whose every value passed the settings write's own check when it was written
/// (ADR-0169 decisions 2 and 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmCheck {
    /// A layer write, a rollback, a node publish, single or in a batch.
    Required,
    /// The settings republish.
    SettingsRepublish,
}

/// [`crate::confirmed_values::check`] against the register, for a write that turns what the store
/// runs from `before` into `after`, as the `400` to answer.
///
/// # Errors
///
/// The `400` naming the first setting that needs a recorded confirmation.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub fn confirmed(check: ConfirmCheck, before: &Value, after: &Value) -> Result<(), Response> {
    confirmed_against(&register(), check, before, after)
}

/// [`confirmed`] against `register`, which a test passes its own of.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
fn confirmed_against(
    register: &[Setting],
    check: ConfirmCheck,
    before: &Value,
    after: &Value,
) -> Result<(), Response> {
    if check == ConfirmCheck::SettingsRepublish {
        return Ok(());
    }
    crate::confirmed_values::check(register, before, after)
        .map_err(|unconfirmed| unconfirmed_response(&unconfirmed, None))
}

/// Saves a store's tree after a layer write or a rollback, once [`crate::confirmed_values::check`]
/// passes against the register: `before` is what the store ran when the tree was read. Nothing is
/// saved for a write it refuses.
///
/// `saved` is the tenant, the store, and the version the tree was read at. That precondition is the
/// one the store evaluates at write time, and it is what prevents two writes interleaving: an
/// `If-Match` compared against the handler's own read can already have been overtaken.
///
/// # Errors
///
/// The `400` for a value that needs a recorded confirmation, the `412` when the tree changed since
/// it was read, or the `503` when the store failed.
pub async fn save_if_confirmed<Cfg, V>(
    config_trees: &Cfg,
    (tenant_id, store_id, read_at): (TenantId, StoreId, Option<&Version>),
    before: &Value,
    tree: &ConfigTree<V>,
) -> Result<(), Response>
where
    Cfg: ConfigTreeStore,
    V: ConfigValidator,
{
    confirmed(ConfirmCheck::Required, before, &tree.effective())?;
    match config_trees
        .save(tenant_id, store_id, &tree.state(), read_at)
        .await
    {
        Ok(UpdateOutcome::Updated(_)) => Ok(()),
        Ok(UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound) => Err(version_mismatch()),
        Err(error) => Err(config_store_error_response(&error)),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use serde_json::json;

    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    use super::{ConfirmCheck, confirmed_against, unconfirmed_response};
    use crate::confirmed_values::Unconfirmed;
    use crate::confirmed_values::tests::confirming_register;

    /// What a node publish, single or in a batch, and the settings republish make of a write that
    /// starts running a value that needs a recorded confirmation.
    #[test]
    fn a_node_publish_is_checked_and_only_the_settings_republish_is_not() {
        let register = confirming_register();
        let before = json!({ "example": { "drawer_owner": "OWNER_SHARED" } });
        let after = json!({ "example": { "drawer_owner": "OWNER_ONE_PERSON" } });
        let refused = confirmed_against(&register, ConfirmCheck::Required, &before, &after)
            .expect_err("a node publish is refused");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert!(
            confirmed_against(&register, ConfirmCheck::SettingsRepublish, &before, &after).is_ok(),
            "the settings republish publishes what Settings recorded"
        );
    }

    #[tokio::test]
    async fn a_write_that_would_run_an_unconfirmed_value_names_the_setting() {
        let response = unconfirmed_response(
            &Unconfirmed {
                setting_key: "example.drawer_owner".to_owned(),
                value: "OWNER_ONE_PERSON",
            },
            Some(StoreId::new(Ulid::from_u128(7))),
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("collect the refusal");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("a JSON envelope");
        assert_eq!(
            body["error"]["details"],
            json!([{ "field": "example.drawer_owner", "reason": "CONFIRMATION_REQUIRED" }])
        );
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("Settings"), "{message}");
        assert!(
            message.starts_with(&format!("store {}", StoreId::new(Ulid::from_u128(7)))),
            "a release names the store: {message}"
        );
    }
}
