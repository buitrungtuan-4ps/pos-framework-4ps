// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The vendors this cloud binary was built with
//! ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)).
//!
//! One line per adapter. A fork that adds a vendor adds its crate to `Cargo.toml` and its line here,
//! and the console lists it the next time it loads: the form is drawn from the adapter's own
//! settings schema, so nothing in the dashboard changes.
//!
//! The console reads the catalogue through `GET /admin/integrations/providers`. What a tenant then
//! configures — a *connection*, with its settings and sealed secrets — is not stored yet; this is
//! only "which vendors can this platform talk to, and what does each need".

use pos_providers::{Family, FieldKind, ProviderDescriptor, Registry, RegistryError, SettingField};
use serde::Serialize;

/// The registry of providers compiled into this binary.
///
/// # Errors
///
/// A [`RegistryError`] if two adapters claim one id or a descriptor is malformed — a programming
/// error, so `main` refuses to start rather than serve a catalogue whose connections could resolve
/// to the wrong vendor. The test below makes that a build failure first.
pub fn registry() -> Result<Registry, RegistryError> {
    Registry::new([
        &shipping_ahamove::PROVIDER,
        &shipping_grabexpress::PROVIDER,
        &erp_sap::PROVIDER,
    ])
}

/// The catalogue as the console reads it.
#[derive(Debug, Serialize)]
pub struct CatalogueView {
    /// Every provider, grouped by family in [`Family::ALL`]'s order and in listed order within one.
    pub providers: Vec<ProviderView>,
}

/// One provider, with the schema the console draws its form from.
#[derive(Debug, Serialize)]
pub struct ProviderView {
    provider_id: &'static str,
    family: &'static str,
    runs_on: &'static str,
    name_key: &'static str,
    /// ISO 3166-1 alpha-2. Empty means every country.
    countries: Vec<String>,
    capabilities: &'static [&'static str],
    sandbox: bool,
    fields: Vec<FieldView>,
}

/// One field of a provider's settings schema.
#[derive(Debug, Serialize)]
pub struct FieldView {
    key: &'static str,
    label_key: &'static str,
    kind: &'static str,
    required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<&'static [&'static str]>,
}

/// Renders a registry for the console.
#[must_use]
pub fn catalogue(registry: &Registry) -> CatalogueView {
    let providers = Family::ALL
        .into_iter()
        .flat_map(|family| registry.in_family(family))
        .map(provider_view)
        .collect();
    CatalogueView { providers }
}

fn provider_view(provider: &'static ProviderDescriptor) -> ProviderView {
    ProviderView {
        provider_id: provider.provider_id,
        family: provider.family.wire(),
        runs_on: provider.family.runs_on().wire(),
        name_key: provider.name_key,
        countries: provider
            .countries
            .iter()
            .map(|country| country.as_str().to_owned())
            .collect(),
        capabilities: provider.capabilities,
        sandbox: provider.sandbox,
        fields: provider.fields.iter().map(field_view).collect(),
    }
}

fn field_view(field: &'static SettingField) -> FieldView {
    let mut view = FieldView {
        key: field.key,
        label_key: field.label_key,
        kind: field.kind.wire(),
        required: field.required,
        max_length: None,
        min: None,
        max: None,
        options: None,
    };
    match &field.kind {
        FieldKind::Text { max_len } | FieldKind::Secret { max_len } => {
            view.max_length = Some(*max_len);
        }
        FieldKind::Number { min, max } => {
            view.min = Some(*min);
            view.max = Some(*max);
        }
        FieldKind::Choice { options } => view.options = Some(options),
        FieldKind::Url | FieldKind::Flag => {}
    }
    view
}

#[cfg(test)]
mod tests {
    use super::{catalogue, registry};

    #[test]
    fn the_compiled_in_providers_form_a_valid_catalogue() {
        let registry = registry().expect("every adapter's descriptor is well formed");
        let view = serde_json::to_value(catalogue(&registry)).expect("serializes");
        let ids: Vec<&str> = view["providers"]
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|provider| provider["provider_id"].as_str())
            .collect();
        // Family order: couriers before the ERP.
        assert_eq!(
            ids,
            vec!["courier.ahamove", "courier.grabexpress", "erp.sap"]
        );
        let ahamove = &view["providers"][0];
        assert_eq!(ahamove["family"], "INTEGRATION_FAMILY_COURIER");
        assert_eq!(ahamove["runs_on"], "PROVIDER_RUNTIME_CLOUD");
        assert_eq!(ahamove["countries"], serde_json::json!(["VN"]));
        assert_eq!(ahamove["fields"][0]["kind"], "SETTING_KIND_URL");
        assert_eq!(ahamove["fields"][1]["min"], 1);
    }
}
