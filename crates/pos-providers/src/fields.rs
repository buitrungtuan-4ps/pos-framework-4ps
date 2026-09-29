// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Fields most HTTP providers share, written once so the console labels them the same way
//! everywhere and a stored key means one thing across vendors.
//!
//! Extracted when the third adapter needed them (`docs/design-principles.md`): the two couriers and
//! the ERP each read a base URL and a timeout, and nothing else yet.

use crate::{FieldKind, SettingField};

/// The vendor API's base URL. `https` only, because a connector sends the tenant's credentials
/// to it.
pub const BASE_URL: SettingField = SettingField {
    key: "base_url",
    label_key: "provider.field.base_url",
    kind: FieldKind::Url,
    required: true,
};

/// How long one request to the vendor may take, in seconds, before it is read as the vendor being
/// unreachable. Optional: an adapter that is not given one uses its own default.
pub const TIMEOUT_SECONDS: SettingField = SettingField {
    key: "timeout_seconds",
    label_key: "provider.field.timeout_seconds",
    kind: FieldKind::Number { min: 1, max: 120 },
    required: false,
};
