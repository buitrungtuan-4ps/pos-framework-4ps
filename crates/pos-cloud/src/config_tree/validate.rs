// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Validating an effective config document before it is published.
//!
//! The cloud is the one place that checks a configuration is coherent, so a store only ever receives
//! a version that already validated ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md),
//! `docs/pos-spec.md` §10). Two validators: a structural one, and the real one that also runs
//! `pos-core`'s inter-flag capability rules — the *same* rules the domain enforces, so the cloud and
//! the edge cannot disagree about which flag combinations are legal.

use serde_json::Value;

use pos_core::capability::{CapabilityContext, conflicts};
use pos_core::lease::LeaseConfig;
use pos_core::ota::{DeviceOtaConfig, FleetUpdateConfig};
use pos_proto::display::LayoutBook;
use pos_proto::menu::MenuBook;

/// Checks an effective config document, returning every violation (empty on success).
///
/// Returning *all* violations, not the first, so an admin fixing a rejected version sees the whole
/// list at once rather than one per attempt.
pub trait ConfigValidator {
    /// Validates `document`.
    ///
    /// # Errors
    ///
    /// The list of human-readable violations if the document is not a publishable configuration.
    fn validate(&self, document: &Value) -> Result<(), Vec<String>>;
}

/// The minimum: an effective configuration must be a JSON object.
#[derive(Debug, Clone, Copy, Default)]
pub struct StructuralValidator;

impl ConfigValidator for StructuralValidator {
    fn validate(&self, document: &Value) -> Result<(), Vec<String>> {
        if document.is_object() {
            Ok(())
        } else {
            Err(vec![
                "the configuration document must be a JSON object".to_owned(),
            ])
        }
    }
}

/// Structural checks, the §10 inter-flag capability rules from `pos-core`, and the OTA rollout rules.
///
/// This is the cloud-side validation the roadmap requires: a version that turns on an incompatible
/// pair of capabilities (pay-first with table service, seats without tables, …), or that carries an
/// incoherent OTA rollout (a bad target version, ring, ramp percent, or signing key id), is rejected
/// here, before any store can hold it. Every check runs through `pos-core`, so the cloud rejects
/// exactly what the edge would refuse.
#[derive(Debug, Clone, Copy, Default)]
pub struct CapabilityValidator;

impl ConfigValidator for CapabilityValidator {
    fn validate(&self, document: &Value) -> Result<(), Vec<String>> {
        StructuralValidator.validate(document)?;
        let mut violations: Vec<String> = conflicts(capability_context(document))
            .iter()
            .map(|rule| rule.description.to_owned())
            .collect();
        violations.extend(ota_violations(document));
        violations.extend(lease_violations(document));
        violations.extend(delivery_node_violations(document));
        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }
}

/// Validates the OTA rollout keys — `fleet_update` and `device_ota` — when the document sets them,
/// through `pos-core`'s shared rules ([ADR-0052](../../../docs/adr/0052-ota-rollout-config.md)), so
/// the cloud rejects exactly what the edge would refuse. An absent key means no rollout is configured
/// at that level, which is not a violation.
fn ota_violations(document: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    if let Some(value) = document.get("fleet_update") {
        match serde_json::from_value::<FleetUpdateConfig>(value.clone()) {
            Ok(config) => {
                if let Err(errors) = config.validate() {
                    violations.extend(errors);
                }
            }
            Err(error) => violations.push(format!("fleet_update is malformed: {error}")),
        }
    }
    if let Some(value) = document.get("device_ota") {
        match serde_json::from_value::<DeviceOtaConfig>(value.clone()) {
            Ok(config) => {
                if let Err(errors) = config.validate() {
                    violations.extend(errors);
                }
            }
            Err(error) => violations.push(format!("device_ota is malformed: {error}")),
        }
    }
    violations
}

/// Validates the `lease` node when the document sets it
/// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)), so a node the store
/// could not parse is rejected here rather than published and silently dropped.
///
/// There is nothing to check beyond the shape: every `u64` is a generation, so this arm has no
/// `validate()` to call the way the OTA arms do. Inventing a fallible rule to match their shape
/// would be a lie about what can go wrong with a counter.
///
/// The node itself is never *authored* — the bump route derives it from the store's `store_lease`
/// row, and no admin route accepts one in a body. This arm guards the generic config-publish route
/// (`PUT /admin/stores/{id}/config/{level}`), which accepts an arbitrary document and would
/// otherwise let somebody hand-write a `lease` node the edge cannot read.
fn lease_violations(document: &Value) -> Vec<String> {
    let Some(value) = document.get("lease") else {
        return Vec::new();
    };
    match serde_json::from_value::<LeaseConfig>(value.clone()) {
        Ok(_) => Vec::new(),
        Err(error) => vec![format!("lease is malformed: {error}")],
    }
}

