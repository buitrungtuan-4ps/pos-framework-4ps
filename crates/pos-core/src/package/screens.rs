// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Whether a package's screens may be published and applied
//! ([ADR-0190](../../../../docs/adr/0190-a-screen-is-drawn-from-the-package.md),
//! [ADR-0203](../../../../docs/adr/0203-a-component-reads-its-own-data.md) decision 4).
//!
//! [`check_screens`] checks every variant of every screen against this release's component and
//! action catalogues, so a version that publishes cannot fail to draw for a reason the cloud could
//! have seen. The cloud runs it before a version is stored and the edge again before it runs one.
//! Whether each translation key has an `en` value is the cloud's to check, against the tenant's
//! grid (ADR-0190 decision 6).
//!
//! A refusal names where the fault is by position (the screen's in `screens`, the node's in the
//! tree's pre-order, the root being 0), never by value, so it can be shown and logged.

use std::collections::BTreeMap;

use pos_proto::actions::{Action, ActionParamKind};
use pos_proto::devices::DeviceClass;
use pos_proto::package::{TimeSlot, Variant};
use pos_proto::screen::{
    ActionBinding, Component, DEPTH_MAX, NODES_MAX, ParamValue, PropertyKind, PropertyValue,
    SCREEN_ID_CHARS_MAX, SCREENS_MAX, Screen, ScreenNode,
};
use pos_proto::wire_enum::WireEnum;

use super::content::is_token;
use super::{LABEL_KEY_CHARS_MAX, VariantRefusal, check_variants};

/// What is wrong with a screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenFault {
    /// More than [`SCREENS_MAX`] screens, counted by `screen_id`.
    TooManyScreens,
    /// The `screen_id` is not 1 to [`SCREEN_ID_CHARS_MAX`] characters of `a`–`z`, `0`–`9` and `_`.
    ScreenId,
    /// The screen's variants cannot be told apart or lack a base ([`check_variants`]).
    Variants(VariantRefusal),
    /// More than [`NODES_MAX`] nodes.
    TooManyNodes,
    /// Deeper than [`DEPTH_MAX`] levels.
    TooDeep,
    /// A node names a component this release does not have.
    UnknownComponent(usize),
    /// A node's component is not drawn on a device class the variant may run on.
    DeviceClass(usize),
    /// A node holds children, and its component holds none.
    Children(usize),
    /// A node gives a property its component does not take.
    UnknownProperty(usize),
    /// A node leaves out a property its component requires.
    MissingProperty(usize, &'static str),
    /// A property's value is not of its kind, not one of its tokens, outside its bounds, or a
    /// malformed translation key.
    PropertyValue(usize, &'static str),
    /// An action property names an act the catalogue does not have.
    UnknownAction(usize, &'static str),
    /// An action property fixes a parameter its act does not take, one of the wrong kind, or an
    /// amount or something a person enters, which a package never fixes.
    FixedParam(usize, &'static str),
    /// An act's required parameter is neither supplied by the component nor fixed by the package.
    MissingParam(usize, &'static str),
}

/// A screen refused, by its position in `screens`, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRefusal {
    /// The variant's position in the content's `screens`.
    pub screen: usize,
    /// What is wrong.
    pub fault: ScreenFault,
}

impl core::fmt::Display for ScreenRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "screen {}: ", self.screen)?;
        match self.fault {
            ScreenFault::TooManyScreens => write!(f, "a package has at most {SCREENS_MAX} screens"),
            ScreenFault::ScreenId => write!(
                f,
                "an id is 1 to {SCREEN_ID_CHARS_MAX} characters of a-z, 0-9 and _"
            ),
            ScreenFault::Variants(refusal) => write!(f, "{refusal}"),
            ScreenFault::TooManyNodes => write!(f, "a screen has at most {NODES_MAX} nodes"),
            ScreenFault::TooDeep => write!(f, "a screen is at most {DEPTH_MAX} levels deep"),
            ScreenFault::UnknownComponent(node) => {
                write!(
                    f,
                    "node {node} names a component this release does not have"
                )
            }
            ScreenFault::DeviceClass(node) => write!(
                f,
                "node {node}'s component is not drawn on every device class the screen runs on"
            ),
            ScreenFault::Children(node) => {
                write!(f, "node {node}'s component holds no children")
            }
            ScreenFault::UnknownProperty(node) => {
                write!(
                    f,
                    "node {node} gives a property its component does not take"
                )
            }
            ScreenFault::MissingProperty(node, property) => {
                write!(f, "node {node} needs `{property}`")
            }
            ScreenFault::PropertyValue(node, property) => {
                write!(f, "node {node}: `{property}` is not a value of its kind")
            }
            ScreenFault::UnknownAction(node, property) => {
                write!(
                    f,
                    "node {node}: `{property}` names an act the catalogue does not have"
                )
            }
            ScreenFault::FixedParam(node, property) => write!(
                f,
                "node {node}: `{property}` fixes a parameter its act does not take, of the wrong \
                 kind, or an amount or entry a person makes"
            ),
            ScreenFault::MissingParam(node, param) => write!(
                f,
                "node {node}: the act needs `{param}`, which neither the component supplies nor \
                 the package fixes"
            ),
        }
    }
}

impl core::error::Error for ScreenRefusal {}

