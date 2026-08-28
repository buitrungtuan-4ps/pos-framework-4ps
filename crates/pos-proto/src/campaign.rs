// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `campaigns` config node ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)):
//! the promotions an operator authored, in the wire shape the edge parses back into the pure
//! campaign engine.
//!
//! `pos_core::campaign::Campaign` is a *runtime* type with no serde — it is the evaluated form the
//! store prices against. This module is the *wire* form: a serializable, field-for-field mirror the
//! cloud compiles from the authoring store and writes as the `campaigns` key on the config tree's
//! Store layer, exactly as `tax`/`menu`/`floor` do. The edge's config apply parses it and calls
//! `pos_core::campaign::campaigns_from_published` to turn it back into `Campaign`s for `evaluate`.
//! The two shapes are kept faithful; the conversion lives in `pos_core::campaign`, the only place
//! that can see both.
//!
//! A campaign carries only what the engine needs to evaluate plus a display `name` for the receipt,
//! audit trail, and console — never a customer identifier or any other T1 field, so the node stays
//! promotion configuration, not personal data.

use serde::{Deserialize, Serialize};

use crate::enums::SalesChannel;
use crate::ids::CampaignId;
use crate::money::{Money, Ratio};
use crate::text::DisplayName;
use crate::wire_enum::Open;

/// The five campaign kinds, in the order the pricing spec (§7) evaluates them. Wire mirror of
/// `pos_core::campaign::CampaignKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishedCampaignKind {
    /// A discount on one item or category.
    ItemLevel,
    /// A combo: a set of items at a set price.
    Combo,
    /// A discount on the whole bill.
    BillLevel,
    /// A voucher code, redeemed atomically against the cloud.
    Voucher,
    /// A manual reduction entered by staff.
    Manual,
}

/// What a campaign takes off. Wire mirror of `pos_core::campaign::Action`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum PublishedAction {
    /// A percentage off the base.
    Percentage {
        /// The rate, as an exact ratio (e.g. 10/100 for 10%).
        rate: Ratio,
    },
    /// A fixed amount off the base, in the base's currency.
    AmountOff {
        /// The amount to subtract.
        amount: Money,
    },
}

/// A weekly schedule window. Wire mirror of `pos_core::campaign::Schedule`; the window is half-open
/// and may wrap past midnight (`start_minute > end_minute`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedSchedule {
    /// The weekday bitmask, `Monday` = bit 0 — the same 7-bit mask
    /// `pos_core::campaign::WeekdaySet` uses (bits above Sunday are ignored).
    pub days: u8,
    /// The first included minute of the day, 0–1439.
    pub start_minute: u16,
    /// The first excluded minute of the day, 0–1439.
    pub end_minute: u16,
}

/// The conditions a campaign requires before it applies. Wire mirror of
/// `pos_core::campaign::Conditions`; every field is optional and absent means unrestricted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedConditions {
    /// A minimum bill total. Absent means no minimum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_bill: Option<Money>,
    /// The sales channels it applies on. Absent means every channel. Wrapped in [`Open`] so a
    /// channel token from a newer cloud round-trips rather than failing the whole node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<Vec<Open<SalesChannel>>>,
    /// The weekly schedule window. Absent means always active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<PublishedSchedule>,
}

/// One authored campaign, in wire form. Faithful mirror of `pos_core::campaign::Campaign` plus the
/// authoring `name` the console and receipt show (the engine does not need a name to evaluate).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedCampaign {
    /// Its stable id — the tiebreak that makes evaluation order total.
    pub id: CampaignId,
    /// The operator-facing name, shown on the receipt line, the audit trail, and the console. Not
    /// used by evaluation.
    pub name: DisplayName,
    /// Which of the five kinds it is.
    pub kind: PublishedCampaignKind,
    /// Priority within its kind and exclusion group; higher applies first.
    pub priority: i32,
    /// An exclusion group: at most one campaign per group applies to a bill. Absent means it stacks
    /// with everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusion_group: Option<u16>,
    /// What it takes off.
    pub action: PublishedAction,
    /// What must hold for it to apply.
    #[serde(default)]
    pub conditions: PublishedConditions,
    /// Remaining quota; absent is unlimited, `Some(0)` is exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_remaining: Option<u32>,
}