/// Validates the compiled delivery nodes — `menu` and `layout` — the way the *edge* reads them, so a
/// node the store could not parse is rejected here rather than published and silently dropped.
///
/// This closes a real ops hazard: the catalog publish path compiles a typed book, but the generic
/// config-publish route (`PUT /admin/stores/{id}/config/{level}`) accepts an arbitrary document, and
/// nothing else checks these nodes. If a `menu` node does not deserialize back to a [`MenuBook`], the
/// edge's `session_from_config` leaves the price book unchanged and no error surfaces — a "successful"
/// publish that never reaches the counter. Parsing is done via `to_string` → `from_str`, not
/// `from_value`, because some wire types (e.g. `CurrencyCode`) deserialize from a *borrowed* `&str`
/// that `from_value` cannot supply — this is byte-for-byte the path the edge uses, so validation
/// accepts exactly what the store will. An absent node is not a violation.
fn delivery_node_violations(document: &Value) -> Vec<String> {
    let mut violations = Vec::new();
    if let Some(value) = document.get("menu")
        && !parses_as::<MenuBook>(value)
    {
        violations.push(
            "the `menu` node is not a parseable MenuBook — the store would silently ignore it"
                .to_owned(),
        );
    }
    if let Some(value) = document.get("layout")
        && !parses_as::<LayoutBook>(value)
    {
        violations.push(
            "the `layout` node is not a parseable LayoutBook — the store would silently ignore it"
                .to_owned(),
        );
    }
    violations
}

/// Whether `value` deserializes to `T` through the edge's `to_string` → `from_str` path.
fn parses_as<T: serde::de::DeserializeOwned>(value: &Value) -> bool {
    serde_json::to_string(value)
        .ok()
        .and_then(|text| serde_json::from_str::<T>(&text).ok())
        .is_some()
}

/// Reads the capability flags out of an effective document, defaulting each to its declared default
/// when the document does not set it — so validation sees the same context a store would. Uses the
/// shared [`CapabilityContext::from_flags`] reader (§10) so the cloud and the edge read a published
/// profile the same way; the closure is this crate's JSON lookup.
fn capability_context(document: &Value) -> CapabilityContext {
    CapabilityContext::from_flags(|key| document.get(key).and_then(Value::as_bool))
}

#[cfg(test)]
mod tests {
    use super::{CapabilityValidator, ConfigValidator, StructuralValidator};

    use serde_json::json;

    #[test]
    fn structural_validation_rejects_a_non_object() {
        assert!(StructuralValidator.validate(&json!([1, 2, 3])).is_err());
        assert!(StructuralValidator.validate(&json!("nope")).is_err());
        assert!(StructuralValidator.validate(&json!({"ok": true})).is_ok());
    }

    #[test]
    fn the_default_configuration_is_valid() {
        // An empty document means every flag takes its default, and the shipped defaults satisfy the
        // §10 rules — so a store with no overrides is always publishable.
        assert_eq!(CapabilityValidator.validate(&json!({})), Ok(()));
    }

