// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The alert domain model ([ADR-0073](../../../docs/adr/0073-alerting.md)): the kinds of operational
//! condition the engine watches, their severity, and the [`FiringAlert`] the pure evaluator produces.
//!
//! These types carry no I/O. The evaluator ([`super::eval`]) turns a read-model snapshot into a set of
//! `FiringAlert`s; the alert store (slice 2) persists them with an open→resolved lifecycle.

use pos_proto::ids::TenantId;

/// How serious an alert is. Ordered least-to-most so the console can sort most-severe first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    /// Informational — worth surfacing, not acting on immediately.
    Info,
    /// Something is wrong and needs attention, but the fleet is still serving.
    Warning,
    /// Serving or data integrity is at risk — act now.
    Critical,
}

impl AlertSeverity {
    /// The stable wire/storage token (matches the serde representation).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    /// Parses the token produced by [`AlertSeverity::as_str`]; unknown tokens fall back to `Warning`
    /// so a stored row with an unexpected value is never dropped.
    #[must_use]
    pub fn parse(token: &str) -> Self {
        match token {
            "info" => Self::Info,
            "critical" => Self::Critical,
            _ => Self::Warning,
        }
    }
}

/// The kind of operational condition an alert reports. Each kind has a stable id (the wire/storage
/// token and the dedup-key namespace), a default severity, and whether it is server-wide (no tenant)
/// or scoped to one tenant's store/endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// A store has not checked in within the offline threshold.
    StoreOffline,
    /// A store's relay queue is backed up — too many, or too old, unreported orders.
    RelayBacklog,
    /// A webhook endpoint auto-disabled after repeated delivery failures.
    WebhookDisabled,
    /// The rollup projector loop is stale or reporting failures.
    ProjectorUnhealthy,
    /// The store→cloud JetStream is near its configured capacity.
    JetstreamCapacity,
}

impl AlertKind {
    /// Every alert kind, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::StoreOffline,
        Self::RelayBacklog,
        Self::WebhookDisabled,
        Self::ProjectorUnhealthy,
        Self::JetstreamCapacity,
    ];

    /// The stable wire/storage token (matches the serde representation).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StoreOffline => "store_offline",
            Self::RelayBacklog => "relay_backlog",
            Self::WebhookDisabled => "webhook_disabled",
            Self::ProjectorUnhealthy => "projector_unhealthy",
            Self::JetstreamCapacity => "jetstream_capacity",
        }
    }

    /// Parses the token produced by [`AlertKind::as_str`], or `None` for an unknown token.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == token)
    }

    /// The severity a firing alert of this kind carries unless the evaluator overrides it.
    #[must_use]
    pub const fn default_severity(self) -> AlertSeverity {
        match self {
            Self::StoreOffline | Self::RelayBacklog | Self::WebhookDisabled => {
                AlertSeverity::Warning
            }
            Self::ProjectorUnhealthy | Self::JetstreamCapacity => AlertSeverity::Critical,
        }
    }

    /// Whether this kind is a server-wide condition (no owning tenant) rather than scoped to a
    /// tenant's store or endpoint.
    #[must_use]
    pub const fn is_server_wide(self) -> bool {
        matches!(self, Self::ProjectorUnhealthy | Self::JetstreamCapacity)
    }
}

/// One firing condition, as the pure evaluator reports it. The store turns this into (or refreshes)
/// an open alert row, keyed by `(tenant_id, kind, dedup_key)`.
///
/// `dedup_key` scopes the alert within its kind: a store id for a store-scoped condition, an endpoint
/// id for a webhook one, empty for a server-wide singleton. `detail` is a small JSON object of the
/// numbers behind the alert (counts, ages, a version) — never a payload or PII.
#[derive(Debug, Clone, PartialEq)]
pub struct FiringAlert {
    /// The condition kind.
    pub kind: AlertKind,
    /// The owning tenant, or `None` for a server-wide alert.
    pub tenant_id: Option<TenantId>,
    /// The dedup key within the kind (store id, endpoint id, or empty for a singleton).
    pub dedup_key: String,
    /// The alert's severity.
    pub severity: AlertSeverity,
    /// A one-line human summary (already composed; not localized — the console localizes by `kind`).
    pub summary: String,
    /// The numbers behind the alert, as a small JSON object.
    pub detail: serde_json::Value,
}

impl FiringAlert {
    /// A firing alert of `kind` with its default severity.
    #[must_use]
    pub fn new(
        kind: AlertKind,
        tenant_id: Option<TenantId>,
        dedup_key: impl Into<String>,
        summary: impl Into<String>,
        detail: serde_json::Value,
    ) -> Self {
        Self {
            kind,
            tenant_id,
            dedup_key: dedup_key.into(),
            severity: kind.default_severity(),
            summary: summary.into(),
            detail,
        }
    }

    /// The same alert at a different severity — the override
    /// [`AlertKind::default_severity`] documents.
    ///
    /// Used by the store-offline rule, where the condition is one kind but its urgency depends on
    /// where the store's edge runs
    /// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)): the same silence
    /// means "probably still selling, and we cannot see it" from a shop-floor box, and "not selling"
    /// from a hosted one.
    #[must_use]
    pub fn at_severity(mut self, severity: AlertSeverity) -> Self {
        self.severity = severity;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{AlertKind, AlertSeverity};

    #[test]
    fn kind_tokens_round_trip_and_are_distinct() {
        let mut seen = std::collections::BTreeSet::new();
        for kind in AlertKind::ALL {
            let token = kind.as_str();
            assert!(seen.insert(token), "duplicate kind token {token}");
            assert_eq!(AlertKind::parse(token), Some(*kind));
            // The token is lower snake_case.
            assert!(
                token.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{token} is not lower snake_case"
            );
        }
        assert_eq!(AlertKind::parse("nope"), None);
    }

    #[test]
    fn severity_tokens_round_trip_and_order_by_urgency() {
        for severity in [
            AlertSeverity::Info,
            AlertSeverity::Warning,
            AlertSeverity::Critical,
        ] {
            assert_eq!(AlertSeverity::parse(severity.as_str()), severity);
        }
        assert!(AlertSeverity::Critical > AlertSeverity::Warning);
        assert!(AlertSeverity::Warning > AlertSeverity::Info);
        // Unknown tokens fall back to Warning, never dropped.
        assert_eq!(AlertSeverity::parse("bogus"), AlertSeverity::Warning);
    }

    #[test]
    fn server_wide_kinds_are_exactly_the_infrastructure_ones() {
        assert!(AlertKind::ProjectorUnhealthy.is_server_wide());
        assert!(AlertKind::JetstreamCapacity.is_server_wide());
        assert!(!AlertKind::StoreOffline.is_server_wide());
        assert!(!AlertKind::RelayBacklog.is_server_wide());
        assert!(!AlertKind::WebhookDisabled.is_server_wide());
    }
}
