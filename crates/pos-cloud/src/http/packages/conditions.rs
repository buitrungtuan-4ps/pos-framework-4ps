// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A version's conditions, compiled at publish
//! ([ADR-0193](../../../../../docs/adr/0193-conditions-and-reactions-are-written-in-cel.md)
//! decisions 2 and 6).
//!
//! The same evaluator the edge compiles them with on apply (`pos_edge_app::app::flows`) and the
//! till runs, in the same restricted dialect, so a version the publish accepts is one the edge
//! applies. An edge guard may read only the views the edge holds (`pos_rules::Variable::EDGE`,
//! ADR-0192 decision 4). Nothing is answered here: a publish has no order to answer over.

use pos_core::flow::FlowConditions;
use pos_rules::Rules;

/// Compiles each condition a version's flows and reactions carry, and answers none.
#[derive(Debug)]
pub(super) struct PublishConditions {
    rules: Option<Rules>,
}

impl PublishConditions {
    pub(super) fn new() -> Self {
        Self {
            rules: Rules::new().ok(),
        }
    }
}

impl FlowConditions for PublishConditions {
    fn check(&self, condition: &str, edge_facts: bool) -> Result<(), String> {
        let rules = self
            .rules
            .as_ref()
            .ok_or("the rule evaluator could not be built")?;
        let compiled = rules
            .compile(condition)
            .map_err(|refused| refused.to_string())?;
        match compiled.till_view() {
            Some(variable) if edge_facts => Err(format!(
                "`{}` is the till's to read, so an edge guard cannot",
                variable.as_str()
            )),
            Some(_) | None => Ok(()),
        }
    }

    fn holds(&self, _condition: &str) -> Result<bool, String> {
        Err("a publish answers no condition".to_owned())
    }
}
