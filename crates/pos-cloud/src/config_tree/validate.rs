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

/// Structural checks plus the §10 inter-flag capability rules from `pos-core`.
///
/// This is the cloud-side inter-flag validation the roadmap requires: a version that turns on an
/// incompatible pair of capabilities (pay-first with table service, seats without tables, …) is
/// rejected here, before any store can hold it.
#[derive(Debug, Clone, Copy, Default)]
pub struct CapabilityValidator;

impl ConfigValidator for CapabilityValidator {
    fn validate(&self, document: &Value) -> Result<(), Vec<String>> {
        StructuralValidator.validate(document)?;
        let violations: Vec<String> = conflicts(capability_context(document))
            .iter()
            .map(|rule| rule.description.to_owned())
            .collect();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }
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
}
