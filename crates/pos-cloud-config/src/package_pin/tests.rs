// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::settings::SettingScope;
use pos_proto::time::Timestamp;
use pos_proto::wire_enum::Open;
use serde_json::{Value, json};

use super::{ChoiceRefusal, broadest_lock, locked_above, may_choose};
use crate::settings::{SettingValue, StorePlacement, resolve_for_store};

const V1: &str = "01J00000000000000000000V01";
const V2: &str = "01J00000000000000000000V02";

fn placement() -> StorePlacement {
    StorePlacement {
        tenant_id: "T".to_owned(),
        brand_id: Some("B".to_owned()),
        store_group_ids: vec!["G".to_owned()],
        store_id: "S".to_owned(),
    }
}

fn value(key: &str, scope: SettingScope, scope_id: &str, value: Value) -> SettingValue {
    SettingValue {
        setting_key: key.to_owned(),
        scope: Open::from_known(scope),
        scope_id: scope_id.to_owned(),
        value,
        update_time: Timestamp::EPOCH,
        pack_country_code: None,
    }
}

fn mode(scope: SettingScope, scope_id: &str, token: &str) -> SettingValue {
    value("package.mode", scope, scope_id, json!(token))
}

fn resolved_field(values: &[SettingValue], field: &str) -> Option<Value> {
    resolve_for_store(values, &placement())
        .into_iter()
        .find(|setting| setting.node == "package" && setting.field == field)
        .map(|setting| setting.value)
}

/// A brand's lock is the broadest that reaches the store, a lock at another brand reaches nothing,
/// and a store's own lock is narrower than both.
#[test]
fn the_broadest_lock_that_reaches_the_store_counts() {
    let values = [
        mode(SettingScope::Store, "S", "PACKAGE_MODE_LOCKED"),
        mode(SettingScope::Brand, "B", "PACKAGE_MODE_LOCKED"),
        mode(SettingScope::Brand, "OTHER", "PACKAGE_MODE_LOCKED"),
        mode(SettingScope::Tenant, "T", "PACKAGE_MODE_CHOOSE"),
    ];
    assert_eq!(broadest_lock(&values, &placement()), Some(2));
    assert!(locked_above(&values, &placement(), SettingScope::Store));
    assert!(locked_above(
        &values,
        &placement(),
        SettingScope::StoreGroup
    ));
    assert!(
        !locked_above(&values, &placement(), SettingScope::Brand),
        "the lock's own scope may change or lift it"
    );
    assert!(!locked_above(&values, &placement(), SettingScope::Tenant));
    assert_eq!(broadest_lock(&values[2..], &placement()), None);
}

/// Under a brand's lock, a store's values written before it are held back: the store runs the
/// brand's version. Lifted, the store's own values reach it again.
#[test]
fn a_lock_holds_back_every_narrower_package_value() {
    let mut values = vec![
        mode(SettingScope::Brand, "B", "PACKAGE_MODE_LOCKED"),
        value(
            "package.locked_version_id",
            SettingScope::Brand,
            "B",
            json!(V1),
        ),
        mode(SettingScope::Store, "S", "PACKAGE_MODE_CHOOSE"),
        value(
            "package.locked_version_id",
            SettingScope::Store,
            "S",
            json!(V2),
        ),
        value("shift.blind_close", SettingScope::Store, "S", json!(true)),
    ];
    assert_eq!(
        resolved_field(&values, "mode"),
        Some(json!("PACKAGE_MODE_LOCKED"))
    );
    assert_eq!(
        resolved_field(&values, "locked_version_id"),
        Some(json!(V1))
    );
    assert!(
        resolve_for_store(&values, &placement())
            .iter()
            .any(|setting| setting.node == "shift"),
        "another node's store value is not held back"
    );

    values.remove(0);
    assert_eq!(
        resolved_field(&values, "mode"),
        Some(json!("PACKAGE_MODE_CHOOSE"))
    );
    assert_eq!(
        resolved_field(&values, "locked_version_id"),
        Some(json!(V2))
    );
}

/// A store chooses only in `PACKAGE_MODE_CHOOSE`, and only an allowed version.
#[test]
fn a_store_chooses_only_an_allowed_version_while_choosing() {
    let choosing = [
        mode(SettingScope::Tenant, "T", "PACKAGE_MODE_CHOOSE"),
        value(
            "package.allowed_version_ids",
            SettingScope::Brand,
            "B",
            json!([V1]),
        ),
    ];
    let settings = resolve_for_store(&choosing, &placement());
    assert_eq!(may_choose(&settings, V1), None);
    assert_eq!(may_choose(&settings, V2), Some(ChoiceRefusal::NotAllowed));

    let locked = [
        mode(SettingScope::Tenant, "T", "PACKAGE_MODE_LOCKED"),
        value(
            "package.allowed_version_ids",
            SettingScope::Tenant,
            "T",
            json!([V1]),
        ),
    ];
    let settings = resolve_for_store(&locked, &placement());
    assert_eq!(may_choose(&settings, V1), Some(ChoiceRefusal::NotChoosing));
    assert_eq!(may_choose(&[], V1), Some(ChoiceRefusal::NotChoosing));
}
