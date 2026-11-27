// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `order_channels` node: the order types and sale sources HQ lists for a store
//! ([ADR-0260](../../../docs/adr/0260-order-types-and-sale-sources-are-published-lists.md)).
//!
//! Each order type maps onto a fixed base kind, `ORDER_TYPE_DINE_IN`, `_TAKEAWAY` or `_DELIVERY`,
//! which alone decides the order's `SalesChannel` and so its price and tax: a published type is a
//! reporting refinement, never a money rule. A node of its own rather than fields on `channels`,
//! whose `enabled` list an older edge reads as the channels a store accepts: a `channels` node
//! without it would refuse every order there.
//!
//! An empty or absent `order_types` reads as the three built-ins, and an absent `sources` as none
//! beyond the built-ins, so a store with no node runs exactly as ADR-0259 left it. The edge reads
//! the node leniently; [`check_order_types`] and [`check_sources`] are the strict rule the cloud
//! applies when a value is written.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::sale_channel::{OrderType, SaleSource, SaleToken};
use crate::text::DisplayName;
use crate::wire_enum::WireEnum;

/// The most order types a list holds (ADR-0260 decision 1).
pub const MAX_ORDER_TYPES: usize = 32;
/// The most sale sources a list holds.
pub const MAX_SOURCES: usize = 64;
/// The most languages an entry is named in.
pub const MAX_LANGUAGES: usize = 4;
/// The longest name, in characters.
pub const MAX_NAME_CHARS: usize = 48;
/// The most sources a type applies to: QR, API and the apps.
pub const MAX_APPLIES_TO: usize = 3;

/// One order type the store offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedOrderType {
    /// Its token, `ORDER_TYPE_*`, never reused for another meaning.
    pub order_type: SaleToken,
    /// How the food leaves the store: `ORDER_TYPE_DINE_IN`, `_TAKEAWAY` or `_DELIVERY`. A
    /// built-in's base is itself.
    pub base_order_type: SaleToken,
    /// Its name in each language, by language code: `en` and `vi`. A built-in left unnamed shows
    /// its translation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, DisplayName>,
    /// Whether the till offers it. A type is deactivated, never removed (decision 3).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub active: bool,
    /// Whether the till offers it first. At most one type is.
    #[serde(default, skip_serializing_if = "is_false")]
    pub default: bool,
    /// The sources, of `SALE_SOURCE_QR`, `_API` and `_APP`, whose orders of its base kind take it
    /// (decision 4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to_sources: Vec<SaleToken>,
}

/// One sale source the store lists beyond the built-ins, or a built-in it renames.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedSaleSource {
    /// Its token, `SALE_SOURCE_*`.
    pub source: SaleToken,
    /// Its name in each language, by language code.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, DisplayName>,
    /// Whether the till offers it.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub active: bool,
}

/// The `order_channels` node. No `deny_unknown_fields`, as for every published node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedOrderChannels {
    /// The order types; read them through [`Self::order_types`]. A list that does not read as
    /// entries reads as absent.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub order_types: Vec<PublishedOrderType>,
    /// The sale sources beyond the built-ins.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub sources: Vec<PublishedSaleSource>,
}

impl PublishedOrderChannels {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "order_channels";

    /// The store's order types: its list, or the three built-ins where it lists none.
    #[must_use]
    pub fn order_types(&self) -> Vec<PublishedOrderType> {
        if self.order_types.is_empty() {
            built_in_types()
        } else {
            self.order_types.clone()
        }
    }

    /// The base kind of an active type the store lists, `None` for any other token.
    #[must_use]
    pub fn active_base(&self, order_type: &SaleToken) -> Option<OrderType> {
        self.order_types()
            .iter()
            .find(|kept| kept.active && kept.order_type == *order_type)
            .and_then(|kept| base_kind(&kept.base_order_type))
    }

    /// Whether the till may record `source`: the store's own till, or an active source the store
    /// lists that is no built-in (a kiosk, a QR page, the API and an app record their own).
    #[must_use]
    pub fn till_may_record(&self, source: &SaleToken) -> bool {
        *source == SaleToken::POS
            || (SaleSource::from_wire(source.as_str()).is_none()
                && self
                    .sources
                    .iter()
                    .any(|kept| kept.active && kept.source == *source))
    }

    /// The type an order from `source` of base kind `base` records: the first active type that
    /// applies to that source and has that base (decision 4), else none.
    #[must_use]
    pub fn matched_type(&self, source: SaleSource, base: OrderType) -> Option<SaleToken> {
        let source = SaleToken::of(source);
        self.order_types()
            .into_iter()
            .find(|kept| {
                kept.active
                    && kept.applies_to_sources.contains(&source)
                    && base_kind(&kept.base_order_type) == Some(base)
            })
            .map(|kept| kept.order_type)
    }

    /// What each listed type and source is called, by token: what a report shows beside a token.
    #[must_use]
    pub fn names(&self) -> BTreeMap<String, BTreeMap<String, DisplayName>> {
        let types = self
            .order_types
            .iter()
            .map(|kept| (kept.order_type.as_str().to_owned(), kept.names.clone()));
        let sources = self
            .sources
            .iter()
            .map(|kept| (kept.source.as_str().to_owned(), kept.names.clone()));
        types
            .chain(sources)
            .filter(|(_, names)| !names.is_empty())
            .collect()
    }
}

/// The three built-in types, each its own base: what a store that lists none offers.
#[must_use]
pub fn built_in_types() -> Vec<PublishedOrderType> {
    [OrderType::DineIn, OrderType::Takeaway, OrderType::Delivery]
        .into_iter()
        .map(|kind| PublishedOrderType {
            order_type: SaleToken::of(kind),
            base_order_type: SaleToken::of(kind),
            names: BTreeMap::new(),
            active: true,
            default: false,
            applies_to_sources: Vec::new(),
        })
        .collect()
}

