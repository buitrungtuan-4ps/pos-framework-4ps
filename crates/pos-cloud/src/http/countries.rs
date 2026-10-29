// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Countries and locales (`/admin/countries`, `/admin/locales`): the read-only master data a
//! console picks a store's country and locale from (ADR-0074).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::console::require_permission;
use pos_country::CountryRegistry;
use pos_proto::determinism::ClockSource;

use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::settings::as_node_value;
use crate::settings::SettingValueSchema;

// --- Countries & locales (read-only master data, ADR-0074, Track M4) ------------------------------

/// One compiled country module as the console reads it: the code, human name, currency, preferred
/// language, number format, the default retention period, and the till facts its coinage and its
/// quoting habit fix. What the platform can serve — not a per-store setting, and not fiscalization.
///
/// The last three are what make a country **choosable** rather than merely listed
/// ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)): a store-settings form reads them
/// to fill its own fields in, so provisioning a Japanese shop does not depend on somebody remembering
/// that Japanese prices are tax-inclusive.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct CountryView {
    code: String,
    display_name: String,
    currency_code: String,
    /// How many decimal places that currency has
    /// ([ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)). Beside the
    /// code because the console's own money fields edit minor units and need the same fact the till
    /// does.
    currency_exponent: u8,
    default_language: String,
    decimal_separator: String,
    group_separator: String,
    digits_per_group: u8,
    default_retention_days: u16,
    prices_include_tax: bool,
    #[schema(required = true)]
    cash_rounding_increment: Option<i64>,
    cash_denominations: Vec<i64>,
    /// The settings a new store in the country starts with, in its units
    /// ([ADR-0183](../../../docs/adr/0183-a-country-pack-seeds-a-new-stores-settings.md)), which
    /// Store settings writes where no scope sets them when it publishes a locale naming the country.
    /// Empty for a pack that gives none.
    setting_defaults: Vec<CountrySettingDefault>,
}

/// One setting a country pack seeds: the register's key and the value, as the setting's node
/// carries it.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct CountrySettingDefault {
    setting_key: String,
    #[schema(value_type = SettingValueSchema)]
    value: serde_json::Value,
}

/// The state the country/locale reads share: the views computed once at start-up from the compiled
/// registry, plus the admin/clock the permission check needs.
#[derive(Clone)]
struct CountryState<A, C> {
    countries: Arc<Vec<CountryView>>,
    locales: Arc<Vec<String>>,
    admin: A,
    clock: C,
}

/// Builds the countries/locales sub-router ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), M4).
///
/// `GET /admin/countries` lists the compiled country modules; `GET /admin/locales` lists the content
/// locales the platform can serve (each module's preferred language plus the enforced `en` fallback,
/// which feeds the translation grid's column set). Both are read-only master data behind
/// [`ConsolePermission::Read`], computed once from the registry at start-up.
pub fn country_router<A, C>(registry: &CountryRegistry, admin: A, clock: C) -> Router
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let mut countries = Vec::new();
    // `en` is always present as the fallback (docs/pos-spec.md §9); a module's preferred language adds
    // to the catalogue.
    let mut locales: BTreeSet<String> = BTreeSet::new();
    locales.insert("en".to_owned());
    for module in registry.modules() {
        let pack = module.locale_pack();
        locales.insert(pack.default_language.as_str().to_owned());
        countries.push(CountryView {
            code: module.country_code().as_str().to_owned(),
            display_name: module.display_name().to_owned(),
            currency_code: pack.currency_code.as_str().to_owned(),
            currency_exponent: pack.currency_exponent,
            default_language: pack.default_language.as_str().to_owned(),
            decimal_separator: pack.number_format.decimal_separator.to_string(),
            group_separator: pack.number_format.group_separator.to_string(),
            digits_per_group: pack.number_format.digits_per_group,
            default_retention_days: pack.default_retention_days,
            prices_include_tax: pack.prices_include_tax,
            cash_rounding_increment: pack.cash_rounding_increment,
            cash_denominations: pack.cash_denominations.clone(),
            setting_defaults: module
                .setting_defaults()
                .into_iter()
                .map(|seeded| CountrySettingDefault {
                    setting_key: seeded.setting_key.to_owned(),
                    value: as_node_value(seeded.value),
                })
                .collect(),
        });
    }
    Router::new()
        .route("/admin/countries", get(admin_list_countries::<A, C>))
        .route("/admin/locales", get(admin_list_locales::<A, C>))
        .with_state(CountryState {
            countries: Arc::new(countries),
            locales: Arc::new(locales.into_iter().collect()),
            admin,
            clock,
        })
}

#[utoipa::path(
    get,
    path = "/admin/countries",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = [CountryView], description = "Every country module this build carries, as an array: \
                                      `code`, `display_name`, `currency_code` and \
                                      `currency_exponent`, `default_language`, the number format \
                                      (`decimal_separator`, `group_separator`, \
                                      `digits_per_group`), `default_retention_days`, and the till \
                                      facts a store's settings start from: `prices_include_tax`, \
                                      `cash_rounding_increment` and `cash_denominations`, in the \
                                      currency's minor units (ADR-0105), and `setting_defaults`, \
                                      the settings a new store in the country starts with \
                                      (ADR-0183). Fixed when the server \
                                      starts; the same for every tenant"),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "countries",
)]
/// A super-admin lists the compiled country modules.
async fn admin_list_countries<A, C>(
    State(state): State<CountryState<A, C>>,
    headers: HeaderMap,
) -> Response
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    (StatusCode::OK, Json(state.countries.as_ref().clone())).into_response()
}

#[utoipa::path(
    get,
    path = "/admin/locales",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = [String], description = "The content locales the platform can serve, as an array of \
                                      language tags: each country module's language, and `en`, the \
                                      fallback every catalogue has. The translation grid's columns"),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "countries",
)]
/// A super-admin lists the content locales the platform can serve.
async fn admin_list_locales<A, C>(
    State(state): State<CountryState<A, C>>,
    headers: HeaderMap,
) -> Response
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    (StatusCode::OK, Json(state.locales.as_ref().clone())).into_response()
}
