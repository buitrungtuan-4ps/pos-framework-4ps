// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `400` a settings write the register refuses becomes: the field at fault and the reason the
//! console explains it by.

use axum::response::Response;

use pos_proto::ErrorStatus;
use pos_proto::settings::ValueRefusal;

use super::api_error_with_details;
use crate::settings::SettingRefusal;

/// The `400` a value the register refuses becomes, naming the field at fault.
pub(super) fn setting_refusal_response(refusal: &SettingRefusal) -> Response {
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
