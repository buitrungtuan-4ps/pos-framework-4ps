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

use pos_core::capability::{Capability, CapabilityContext, conflicts};
use pos_core::ota::{DeviceOtaConfig, FleetUpdateConfig};

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

/// Reads the capability flags out of an effective document, defaulting each to its declared default
/// when the document does not set it — so validation sees the same context a store would.
fn capability_context(document: &Value) -> CapabilityContext {
    Capability::ALL
        .iter()
        .copied()
        .filter(|capability| {
            let meta = capability.meta();
            document
                .get(meta.key)
                .and_then(Value::as_bool)
                .unwrap_or(meta.default_on)
        })
        .collect()
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
}