/// The `campaigns` config node: every campaign a store evaluates, in wire form.
///
/// A list (not a map) for the same round-trips-in-a-diff reason
/// [`crate::menu::MenuBook`] and [`crate::locale::TaxRateTable`] are lists. Empty is the safe default
/// — a store with no node published simply runs no promotions, the never-blank config contract
/// keeping whatever the edge already holds if a publish is absent or unparseable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedCampaigns {
    #[serde(default)]
    campaigns: Vec<PublishedCampaign>,
}

impl PublishedCampaigns {
    /// An empty node — no campaign, so nothing discounts until the cloud publishes one.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            campaigns: Vec::new(),
        }
    }

    /// A node from its campaigns.
    #[must_use]
    pub const fn from_campaigns(campaigns: Vec<PublishedCampaign>) -> Self {
        Self { campaigns }
    }

    /// Adds a campaign, for building a node in code or a test.
    #[must_use]
    pub fn with(mut self, campaign: PublishedCampaign) -> Self {
        self.campaigns.push(campaign);
        self
    }

    /// Every campaign, in the order the node lists them.
    #[must_use]
    pub fn campaigns(&self) -> &[PublishedCampaign] {
        &self.campaigns
    }

    /// Whether the node carries no campaigns at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.campaigns.is_empty()
    }

    /// How many campaigns the node carries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.campaigns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PublishedAction, PublishedCampaign, PublishedCampaignKind, PublishedCampaigns,
        PublishedConditions, PublishedSchedule,
    };
    use crate::enums::SalesChannel;
    use crate::ids::CampaignId;
    use crate::money::{CurrencyCode, Money, Ratio};
    use crate::text::DisplayName;
    use crate::ulid::Ulid;
    use crate::wire_enum::Open;

    fn sample() -> PublishedCampaign {
        PublishedCampaign {
            id: CampaignId::new(Ulid::from_u128(1)),
            name: DisplayName::new("Happy hour"),
            kind: PublishedCampaignKind::ItemLevel,
            priority: 10,
            exclusion_group: Some(1),
            action: PublishedAction::Percentage {
                rate: Ratio::percent(15).expect("valid percent"),
            },
            conditions: PublishedConditions {
                min_bill: Some(Money::new(CurrencyCode::VND, 100_000)),
                channels: Some(vec![Open::from_known(SalesChannel::DineIn)]),
                schedule: Some(PublishedSchedule {
                    days: 0b0111_1111,
                    start_minute: 16 * 60,
                    end_minute: 17 * 60,
                }),
            },
            quota_remaining: Some(50),
        }
    }

    #[test]
    fn a_campaigns_node_round_trips_through_json() {
        let node = PublishedCampaigns::new().with(sample());
        let json = serde_json::to_string(&node).expect("serialize");
        let back: PublishedCampaigns = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(node, back);
        assert_eq!(back.len(), 1);
    }

    #[test]
    fn absent_optionals_are_omitted_and_reload_as_none() {
        let bare = PublishedCampaign {
            exclusion_group: None,
            conditions: PublishedConditions::default(),
            quota_remaining: None,
            ..sample()
        };
        let json = serde_json::to_string(&bare).expect("serialize");
        assert!(
            !json.contains("exclusion_group"),
            "an absent optional is not emitted: {json}"
        );
        assert!(!json.contains("quota_remaining"), "no quota key: {json}");
        let back: PublishedCampaign = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(bare, back);
    }

    #[test]
    fn an_action_serializes_with_a_type_tag() {
        let amount_off = PublishedAction::AmountOff {
            amount: Money::new(CurrencyCode::VND, 20_000),
        };
        let json = serde_json::to_string(&amount_off).expect("serialize");
        assert!(json.contains("\"type\":\"amount_off\""), "tagged: {json}");
    }

    #[test]
    fn an_empty_node_is_the_default() {
        assert!(PublishedCampaigns::default().is_empty());
    }
}