    #[test]
    fn an_incompatible_flag_pair_is_rejected_with_a_reason() {
        // pay-first and table service cannot both be on (§10).
        let outcome = CapabilityValidator.validate(&json!({
            "pay_first_enabled": true,
            "tables_enabled": true
        }));
        let violations = outcome.expect_err("the pair must be rejected");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("pay_first_enabled") && violations[0].contains("tables_enabled"),
            "the reason names the conflicting flags: {violations:?}"
        );
    }

    #[test]
    fn a_coherent_non_default_configuration_is_accepted() {
        // Counter service: pay-first on, table service off — a legal combination.
        assert_eq!(
            CapabilityValidator.validate(&json!({
                "pay_first_enabled": true,
                "tables_enabled": false,
                "seats_enabled": false
            })),
            Ok(())
        );
    }

    #[test]
    fn a_coherent_fleet_update_key_is_accepted() {
        assert_eq!(
            CapabilityValidator.validate(&json!({
                "fleet_update": {
                    "target_version": "1.2.3",
                    "min_ring": "pilot",
                    "rollout_percent": 25,
                    "signing_key_id": "a1a1a1a1a1a1a1a1"
                }
            })),
            Ok(())
        );
    }

    #[test]
    fn an_incoherent_fleet_update_key_is_rejected_with_reasons() {
        // A ring that names no ring and a ramp past 100 — both must be reported.
        let violations = CapabilityValidator
            .validate(&json!({
                "fleet_update": {
                    "target_version": "1.2.3",
                    "min_ring": "everywhere",
                    "rollout_percent": 250,
                    "signing_key_id": "a1a1a1a1a1a1a1a1"
                }
            }))
            .expect_err("a bad ring and ramp must be rejected");
        assert!(
            violations.iter().any(|v| v.contains("min_ring")),
            "{violations:?}"
        );
        assert!(
            violations.iter().any(|v| v.contains("rollout_percent")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_malformed_fleet_update_key_is_rejected() {
        // rollout_percent as a string is the wrong shape — the key fails to deserialize at all.
        let violations = CapabilityValidator
            .validate(&json!({
                "fleet_update": {
                    "target_version": "1.0.0",
                    "min_ring": "lab",
                    "rollout_percent": "lots",
                    "signing_key_id": "a1a1a1a1a1a1a1a1"
                }
            }))
            .expect_err("a malformed key must be rejected");
        assert!(
            violations
                .iter()
                .any(|v| v.contains("fleet_update is malformed")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_lease_node_that_is_not_a_generation_is_rejected() {
        // The bump route derives this node, so it is always well-formed on that path. This guards
        // the generic config-publish route, where somebody could hand-write one the edge would then
        // silently drop — a "successful" publish that never takes effect.
        let violations = CapabilityValidator
            .validate(&json!({ "lease": { "generation": "four" } }))
            .expect_err("a generation that is not a number is a violation");
        assert!(violations.iter().any(|v| v.contains("lease is malformed")));

        CapabilityValidator
            .validate(&json!({ "lease": { "generation": 4 } }))
            .expect("a well-formed generation publishes");
        CapabilityValidator
            .validate(&json!({ "menu_version": 1 }))
            .expect("an absent lease node is not a violation");
    }

    #[test]
    fn a_bad_device_ota_assignment_is_rejected() {
        let violations = CapabilityValidator
            .validate(&json!({ "device_ota": { "ring": "nope", "canary_bucket": 200 } }))
            .expect_err("a bad ring and out-of-range bucket must be rejected");
        assert!(
            violations.iter().any(|v| v.contains("device_ota.ring")),
            "{violations:?}"
        );
        assert!(
            violations
                .iter()
                .any(|v| v.contains("device_ota.canary_bucket")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_document_with_no_ota_keys_is_unaffected() {
        // The OTA checks fire only on fleet_update / device_ota; an ordinary config still validates.
        assert_eq!(
            CapabilityValidator.validate(&json!({ "tips_enabled": true })),
            Ok(())
        );
    }

    #[test]
    fn a_compiled_menu_node_is_accepted() {
        use pos_proto::enums::SalesChannel;
        use pos_proto::ids::{MenuItemId, TaxClassId};
        use pos_proto::menu::{MenuBook, MenuCatalog, MenuEntry};
        use pos_proto::money::{CurrencyCode, Money};
        use pos_proto::text::DisplayName;
        use pos_proto::ulid::Ulid;

        // A real compiled book — exactly what the catalog publish emits — must validate, so the
        // hardening never rejects a legitimate publish.
        let catalog = MenuCatalog::new().with(MenuEntry::new(
            MenuItemId::new(Ulid::from_u128(1)),
            DisplayName::new("Margherita"),
            Money::new(CurrencyCode::VND, 99_000),
            TaxClassId::new(Ulid::from_u128(2)),
        ));
        let book = MenuBook::new().with(SalesChannel::DineIn, catalog);
        let document = json!({ "menu": serde_json::to_value(&book).expect("serialize") });
        assert_eq!(CapabilityValidator.validate(&document), Ok(()));
    }

    #[test]
    fn a_malformed_menu_node_is_rejected_before_a_store_can_silently_drop_it() {
        // The generic config-publish route accepts any document; a `menu` node the edge could not
        // parse must be caught here, not published and silently ignored by the store.
        let violations = CapabilityValidator
            .validate(&json!({ "menu": "not a book" }))
            .expect_err("a malformed menu node must be rejected");
        assert!(
            violations.iter().any(|v| v.contains("`menu` node")),
            "the reason names the menu node: {violations:?}"
        );
    }

    #[test]
    fn a_malformed_layout_node_is_rejected() {
        let violations = CapabilityValidator
            .validate(&json!({ "layout": [1, 2, 3] }))
            .expect_err("a malformed layout node must be rejected");
        assert!(
            violations.iter().any(|v| v.contains("`layout` node")),
            "the reason names the layout node: {violations:?}"
        );
    }

    #[test]
    fn a_document_with_no_delivery_nodes_is_unaffected() {
        // The menu/layout checks fire only when those keys are present.
        assert_eq!(
            CapabilityValidator.validate(&json!({ "tables_enabled": true })),
            Ok(())
        );
    }
}
