// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The settings nodes a batch publish carries: capabilities, channels, tender, origins, QR and
//! vendor policies.

use axum::response::Response;
use pos_proto::channels::PublishedVendorPolicy;
use pos_proto::ids::TenantId;
use serde::Deserialize;

use super::batch_node::{BatchNode, BatchNodePlan, BoxFuture, SameEverywhere, batch_arguments};
use crate::http::config_nodes::{
    QrGuardrailFields, channels_node, origins_node, qr_guardrail_node, tender_node,
    vendor_policies_node,
};
use crate::http::people::capability_flag_nodes;

/// The `capabilities` node: the flag booleans an operator ticked.
pub(super) struct CapabilitiesBatchNode;

/// A `capabilities` batch's arguments — the same `flags` map the single-store body carries.
#[derive(Debug, Clone, Deserialize)]
struct CapabilitiesBatchArguments {
    flags: std::collections::BTreeMap<String, bool>,
}

impl BatchNode for CapabilitiesBatchNode {
    fn key(&self) -> &'static str {
        "capabilities"
    }

    fn audit_action(&self) -> &'static str {
        "config.capabilities.publish"
    }

    fn plan<'a>(
        &'a self,
        _tenant_id: TenantId,
        arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let parsed: CapabilitiesBatchArguments = batch_arguments(self.key(), arguments)?;
            let nodes = capability_flag_nodes(&parsed.flags)?;
            Ok(SameEverywhere {
                summary: serde_json::json!({ "flags": parsed.flags.len() }),
                nodes,
            }
            .boxed())
        })
    }
}

/// The `channels` node: which sales channels each member answers.
pub(super) struct ChannelsBatchNode;

/// A `channels` batch's arguments.
#[derive(Debug, Clone, Deserialize)]
struct ChannelsBatchArguments {
    #[serde(default)]
    enabled: Vec<String>,
}

impl BatchNode for ChannelsBatchNode {
    fn key(&self) -> &'static str {
        "channels"
    }

    fn audit_action(&self) -> &'static str {
        "config.channels.publish"
    }

    fn plan<'a>(
        &'a self,
        _tenant_id: TenantId,
        arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let parsed: ChannelsBatchArguments = batch_arguments(self.key(), arguments)?;
            let value = channels_node(&parsed.enabled)?;
            Ok(SameEverywhere {
                summary: serde_json::json!({ "enabled": parsed.enabled }),
                nodes: vec![("channels".to_owned(), value)],
            }
            .boxed())
        })
    }
}

/// The `tender` node: which payment methods each member accepts.
pub(super) struct TenderBatchNode;

/// A `tender` batch's arguments.
#[derive(Debug, Clone, Deserialize)]
struct TenderBatchArguments {
    #[serde(default)]
    accepted: Vec<String>,
}

impl BatchNode for TenderBatchNode {
    fn key(&self) -> &'static str {
        "tender"
    }

    fn audit_action(&self) -> &'static str {
        "config.tender.publish"
    }

    fn plan<'a>(
        &'a self,
        _tenant_id: TenantId,
        arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let parsed: TenderBatchArguments = batch_arguments(self.key(), arguments)?;
            let value = tender_node(&parsed.accepted)?;
            Ok(SameEverywhere {
                summary: serde_json::json!({ "accepted": parsed.accepted }),
                nodes: vec![("tender".to_owned(), value)],
            }
            .boxed())
        })
    }
}

/// The `origins` node: the extra origins each member's edge answers (ADR-0111).
pub(super) struct OriginsBatchNode;

/// An `origins` batch's arguments.
#[derive(Debug, Clone, Deserialize)]
struct OriginsBatchArguments {
    #[serde(default)]
    allowed: Vec<String>,
}

impl BatchNode for OriginsBatchNode {
    fn key(&self) -> &'static str {
        "origins"
    }

    fn audit_action(&self) -> &'static str {
        "config.origins.publish"
    }

    fn plan<'a>(
        &'a self,
        _tenant_id: TenantId,
        arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let parsed: OriginsBatchArguments = batch_arguments(self.key(), arguments)?;
            let value = origins_node(&parsed.allowed)?;
            Ok(SameEverywhere {
                summary: serde_json::json!({ "allowed": parsed.allowed.len() }),
                nodes: vec![("origins".to_owned(), value)],
            }
            .boxed())
        })
    }
}

/// The `qr` node: the guardrails at each member (ADR-0116), and QR ordering's switch when the
/// arguments name `enabled`.
pub(super) struct QrBatchNode;

impl BatchNode for QrBatchNode {
    fn key(&self) -> &'static str {
        "qr"
    }

    fn audit_action(&self) -> &'static str {
        "config.qr.publish"
    }

    fn plan<'a>(
        &'a self,
        _tenant_id: TenantId,
        arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let parsed: QrGuardrailFields = batch_arguments(self.key(), arguments)?;
            let enabled = parsed.enabled;
            let value = qr_guardrail_node(&parsed)?;
            Ok(SameEverywhere {
                summary: serde_json::json!({ "enabled": enabled }),
                nodes: vec![("qr".to_owned(), value)],
            }
            .boxed())
        })
    }
}

/// The `vendors` node: each member's per-marketplace policies.
pub(super) struct VendorPoliciesBatchNode;

/// A `vendor_policies` batch's arguments.
#[derive(Debug, Clone, Deserialize)]
struct VendorPoliciesBatchArguments {
    #[serde(default)]
    policies: Vec<PublishedVendorPolicy>,
}

impl BatchNode for VendorPoliciesBatchNode {
    fn key(&self) -> &'static str {
        "vendor_policies"
    }

    fn audit_action(&self) -> &'static str {
        "config.vendors.publish"
    }

    fn plan<'a>(
        &'a self,
        _tenant_id: TenantId,
        arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>> {
        Box::pin(async move {
            let parsed: VendorPoliciesBatchArguments = batch_arguments(self.key(), arguments)?;
            let count = parsed.policies.len();
            let value = vendor_policies_node(parsed.policies)?;
            Ok(SameEverywhere {
                summary: serde_json::json!({ "policies": count }),
                nodes: vec![("vendors".to_owned(), value)],
            }
            .boxed())
        })
    }
}
