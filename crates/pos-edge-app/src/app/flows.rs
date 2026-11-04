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
//!
//! # Which version's rules
//!
//! An order keeps the rules of the package version it opened under (decision 5): its
//! `sales.order.opened` names that version, and a publish while it is open does not change what
//! the edge checks it against. The edge holds at most [`RETAINED_MAX`] versions, each with its
//! conditions compiled once, preferring to drop one no open order references. An order whose
//! version is no longer held, after an eviction or a restart, is checked under the version the store
//! runs now, and the edge logs that it did so. An order opened under the default package has no
//! guards.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use pos_core::flow::{FlowConditions, unmet_guard};
use pos_core::package::{VariantContext, resolve};
use pos_ports::event_store::EventStore;
use pos_proto::enums::SalesChannel;
use pos_proto::flow::Flow;
use pos_proto::ids::{OrderId, PackageVersionId};
use pos_proto::package::{PackageContent, Variant};
use pos_proto::wire_enum::{Open, WireEnum};
use pos_rules::{Condition, Fact, Facts, Rules, Variable};

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
    /// The version's conditions compiled once, consulted before compiling again.
    compiled: Option<Arc<Compiled>>,
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
            compiled: None,
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
        let cached = self
            .compiled
            .as_ref()
            .and_then(|compiled| compiled.conditions.get(condition));
        if let Some(cached) = cached {
            return cached
                .evaluate(&self.facts)
                .map_err(|refused| refused.to_string());
        }
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

/// The most package versions the edge holds for open orders (ADR-0192 decision 5).
pub const RETAINED_MAX: usize = 4;

/// A package version's conditions, each compiled once: every `applies_when` and `done_when` of
/// every variant of every flow.
#[derive(Debug, Default)]
pub struct Compiled {
    conditions: HashMap<String, Condition>,
}

impl Compiled {
    fn of(content: &PackageContent) -> Self {
        let Ok(rules) = Rules::new() else {
            return Self::default();
        };
        let conditions = content
            .flows
            .iter()
            .flat_map(|variant| &variant.body.steps)
            .flat_map(|step| [&step.applies_when, &step.done_when])
            .flatten()
            .filter_map(|source| {
                let compiled = rules.compile(source).ok()?;
                Some((source.clone(), compiled))
            })
            .collect();
        Self { conditions }
    }
}

/// A held version: its content and its compiled conditions.
type Held = (Arc<PackageContent>, Arc<Compiled>);

/// The package versions open orders still reference, at most [`RETAINED_MAX`], oldest use first.
#[derive(Debug, Default)]
pub struct Retained {
    versions: Vec<Held>,
}

impl Retained {
    fn get(&self, version: PackageVersionId) -> Option<Held> {
        self.versions
            .iter()
            .find(|(content, _)| content.package_version_id == version)
            .map(|(content, compiled)| (Arc::clone(content), Arc::clone(compiled)))
    }

    /// Holds `content`, compiling it if it is new. At the limit it first drops the oldest version
    /// no open order references, or the oldest of all when every one is referenced.
    fn hold(&mut self, content: &Arc<PackageContent>, referenced: &HashSet<PackageVersionId>) {
        let version = content.package_version_id;
        if let Some(at) = self
            .versions
            .iter()
            .position(|(held, _)| held.package_version_id == version)
        {
            let held = self.versions.remove(at);
            self.versions.push(held);
            return;
        }
        if self.versions.len() >= RETAINED_MAX {
            let at = self
                .versions
                .iter()
                .position(|(held, _)| !referenced.contains(&held.package_version_id))
                .unwrap_or(0);
            let (dropped, _) = self.versions.remove(at);
            let still_referenced = referenced.contains(&dropped.package_version_id);
            tracing::info!(
                package_version_id = %dropped.package_version_id,
                still_referenced,
                "the edge no longer holds a package version; its open orders are checked under the current one"
            );
        }
        self.versions
            .push((Arc::clone(content), Arc::new(Compiled::of(content))));
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
    /// The package version an order opening now runs under, held with its conditions compiled so
    /// a later publish does not change the order's rules. `None` under the default package.
    pub(in crate::app) fn opening_package_version(&self) -> Option<PackageVersionId> {
        let package = Arc::clone(self.session().package.as_ref()?);
        let referenced = self.lock_projection().referenced_versions();
        self.lock_packages().hold(&package, &referenced);
        Some(package.package_version_id)
    }

    fn lock_packages(&self) -> std::sync::MutexGuard<'_, Retained> {
        self.packages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The version whose rules check `order_id`: the one it opened under while the edge holds it,
    /// else the current one, logged. `None` for an order opened under the default package, or when
    /// the store now runs the default package.
    fn package_for(&self, order_id: OrderId, version: Option<PackageVersionId>) -> Option<Held> {
        let version = version?;
        if let Some(held) = self.lock_packages().get(version) {
            return Some(held);
        }
        let current = Arc::clone(self.session().package.as_ref()?);
        let referenced = self.lock_projection().referenced_versions();
        let mut packages = self.lock_packages();
        if current.package_version_id != version {
            tracing::info!(
                order_id = %order_id,
                opened_under = %version,
                checked_under = %current.package_version_id,
                "an order's package version is no longer held; checking it under the current one"
            );
        }
        packages.hold(&current, &referenced);
        packages.get(current.package_version_id)
    }

    /// Re-checks a command for `action_id` on `order_id` against the flows of the version the order
    /// opened under (see [`Self::package_for`]): the variant of each flow for the order's channel,
    /// and its guards answered from the order's facts. Passes when the order runs under the default
    /// package, or its version has no guard for the act.
    ///
    /// # Errors
    ///
    /// [`AppError::FlowPrerequisiteUnmet`] naming the flow and the step not done.
    pub(in crate::app) fn check_flow_guard(
        &self,
        action_id: &str,
        order_id: OrderId,
    ) -> Result<(), AppError> {
        let (channel, guest_count, line_count, version) = {
            let projection = self.lock_projection();
            (
                projection
                    .order_origin(order_id)
                    .and_then(|origin| origin.channel),
                projection.guest_count(order_id),
                projection.lines_for_order(order_id).len(),
                projection.package_version(order_id),
            )
        };
        let Some((package, compiled)) = self.package_for(order_id, version) else {
            return Ok(());
        };
        if package.flows.is_empty() {
            return Ok(());
        }
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
        let conditions = EdgeConditions {
            compiled: Some(compiled),
            ..EdgeConditions::with(order_facts(channel, guest_count, line_count))
        };
        match unmet_guard(&flows, action_id, &conditions) {
            Some(unmet) => Err(AppError::FlowPrerequisiteUnmet {
                flow_id: unmet.flow_id,
                step_id: unmet.step_id,
            }),
            None => Ok(()),
        }
    }
}
