// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A setting value a store's country forbids, refused at the write
//! ([ADR-0223](../../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md) decision 3).
//!
//! Each compiled-in pack names the values its law refuses (`forbidden_settings`), such as
//! `weighing.manual_entry` on in `vn`, `jp` and `in`. A write is refused when any store it reaches
//! has published a country that forbids the value; a store whose country is not yet known does not
//! block it, because the edge refuses the act under its own pack whatever the setting says.

use std::collections::BTreeMap;

use axum::response::Response;
use pos_cloud_config::settings::SettingRefusal;
use pos_country::CountryRegistry;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::locale::CountryCode;

use super::presets::{as_node_value, store_country};
use crate::config_tree::ConfigTreeStore;
use crate::http::setting_refusal::setting_refusal_response;

/// Per country, each setting key and the values its law forbids, as a setting's node carries them.
pub(super) type CountryForbidden = BTreeMap<CountryCode, Vec<(String, serde_json::Value)>>;

/// Reads every compiled-in pack's `forbidden_settings` into [`CountryForbidden`].
pub(super) fn country_forbidden(countries: &CountryRegistry) -> CountryForbidden {
    countries
        .modules()
        .map(|module| {
            let forbidden = module
                .forbidden_settings()
                .into_iter()
                .map(|entry| (entry.setting_key.to_owned(), as_node_value(entry.value)))
                .collect();
            (module.country_code(), forbidden)
        })
        .collect()
}

/// The `400` a write of `value` to `setting_key` reaching `stores` answers when a store among them
/// is in a country that forbids it, the `503` when a store's country cannot be read, or `Ok`.
pub(super) async fn law_refusal<Cfg: ConfigTreeStore>(
    forbidden: &CountryForbidden,
    config_trees: &Cfg,
    tenant_id: TenantId,
    stores: &[StoreId],
    (setting_key, value): (&str, &serde_json::Value),
) -> Result<(), Response> {
    let forbidding: Vec<CountryCode> = forbidden
        .iter()
        .filter(|(_, entries)| {
            entries
                .iter()
                .any(|(key, refused)| key == setting_key && refused == value)
        })
        .map(|(country, _)| *country)
        .collect();
    if forbidding.is_empty() {
        return Ok(());
    }
    for store_id in stores {
        let country = store_country(config_trees, tenant_id, *store_id).await?;
        if let Some(country_code) = country.filter(|country| forbidding.contains(country)) {
            return Err(setting_refusal_response(
                &SettingRefusal::ForbiddenByCountry {
                    setting_key: setting_key.to_owned(),
                    country_code,
                    store_id: store_id.to_string(),
                },
            ));
        }
    }
    Ok(())
}
