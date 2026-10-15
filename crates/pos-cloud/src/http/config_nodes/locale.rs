// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PUT /admin/config/locale`: a store's locale node, checked against its country (ADR-0074).

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_country::CountryRegistry;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::locale::{CountryCode, NumberFormat};
use pos_proto::money::CurrencyCode;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::{Json, PublishedConfig, locale_node, publish_config_nodes};

// --- Locale publish (`/admin/config/locale`, ADR-0074, Track M4) --------------------------------

/// The collaborators the locale-publish route needs: the config-tree store the `locale` node is
/// written onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigLocaleState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    /// The country packs' values a store's `locale` node carries, computed once from the compiled
    /// registry at start-up — the same shape `CountryState` holds its views in, and for the same
    /// reason: the registry does not change while the process runs.
    facts: Arc<PackedFacts>,
}

/// The country-pack values that ride a store's `locale` node, indexed by whatever identifies each.
///
/// They are keyed differently on purpose, and the difference is the fact each one is *about*:
///
/// * the **exponent** belongs to the currency — the cent is two places wherever the dollar is spent
///   — and a store picks its currency on the settings form independently of where it sits, so a
///   country key would hand a Vietnamese store trading in dollars the đồng's zero;
/// * the **retention period** belongs to the jurisdiction, because it answers to that country's
///   privacy law rather than to what the shop takes payment in.
#[derive(Debug, Default)]
pub(in crate::http) struct PackedFacts {
    /// How many decimal places each currency the platform has a pack for has
    /// ([ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)).
    pub(in crate::http) exponents: std::collections::BTreeMap<CurrencyCode, u8>,
    /// How long each country's pack says a store keeps a personal record, in days
    /// ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)).
    pub(in crate::http) retention_days: std::collections::BTreeMap<CountryCode, u16>,
    /// How each country writes a number — its decimal mark, its group mark, and how many digits go
    /// in a group ([ADR-0136](../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)).
    ///
    /// Keyed on the country, like the retention period: what marks a figure carries is a convention
    /// of the place, not of the money. Vietnam writes `1.234.567,50` whether the amount is in đồng
    /// or in dollars.
    pub(in crate::http) number_formats: std::collections::BTreeMap<CountryCode, NumberFormat>,
    /// The language each country's pack starts a store in, as a BCP 47 tag, which a receipt set to
    /// `RECEIPT_LANGUAGE_COUNTRY` prints in
    /// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
    /// Keyed on the country: it is the language of the place, whatever the store trades in.
    pub(in crate::http) languages: std::collections::BTreeMap<CountryCode, String>,
}

/// Builds those from the compiled country modules.
///
/// Two packs naming one currency must agree about its exponent, so the first wins and the order is
/// the registry's — deterministic rather than merely unspecified. A country appears once, so its
/// retention period raises no such question.
pub(in crate::http) fn packed_facts(registry: &CountryRegistry) -> PackedFacts {
    let mut facts = PackedFacts::default();
    for module in registry.modules() {
        let pack = module.locale_pack();
        facts
            .exponents
            .entry(pack.currency_code)
            .or_insert(pack.currency_exponent);
        facts
            .retention_days
            .insert(module.country_code(), pack.default_retention_days);
        facts
            .number_formats
            .insert(module.country_code(), pack.number_format.clone());
        facts.languages.insert(
            module.country_code(),
            pack.default_language.as_str().to_owned(),
        );
    }
    facts
}

/// A super-admin sets a store's locale settings: the `(tenant, store)` and the currency, IANA
/// timezone, and business-date cutoff hour. These drive money display and business-date derivation on
/// the edge (ADR-0014); until M4 they were hardcoded to VND/UTC/04:00 in the edge bootstrap.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct PublishLocaleRequest {
    pub(in crate::http) tenant_id: String,
    pub(in crate::http) store_id: String,
    /// Which country this store is in, as an ISO 3166-1 alpha-2 code
    /// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// **Required, and that is a deliberate break** from the shape this route had before. It sits
    /// beside `currency_code` and `timezone` because [D21](../../../docs/roadmap-v3.md) puts all
    /// three in the same place — attributes of a *store* — and this node already carried the other
    /// two. It was simply missing.
    ///
    /// Required rather than optional because a publish rebuilds this node wholesale, so an optional
    /// field has only two possible meanings and both are wrong. "Absent clears it" makes an edit to
    /// the cutoff hour erase where a store's data rests, silently. "Absent keeps it" makes this
    /// field behave unlike `display_language` one line down, which *does* clear when omitted — a
    /// trap for the next reader of this struct. Required leaves no absent case at all: a caller
    /// that does not send one is refused by name, loudly, and fixes one line.
    ///
    /// `#[serde(default)]` and *not* a bare required field, deliberately. A missing field makes
    /// axum's `Json` extractor answer `422` with its own plain body, which is off the
    /// [AIP-193](../../../docs/naming-and-api.md) envelope the whole `/admin` surface was moved
    /// onto. Defaulting to the empty string routes "absent" through the same check as "malformed",
    /// so both answer one `400` naming `country_code` and saying what it accepts.
    #[serde(default)]
    pub(in crate::http) country_code: String,
    pub(in crate::http) currency_code: String,
    pub(in crate::http) timezone: String,
    pub(in crate::http) cutoff_hour: u8,
    /// The store's display language (a locale code, e.g. `"vi"`), which selects a compiled item's
    /// per-locale name at the edge (ADR-0074). Optional; when absent or blank the store shows each
    /// item's default name, exactly as before.
    #[serde(default)]
    pub(in crate::http) display_language: Option<String>,
    /// Whether this store's menu prices already contain their tax
    /// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)): Japan's 税込 and
    /// India's MRP are `true`, Vietnam's `++` is `false`. Defaults to `false`, which is what every
    /// store did before the field existed.
    #[serde(default)]
    pub(in crate::http) prices_include_tax: bool,
    /// What the grand total is rounded to in cash, in minor units — `1000` for Vietnam's thousand
    /// đồng, `100` for India's rupee, absent for Japan
    /// ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)).
    #[serde(default)]
    pub(in crate::http) cash_rounding_increment: Option<i64>,
    /// The notes the till offers as quick-cash keys, in minor units. Absent or empty means the exact
    /// amount only, which is the honest answer rather than a guess.
    #[serde(default)]
    pub(in crate::http) cash_denominations: Vec<i64>,
}

