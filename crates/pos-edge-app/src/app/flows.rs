// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge's half of a package's flows
//! ([ADR-0192](../../../../docs/adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)
//! decision 4): before a command for an action a guard protects, the edge evaluates the guard from
//! its own facts and refuses with `FLOW_PREREQUISITE_UNMET`, naming the step. The conditions are
//! `pos-rules`'s; the graph is `pos_core::flow`'s.
//!
//! # What the edge can see
//!
//! A guard reads only the views the edge holds: `order`, `line`, `bill`, `channel` and `now`. The
//! `order` view is the order's `guest_count` (absent when none was recorded), its `line_count` and
//! its `channel`; `channel` is the order's sales channel token. `device` and `role` are the till's:
//! a guard reading them is refused when the package is applied.

use std::collections::BTreeMap;

use pos_core::flow::{FlowConditions, unmet_guard};
use pos_core::package::{VariantContext, resolve};
use pos_ports::event_store::EventStore;
use pos_proto::enums::SalesChannel;
use pos_proto::flow::Flow;
use pos_proto::ids::OrderId;
use pos_proto::package::Variant;
use pos_proto::wire_enum::{Open, WireEnum};
use pos_rules::{Fact, Facts, Rules, Variable};

use super::{AppError, Edge};

/// The views an edge guard may read.
const EDGE_VIEWS: [Variable; 5] = [
    Variable::Order,
    Variable::Line,
    Variable::Bill,
    Variable::Channel,
    Variable::Now,
];

/// Conditions compiled by `pos-rules`, answered over `facts`: none when the edge checks a package
/// on apply, an order's when it re-checks a command.
#[derive(Debug)]
pub struct EdgeConditions {
    rules: Option<Rules>,
    facts: Facts,
}

impl EdgeConditions {
    /// For checking a package on apply: conditions are compiled, never answered.
    #[must_use]
    pub fn for_apply() -> Self {
        Self::with(Facts::new())
    }

    fn with(facts: Facts) -> Self {
        Self {
            rules: Rules::new().ok(),
            facts,
        }
    }
}

impl FlowConditions for EdgeConditions {
    fn check(&self, condition: &str, edge_facts: bool) -> Result<(), String> {
        let rules = self
            .rules
            .as_ref()
            .ok_or("the rule evaluator could not be built")?;
        let compiled = rules
            .compile(condition)
            .map_err(|refused| refused.to_string())?;
        let foreign = Variable::ALL
            .into_iter()
            .find(|variable| !EDGE_VIEWS.contains(variable) && compiled.reads(*variable));
        match foreign {
            Some(variable) if edge_facts => Err(format!(
                "`{}` is the till's to read, so an edge guard cannot",
                variable.as_str()
            )),
            Some(_) | None => Ok(()),
        }
    }

    fn holds(&self, condition: &str) -> Result<bool, String> {
        let rules = self
            .rules
            .as_ref()
            .ok_or("the rule evaluator could not be built")?;
        rules
            .compile(condition)
            .and_then(|compiled| compiled.evaluate(&self.facts))
            .map_err(|refused| refused.to_string())
    }
}

/// The order view a guard reads, from what the edge holds about the order.
fn order_facts(
    channel: Option<SalesChannel>,
    guest_count: Option<u16>,
    line_count: usize,
) -> Facts {
    let token = channel.map_or(SalesChannel::UNSPECIFIED.as_wire(), WireEnum::as_wire);
    let mut order = BTreeMap::from([
        ("channel".to_owned(), Fact::Text(token.to_owned())),
        (
            "line_count".to_owned(),
            Fact::Int(i64::try_from(line_count).unwrap_or(i64::MAX)),
        ),
    ]);
    if let Some(count) = guest_count {
        order.insert("guest_count".to_owned(), Fact::Int(i64::from(count)));
    }
    Facts::new()
        .with(Variable::Order, Fact::Map(order))
        .with(Variable::Channel, Fact::Text(token.to_owned()))
}

impl<S: EventStore> Edge<S> {
    /// Re-checks a command for `action_id` on `order_id` against the package's flows: the variant of
    /// each flow for the order's channel, and its guards answered from the order's facts. Passes
    /// when the store runs no package, or the package has no guard for the act.
    ///
    /// # Errors
    ///
    /// [`AppError::FlowPrerequisiteUnmet`] naming the flow and the step not done.
    pub(in crate::app) fn check_flow_guard(
        &self,
        action_id: &str,
        order_id: OrderId,
    ) -> Result<(), AppError> {
        let session = self.session();
        let Some(package) = session.package.as_ref() else {
            return Ok(());
        };
        if package.flows.is_empty() {
            return Ok(());
        }
        let (channel, guest_count, line_count) = {
            let projection = self.lock_projection();
            (
                projection
                    .order_origin(order_id)
                    .and_then(|origin| origin.channel),
                projection.guest_count(order_id),
                projection.lines_for_order(order_id).len(),
            )
        };
        let context = VariantContext {
            sales_channel: channel.map_or_else(Open::default, Open::from_known),
            ..VariantContext::default()
        };
        // Each flow's variant for this order, chosen by the order's channel; the other dimensions
        // are the till's (ADR-0189 decision 4), so the edge falls back to the variant stating none.
        let mut ids: Vec<&str> = package
            .flows
            .iter()
            .map(|variant| variant.body.flow_id.as_str())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let resolved: Vec<Flow> = ids
            .into_iter()
            .filter_map(|id| {
                let variants: Vec<Variant<Flow>> = package
                    .flows
                    .iter()
                    .filter(|variant| variant.body.flow_id == id)
                    .cloned()
                    .collect();
                resolve(&variants, &context).cloned()
            })
            .collect();
        let flows: Vec<&Flow> = resolved.iter().collect();
        let conditions = EdgeConditions::with(order_facts(channel, guest_count, line_count));
        match unmet_guard(&flows, action_id, &conditions) {
            Some(unmet) => Err(AppError::FlowPrerequisiteUnmet {
                flow_id: unmet.flow_id,
                step_id: unmet.step_id,
            }),
            None => Ok(()),
        }
    }
}
