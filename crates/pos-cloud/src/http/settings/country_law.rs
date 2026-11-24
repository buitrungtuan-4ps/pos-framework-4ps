// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A setting value a store's country forbids, refused at the write
//! ([ADR-0223](../../../../docs/adr/0223-a-weighed-item-is-priced-by-its-weight.md) decision 3).
//!
//! Each compiled-in pack names the values its law refuses (`forbidden_settings`), such as
//! `weighing.manual_entry` on in `vn`, `jp` and `in`. A write is refused when any store it reaches
//! has published a country that forbids the value; a store whose country is not yet known does not
//! block it, because the edge refuses the act under its own pack whatever the setting says.
//!
//! An e-invoice delay past the country's legal limit is refused the same way
//! (`SETTING_BEYOND_COUNTRY_LIMIT`), and the edge clamps every schedule to the limit besides.

use std::collections::BTreeMap;

use axum::response::Response;
use pos_cloud_config::settings::SettingRefusal;
use pos_country::CountryRegistry;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::locale::CountryCode;

use super::presets::{as_node_value, store_country};
use crate::config_tree::ConfigTreeStore;
use crate::http::setting_refusal::setting_refusal_response;

/// What a country's law refuses of one setting.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Refused {
    /// This value, as the setting's node carries it.
    Value(serde_json::Value),
    /// Any whole number above this: the e-invoice delay past the country's legal limit
    /// ([ADR-0229](../../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md) decision 2).
    Above(i64),
}

impl Refused {
    fn refuses(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::Value(refused) => refused == value,
            Self::Above(limit) => value.as_i64().is_some_and(|number| number > *limit),
        }
    }
}

/// Per country, each setting key and what its law refuses of it.
pub(super) type CountryForbidden = BTreeMap<CountryCode, Vec<(String, Refused)>>;

/// Reads every compiled-in pack's `forbidden_settings` into [`CountryForbidden`].
pub(super) fn country_forbidden(countries: &CountryRegistry) -> CountryForbidden {
    countries
        .modules()
        .map(|module| {
            let mut forbidden: Vec<(String, Refused)> = module
                .forbidden_settings()
                .into_iter()
                .map(|entry| {
                    let value = Refused::Value(as_node_value(entry.value));
                    (entry.setting_key.to_owned(), value)
                })
                .collect();
            if let Some(limit) = module.einvoice_issue_limit() {
                let longest = i64::from(limit.max_delay_minutes());
                forbidden.push(("einvoice.delay_minutes".to_owned(), Refused::Above(longest)));
            }
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
    let forbidding: Vec<(CountryCode, bool)> = forbidden
        .iter()
        .filter_map(|(country, entries)| {
            entries
                .iter()
                .find(|(key, refused)| key == setting_key && refused.refuses(value))
                .map(|(_, refused)| (*country, matches!(refused, Refused::Above(_))))
        })
        .collect();
    if forbidding.is_empty() {
        return Ok(());
    }
    for store_id in stores {
        let country = store_country(config_trees, tenant_id, *store_id).await?;
        let found = country.and_then(|country| {
            forbidding
                .iter()
                .find(|(forbidding, _)| *forbidding == country)
        });
        if let Some((country_code, beyond_limit)) = found {
            let (setting_key, country_code, store_id) =
                (setting_key.to_owned(), *country_code, store_id.to_string());
            let refusal = if *beyond_limit {
                SettingRefusal::BeyondCountryLimit {
                    setting_key,
                    country_code,
                    store_id,
                }
            } else {
                SettingRefusal::ForbiddenByCountry {
                    setting_key,
                    country_code,
                    store_id,
                }
            };
            return Err(setting_refusal_response(&refusal));
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "country-vn"))]
mod tests {
    use pos_proto::locale::CountryCode;

    use super::{Refused, country_forbidden};

    #[test]
    fn a_delay_past_the_countrys_limit_is_refused_and_one_within_it_is_not() {
        let countries = pos_country::country_registry! { pos_country_vn::Vietnam };
        let forbidden = country_forbidden(&countries);
        let vietnam = forbidden
            .get(&CountryCode::VN)
            .expect("Vietnam is compiled in");
        let (_, limit) = vietnam
            .iter()
            .find(|(key, _)| key == "einvoice.delay_minutes")
            .expect("Vietnam states an issue limit");
        assert_eq!(*limit, Refused::Above(1_439));
        assert!(limit.refuses(&serde_json::json!(1_440)));
        assert!(!limit.refuses(&serde_json::json!(1_439)));
        assert!(!limit.refuses(&serde_json::json!(30)));
        assert!(
            vietnam
                .iter()
                .any(|(key, refused)| key == "weighing.manual_entry"
                    && refused.refuses(&serde_json::json!(true))),
            "the forbidden values still are"
        );
    }
}