/// The base kind a token names, `None` unless it is one of the three.
#[must_use]
pub fn base_kind(token: &SaleToken) -> Option<OrderType> {
    OrderType::from_wire(token.as_str()).filter(|kind| *kind != OrderType::Unspecified)
}

/// Why a value is not a list a setting takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListRefusal {
    /// Not a list of entries of the right shape.
    NotAList,
    /// Longer than its maximum, or a name, a language list or a source list over its limit.
    OverLimit,
}

/// The strict check of an `order_types` value, as the cloud applies it at write.
///
/// # Errors
///
/// [`ListRefusal`] naming what is wrong.
pub fn check_order_types(value: &serde_json::Value) -> Result<(), ListRefusal> {
    const FIELDS: &[&str] = &[
        "order_type",
        "base_order_type",
        "names",
        "active",
        "default",
        "applies_to_sources",
    ];
    entries(value, FIELDS)?;
    let list: Vec<PublishedOrderType> =
        serde_json::from_value(value.clone()).map_err(|_| ListRefusal::NotAList)?;
    if list.len() > MAX_ORDER_TYPES {
        return Err(ListRefusal::OverLimit);
    }
    let apps = [SaleSource::Qr, SaleSource::Api, SaleSource::App].map(SaleToken::of);
    for (index, kept) in list.iter().enumerate() {
        let built_in = base_kind(&kept.order_type);
        let sound = kept.order_type.is_order_type()
            && kept.order_type != SaleToken::TYPE_NOT_RECORDED
            && base_kind(&kept.base_order_type)
                .is_some_and(|base| built_in.is_none_or(|own| own == base))
            && (built_in.is_some() || kept.names.contains_key("en"))
            && (!kept.default || kept.active)
            && kept
                .applies_to_sources
                .iter()
                .all(|source| apps.contains(source))
            && distinct(&kept.applies_to_sources)
            && !list
                .iter()
                .take(index)
                .any(|earlier| earlier.order_type == kept.order_type);
        if !sound {
            return Err(ListRefusal::NotAList);
        }
        if kept.applies_to_sources.len() > MAX_APPLIES_TO {
            return Err(ListRefusal::OverLimit);
        }
        check_names(&kept.names)?;
    }
    if list.iter().filter(|kept| kept.default).count() > 1 {
        return Err(ListRefusal::NotAList);
    }
    Ok(())
}

/// The strict check of a `sources` value, as the cloud applies it at write.
///
/// # Errors
///
/// [`ListRefusal`] naming what is wrong.
pub fn check_sources(value: &serde_json::Value) -> Result<(), ListRefusal> {
    entries(value, &["source", "names", "active"])?;
    let list: Vec<PublishedSaleSource> =
        serde_json::from_value(value.clone()).map_err(|_| ListRefusal::NotAList)?;
    if list.len() > MAX_SOURCES {
        return Err(ListRefusal::OverLimit);
    }
    for (index, kept) in list.iter().enumerate() {
        let built_in = SaleSource::from_wire(kept.source.as_str());
        let sound = kept.source.is_source()
            && !matches!(built_in, Some(SaleSource::Unspecified | SaleSource::App))
            && (built_in.is_some() || kept.names.contains_key("en"))
            && !list
                .iter()
                .take(index)
                .any(|earlier| earlier.source == kept.source);
        if !sound {
            return Err(ListRefusal::NotAList);
        }
        check_names(&kept.names)?;
    }
    Ok(())
}

/// The tokens a list holds, for the cloud's rule that a scope's list never drops one (decision
/// 3): `field` is `order_type` or `source`.
#[must_use]
pub fn tokens_of(value: &serde_json::Value, field: &str) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get(field)?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a value is a list of objects, each holding only `fields`.
fn entries(value: &serde_json::Value, fields: &[&str]) -> Result<(), ListRefusal> {
    let items = value.as_array().ok_or(ListRefusal::NotAList)?;
    let shaped = items.iter().all(|item| {
        item.as_object()
            .is_some_and(|object| object.keys().all(|key| fields.contains(&key.as_str())))
    });
    shaped.then_some(()).ok_or(ListRefusal::NotAList)
}

/// An entry's names: at most four languages, each a code of two to eight lower-case letters or
/// `-`, each name one to 48 characters with something but spaces in it.
fn check_names(names: &BTreeMap<String, DisplayName>) -> Result<(), ListRefusal> {
    if names.len() > MAX_LANGUAGES {
        return Err(ListRefusal::OverLimit);
    }
    for (language, name) in names {
        let code = (2..=8).contains(&language.len())
            && language
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'-');
        if !code || name.as_str().trim().is_empty() {
            return Err(ListRefusal::NotAList);
        }
        if name.as_str().chars().count() > MAX_NAME_CHARS {
            return Err(ListRefusal::OverLimit);
        }
    }
    Ok(())
}

fn distinct(tokens: &[SaleToken]) -> bool {
    tokens
        .iter()
        .enumerate()
        .all(|(index, token)| !tokens.iter().take(index).any(|earlier| earlier == token))
}

const fn yes() -> bool {
    true
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if"
)]
const fn is_true(value: &bool) -> bool {
    *value
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// A list as the node carries it: one that does not read as entries reads as absent, so it costs
/// the node its other list nothing.
fn lenient<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    Ok(serde_json::from_value(serde_json::Value::deserialize(deserializer)?).unwrap_or_default())
}

#[cfg(test)]
mod tests;