/// Checks a package's screens, every variant of each: the count and ids, each screen's variants,
/// and each tree's size, depth, components, properties and acts.
///
/// # Errors
///
/// The first [`ScreenRefusal`] found, in the order the screens are listed.
pub fn check_screens(
    screens: &[Variant<Screen>],
    time_slots: &[TimeSlot],
) -> Result<(), ScreenRefusal> {
    let mut by_id: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, variant) in screens.iter().enumerate() {
        let refuse = |fault| ScreenRefusal {
            screen: index,
            fault,
        };
        if !is_token(&variant.body.screen_id, SCREEN_ID_CHARS_MAX, |c| c == '_') {
            return Err(refuse(ScreenFault::ScreenId));
        }
        let positions = by_id.entry(variant.body.screen_id.as_str()).or_default();
        positions.push(index);
        if by_id.len() > SCREENS_MAX {
            return Err(refuse(ScreenFault::TooManyScreens));
        }
        let class = variant.selector.device_class.known();
        check_tree(&variant.body.root, class).map_err(refuse)?;
    }
    for positions in by_id.values() {
        let variants: Vec<Variant<()>> = positions
            .iter()
            .filter_map(|&index| screens.get(index))
            .map(|variant| Variant {
                selector: variant.selector.clone(),
                body: (),
            })
            .collect();
        if let Err(refusal) = check_variants(&variants, time_slots) {
            return Err(ScreenRefusal {
                screen: positions.first().copied().unwrap_or_default(),
                fault: ScreenFault::Variants(refusal),
            });
        }
    }
    Ok(())
}

/// Walks the tree in pre-order, without recursion: a published tree is at most [`DEPTH_MAX`] deep,
/// but one that is not must still be refused without exhausting the stack.
fn check_tree(root: &ScreenNode, class: DeviceClass) -> Result<(), ScreenFault> {
    let mut pending: Vec<(&ScreenNode, usize)> = vec![(root, 1)];
    let mut index = 0;
    while let Some((node, depth)) = pending.pop() {
        if index >= NODES_MAX {
            return Err(ScreenFault::TooManyNodes);
        }
        if depth > DEPTH_MAX {
            return Err(ScreenFault::TooDeep);
        }
        check_node(index, node, class)?;
        pending.extend(node.children.iter().rev().map(|child| (child, depth + 1)));
        index += 1;
    }
    Ok(())
}

fn check_node(index: usize, node: &ScreenNode, class: DeviceClass) -> Result<(), ScreenFault> {
    let component =
        Component::find(&node.component_id).ok_or(ScreenFault::UnknownComponent(index))?;
    let drawn = if class == DeviceClass::Unspecified {
        // A variant for no class, or for one device by id, may run on any class.
        DeviceClass::ALL
            .iter()
            .filter(|known| **known != DeviceClass::Unspecified)
            .all(|known| component.device_classes.contains(known))
    } else {
        component.device_classes.contains(&class)
    };
    if !drawn {
        return Err(ScreenFault::DeviceClass(index));
    }
    if !component.holds_children && !node.children.is_empty() {
        return Err(ScreenFault::Children(index));
    }
    if node
        .properties
        .keys()
        .any(|name| component.property(name).is_none())
    {
        return Err(ScreenFault::UnknownProperty(index));
    }
    for property in component.properties {
        let Some(value) = node.properties.get(property.name) else {
            if property.required {
                return Err(ScreenFault::MissingProperty(index, property.name));
            }
            continue;
        };
        let name = property.name;
        let fits = match (property.kind, value) {
            (PropertyKind::TranslationKey, PropertyValue::Text(key)) => {
                is_token(key, LABEL_KEY_CHARS_MAX, |c| c == '_' || c == '.')
            }
            (PropertyKind::Token, PropertyValue::Text(token)) => {
                property.tokens.contains(&token.as_str())
            }
            (PropertyKind::Int, PropertyValue::Number(number)) => {
                (property.bounds.0..=property.bounds.1).contains(number)
            }
            (PropertyKind::Bool, PropertyValue::Flag(_)) => true,
            (PropertyKind::Action, PropertyValue::Action(binding)) => {
                check_binding(index, name, binding, component.supplies)?;
                true
            }
            _ => false,
        };
        if !fits {
            return Err(ScreenFault::PropertyValue(index, name));
        }
    }
    Ok(())
}

/// An action property: a catalogued act, parameters it takes of their own kinds and never an
/// amount or an entry, and every required one supplied or fixed (ADR-0203 decision 3).
fn check_binding(
    index: usize,
    property: &'static str,
    binding: &ActionBinding,
    supplies: &[&str],
) -> Result<(), ScreenFault> {
    let action =
        Action::find(&binding.action_id).ok_or(ScreenFault::UnknownAction(index, property))?;
    for (name, value) in &binding.params {
        let param = action.params.iter().find(|param| param.name == name);
        let fits = param.is_some_and(|param| {
            matches!(
                (param.kind, value),
                (
                    ActionParamKind::Id | ActionParamKind::Token | ActionParamKind::Text,
                    ParamValue::Text(_),
                ) | (ActionParamKind::Count, ParamValue::Number(_))
                    | (ActionParamKind::IdList, ParamValue::List(_))
            )
        });
        if !fits {
            return Err(ScreenFault::FixedParam(index, property));
        }
    }
    match action.params.iter().find(|param| {
        param.required
            && !binding.params.contains_key(param.name)
            && !supplies.contains(&param.name)
    }) {
        Some(param) => Err(ScreenFault::MissingParam(index, param.name)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
