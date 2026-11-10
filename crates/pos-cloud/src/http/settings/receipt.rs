// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a write of `receipt.templates` is checked against beyond the register: the tenant's
//! translation grid and the sections each compiled-in country requires
//! ([ADR-0207](../../../../docs/adr/0207-a-receipt-is-a-structured-template-per-store-and-channel.md)
//! decision 3, [`pos_cloud_config::receipt_template`]).

use std::sync::Arc;

use axum::response::Response;
use pos_cloud_api::response::service_unavailable;
use pos_cloud_catalog::translations::TranslationStore;
use pos_cloud_config::receipt_template::{RequiredSections, TEMPLATES_KEY, refusal};
use pos_country::CountryRegistry;
use pos_proto::ids::TenantId;

use crate::http::grid_keys::{FallbackKeys, GridKeys, StoreGrid};
use crate::http::setting_refusal::setting_refusal_response;

/// No grid: the package catch-up's state, which never writes a value. Failing closed, it answers
/// that the grid is unavailable.
struct NoGrid;

impl GridKeys for NoGrid {
    fn fallback_keys(&self, _tenant: TenantId) -> FallbackKeys<'_> {
        Box::pin(async { Err("the package catch-up reads no translation grid".to_owned()) })
    }
}

/// The grid and the countries' required sections a `receipt.templates` write is checked against.
#[derive(Clone)]
pub(super) struct ReceiptChecks {
    grid: Arc<dyn GridKeys>,
    required: Arc<Vec<RequiredSections>>,
}

impl ReceiptChecks {
    /// Checks against `translations` and every module of `countries`.
    pub(super) fn new<Tr>(countries: &CountryRegistry, translations: Tr) -> Self
    where
        Tr: TranslationStore + Send + Sync + 'static,
    {
        Self {
            grid: Arc::new(StoreGrid(translations)),
            required: Arc::new(required_sections(countries)),
        }
    }

    /// Checks that refuse every receipt template with a `503`, for a state that writes none.
    pub(super) fn without_grid(countries: &CountryRegistry) -> Self {
        Self {
            grid: Arc::new(NoGrid),
            required: Arc::new(required_sections(countries)),
        }
    }

    /// The `400` or `503` a write of `value` to `setting_key` answers, or `Ok` to write it. Only
    /// `receipt.templates` is checked here, after the register has taken its shape.
    pub(super) async fn check(
        &self,
        tenant: TenantId,
        setting_key: &str,
        value: &serde_json::Value,
    ) -> Result<(), Response> {
        if setting_key != TEMPLATES_KEY {
            return Ok(());
        }
        let keys = self.grid.fallback_keys(tenant).await.map_err(|error| {
            tracing::warn!(%error, "a receipt template write could not read the translation grid");
            service_unavailable("translation store")
        })?;
        match refusal(value, |key| keys.contains(key), &self.required) {
            Some(refused) => Err(setting_refusal_response(&refused)),
            None => Ok(()),
        }
    }
}

/// Each compiled-in country and the optional sections its law requires.
fn required_sections(countries: &CountryRegistry) -> Vec<RequiredSections> {
    countries
        .modules()
        .map(|module| (module.country_code(), module.receipt_required_sections()))
        .filter(|(_, sections)| !sections.is_empty())
        .collect()
}