/// Builds the locale-publish sub-router ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), M4).
///
/// One route: validate a store's currency / IANA timezone / cutoff hour with the domain constructors,
/// write them as the store's `locale` config node, and version it through the config tree — the
/// node-merge the other publishes use, so sibling nodes survive. Behind
/// [`ConsolePermission::PublishConfig`]. The edge applies the node to its session's currency, timezone,
/// and cutoff.
pub fn config_locale_router<Cfg, A, C>(
    registry: &CountryRegistry,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/locale",
            axum::routing::put(admin_publish_locale::<Cfg, A, C>),
        )
        .with_state(ConfigLocaleState {
            config_trees,
            admin,
            clock,
            audit,
            facts: Arc::new(packed_facts(registry)),
        })
}

/// Checks the till-money half of a locale publish and returns the denominations to store
/// ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)).
///
/// Both refusals are about a number that cannot mean what it says. A rounding increment of zero or
/// less is a typo rather than a posture — `round_to_increment` needs a non-zero step, and "round to
/// nothing" is expressed by omitting the field — and a note worth nothing is not a note. Refused
/// here rather than dropped at the edge, so the person who typed it finds out.
///
/// The denominations come back sorted and de-duplicated rather than as given: the till lays its keys
/// out in this order, and a repeated note would be a repeated button.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub(in crate::http) fn checked_denominations(
    request: &PublishLocaleRequest,
) -> Result<Vec<i64>, Response> {
    if request
        .cash_rounding_increment
        .is_some_and(|increment| increment <= 0)
    {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "cash_rounding_increment must be a positive number of minor units, or absent for no \
             rounding",
            &[("cash_rounding_increment", "OUT_OF_RANGE")],
        ));
    }
    if request
        .cash_denominations
        .iter()
        .any(|denomination| *denomination <= 0)
    {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "cash_denominations are note values in minor units and must all be positive",
            &[("cash_denominations", "OUT_OF_RANGE")],
        ));
    }
    let mut denominations = request.cash_denominations.clone();
    denominations.sort_unstable();
    denominations.dedup();
    Ok(denominations)
}

/// The store's locale settings, each field checked with its own domain constructor, as the `locale`
/// node they become.
///
/// Separate from the handler because it is the whole of what this route validates: five domain
/// parses and one optional key, with no session, no store and no clock in sight. A refusal is the
/// finished [`Response`] rather than an error type, because each one names its own field and its
/// own code and there is no second caller that would want to phrase them differently.
///
/// # Errors
///
/// An AIP-193 `400` naming the first field that does not parse.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub(in crate::http) fn checked_locale_node(
    request: &PublishLocaleRequest,
    facts: &PackedFacts,
) -> Result<serde_json::Value, Response> {
    let (country, currency, denominations) = locale_node::checked_locale_fields(request)?;

    let mut locale_value = serde_json::json!({
        // Upper-cased by `CountryCode::parse` on the way in, so the fleet console's comparison
        // against a store's `region_country` — written through the same parse — is an equality test
        // rather than a case fold (ADR-0114).
        "country_code": country.as_str(),
        "currency_code": request.currency_code,
        "timezone": request.timezone,
        "cutoff_hour": request.cutoff_hour,
        "prices_include_tax": request.prices_include_tax,
        "cash_rounding_increment": request.cash_rounding_increment,
        "cash_denominations": denominations,
    });
    locale_node::add_country_pack_facts(&mut locale_value, facts, country, currency);
    // The display language is optional: include it only when a non-blank code was given, so a store
    // that never sets one keeps a clean node and shows each item's default name (ADR-0074).
    if let Some(language) = request
        .display_language
        .as_deref()
        .map(str::trim)
        .filter(|language| !language.is_empty())
        && let serde_json::Value::Object(map) = &mut locale_value
    {
        map.insert(
            "display_language".to_owned(),
            serde_json::Value::String(language.to_owned()),
        );
    }
    Ok(locale_value)
}

/// Validates a store's locale settings and writes them as its `locale` node, versioned — the same
/// load→merge→publish→version shape as the other node publishes. Each field is checked with its domain
/// constructor before anything is written (a real IANA timezone against the tz database, a 3-letter
/// currency, an hour in `0..=23`), so a bad value is a `400` naming it rather than a stored error.
async fn admin_publish_locale<Cfg, A, C>(
    State(state): State<ConfigLocaleState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishLocaleRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let locale_value = match checked_locale_node(&request, &state.facts) {
        Ok(node) => node,
        Err(refusal) => return refusal,
    };

    // Set the `locale` key on the store's Store layer (index 2) and re-publish it, preserving the
    // other Store-level keys.
    let nodes = vec![("locale".to_owned(), locale_value.clone())];

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
        audit_action(
            &state.audit,
            &state.clock,
            &context,
            Some(tenant_id),
            "config.locale.publish",
            "store",
            &store_id.to_string(),
            None,
            Some(locale_value),
        )
        .await;
        (
            StatusCode::OK,
            Json(PublishedConfig {
                config_version_id: id.to_string(),
            }),
        )
            .into_response()
    }
}
