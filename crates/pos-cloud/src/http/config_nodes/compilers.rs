// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The settings-node compilers the routes here and the batch fan-out share (ADR-0122 §3).

use axum::response::Response;
use pos_cloud_api::response::{api_error, api_error_with_details};
use pos_proto::ErrorStatus;
use pos_proto::channels::{
    PublishedChannels, PublishedTender, PublishedVendorPolicies, PublishedVendorPolicy,
};
use pos_proto::enums::{PaymentMethod, SalesChannel};
use pos_proto::origins::PublishedOrigins;
use serde::Deserialize;

use super::parse_known_tokens;
use super::qr::QrBusinessHoursRequest;

// --- The settings-node compilers, shared with the batch fan-out (ADR-0122 §3) ------------------
//
// Each of these turns a request body into the one node value a publish writes, and each is called
// from exactly two places: the single-store route above, and that node's `BatchNode` impl. Sharing
// them is what makes ADR-0122 §3 true — a batch runs the *same* validation as the route it fans
// out, so a value the console could not have published to one store cannot reach fifty either.

/// Compiles the `channels` node: the sales channels a store answers.
#[expect(
    clippy::result_large_err,
    reason = "the refusal *is* an axum Response by design — the route's own, factored out so the \
              batch fan-out refuses identically rather than by a second copy of the rule"
)]
pub(in crate::http) fn channels_node(enabled: &[String]) -> Result<serde_json::Value, Response> {
    let enabled = parse_known_tokens::<SalesChannel>("enabled", enabled)?;
    serde_json::to_value(PublishedChannels::new(enabled))
        .map_err(|_| channels_serialize_unavailable())
}

/// Compiles the `tender` node: the payment methods a store accepts.
#[expect(
    clippy::result_large_err,
    reason = "the refusal *is* an axum Response by design — the route's own, factored out so the \
              batch fan-out refuses identically rather than by a second copy of the rule"
)]
pub(in crate::http) fn tender_node(accepted: &[String]) -> Result<serde_json::Value, Response> {
    let accepted = parse_known_tokens::<PaymentMethod>("accepted", accepted)?;
    serde_json::to_value(PublishedTender::new(accepted))
        .map_err(|_| channels_serialize_unavailable())
}

/// Compiles the `origins` node: the extra origins a store's edge answers (ADR-0111).
///
/// The refusal names the entry and the rule it broke: the operator is fixing a value they typed,
/// and a published origin is a configuration value, never a secret.
#[expect(
    clippy::result_large_err,
    reason = "the refusal *is* an axum Response by design — the route's own, factored out so the \
              batch fan-out refuses identically rather than by a second copy of the rule"
)]
pub(in crate::http) fn origins_node(allowed: &[String]) -> Result<serde_json::Value, Response> {
    let node = PublishedOrigins::new(allowed)
        .map_err(|refused| api_error(ErrorStatus::InvalidArgument, refused.to_string()))?;
    serde_json::to_value(node).map_err(|_| channels_serialize_unavailable())
}

/// Compiles the `vendors` node: a store's per-marketplace policies.
#[expect(
    clippy::result_large_err,
    reason = "the refusal *is* an axum Response by design — the route's own, factored out so the \
              batch fan-out refuses identically rather than by a second copy of the rule"
)]
pub(in crate::http) fn vendor_policies_node(
    policies: Vec<PublishedVendorPolicy>,
) -> Result<serde_json::Value, Response> {
    if policies
        .iter()
        .any(|policy| policy.availability.is_unspecified() || policy.availability.is_unrecognised())
    {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a vendor policy names an unknown availability (open/busy/closed)",
            &[("policies", "INVALID_ENUM_VALUE")],
        ));
    }
    serde_json::to_value(PublishedVendorPolicies::new(policies))
        .map_err(|_| channels_serialize_unavailable())
}

/// The QR guardrails themselves, with the `(tenant, store)` a single-store publish also carries
/// stripped off — which is precisely what makes this node batchable under ADR-0122 §4.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct QrGuardrailFields {
    /// QR ordering's switch, for a caller that still sends it here. Optional: the switch is
    /// `qr_ordering_enabled` (ADR-0160 decision 5), and `qr.enabled` follows it. A value sent here is
    /// written as the switch, so an older console's checkbox and a cohort copy of a store's `qr`
    /// node keep doing what they did; absent, the store's switch is kept.
    #[serde(default)]
    pub(in crate::http) enabled: Option<bool>,
    pub(in crate::http) staff_confirmation_required: bool,
    pub(in crate::http) per_table_limit: u32,
    pub(in crate::http) rate_window_secs: u64,
    pub(in crate::http) business_hours: Option<QrBusinessHoursRequest>,
}

/// Compiles the `qr` node: the guardrails around QR ordering (ADR-0116), and `enabled` when the
/// caller sent one. The publish then sets `enabled` to the store's switch
/// ([`crate::qr_ordering::align`]), so the node always carries it.
#[expect(
    clippy::result_large_err,
    reason = "the refusal *is* an axum Response by design — the route's own, factored out so the \
              batch fan-out refuses identically rather than by a second copy of the rule"
)]
pub(in crate::http) fn qr_guardrail_node(
    fields: &QrGuardrailFields,
) -> Result<serde_json::Value, Response> {
    let mut node = serde_json::json!({
        "staff_confirmation_required": fields.staff_confirmation_required,
        "per_table_limit": fields.per_table_limit,
        "rate_window_secs": fields.rate_window_secs,
    });
    if let (Some(enabled), serde_json::Value::Object(map)) = (fields.enabled, &mut node) {
        map.insert("enabled".to_owned(), serde_json::Value::Bool(enabled));
    }
    if let Some(hours) = fields.business_hours.as_ref() {
        let mut out_of_range: Vec<(&str, &str)> = Vec::with_capacity(2);
        if hours.open_hour > 23 {
            out_of_range.push(("open_hour", "OUT_OF_RANGE"));
        }
        if hours.close_hour > 23 {
            out_of_range.push(("close_hour", "OUT_OF_RANGE"));
        }
        if !out_of_range.is_empty() {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "open_hour and close_hour must be in 0..=23",
                &out_of_range,
            ));
        }
        if let serde_json::Value::Object(map) = &mut node {
            map.insert(
                "business_hours".to_owned(),
                serde_json::json!({
                    "open_hour": hours.open_hour,
                    "close_hour": hours.close_hour,
                    "tz_offset_minutes": hours.tz_offset_minutes,
                }),
            );
        }
    }
    Ok(node)
}

/// The `503` for the impossible case that a channels/tender node fails to serialise.
fn channels_serialize_unavailable() -> Response {
    tracing::error!("could not serialise a channels/tender node");
    api_error(
        ErrorStatus::Unavailable,
        "the configuration service is unavailable",
    )
}
