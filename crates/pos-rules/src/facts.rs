// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a condition can see (ADR-0193 decision 4): read-only views, built by the caller from the
//! state it holds, with amounts as integers in minor units and no personal data.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use cel::Value;

/// A view a condition may read. Nothing else is in scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Variable {
    /// The order on screen, or the order a command acts on.
    Order,
    /// The line an act concerns.
    Line,
    /// The bill on screen.
    Bill,
    /// The sales channel, as its wire token.
    Channel,
    /// The device, by its class.
    Device,
    /// The signed-in person's role: never their name or id.
    Role,
    /// The store's local time: the minute of the day, the weekday and the package's time slot,
    /// read through `ClockSource` by the caller.
    Now,
}

impl Variable {
    /// Every view, in a fixed order.
    pub const ALL: [Self; 7] = [
        Self::Order,
        Self::Line,
        Self::Bill,
        Self::Channel,
        Self::Device,
        Self::Role,
        Self::Now,
    ];

    /// The name a condition reads it by.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Order => "order",
            Self::Line => "line",
            Self::Bill => "bill",
            Self::Channel => "channel",
            Self::Device => "device",
            Self::Role => "role",
            Self::Now => "now",
        }
    }

    /// The view a condition names, if it is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|variable| variable.as_str() == name)
    }
}

/// One value a condition can read. There is no fraction: an amount is an [`Fact::Int`] in the
/// currency's minor unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fact {
    /// True or false.
    Bool(bool),
    /// A whole number: an amount in minor units, a count, a minute of the day.
    Int(i64),
    /// A token or an id.
    Text(String),
    /// A list, such as an order's category ids.
    List(Vec<Fact>),
    /// Named fields, such as an order's.
    Map(BTreeMap<String, Fact>),
}

impl Fact {
    /// The value the evaluator reads.
    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Bool(value) => Value::Bool(*value),
            Self::Int(value) => Value::Int(*value),
            Self::Text(value) => Value::String(Arc::new(value.clone())),
            Self::List(items) => Value::List(Arc::new(items.iter().map(Self::to_value).collect())),
            Self::Map(fields) => fields
                .iter()
                .map(|(name, fact)| (name.clone(), fact.to_value()))
                .collect::<HashMap<String, Value>>()
                .into(),
        }
    }
}

/// The views one evaluation reads. A view left unset is absent, and a condition reading it fails
/// rather than guessing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    views: BTreeMap<Variable, Fact>,
}

impl Facts {
    /// No views.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a view, replacing what it held.
    #[must_use]
    pub fn with(mut self, variable: Variable, fact: Fact) -> Self {
        self.views.insert(variable, fact);
        self
    }

    /// The views set, in [`Variable`] order.
    pub fn iter(&self) -> impl Iterator<Item = (Variable, &Fact)> {
        self.views.iter().map(|(variable, fact)| (*variable, fact))
    }
}
