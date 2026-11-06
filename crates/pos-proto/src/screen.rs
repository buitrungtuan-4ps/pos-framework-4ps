// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A package's screens, and the component catalogue they are drawn from
//! ([ADR-0190](../../../docs/adr/0190-a-screen-is-drawn-from-the-package.md),
//! [ADR-0203](../../../docs/adr/0203-a-component-reads-its-own-data.md)).
//!
//! A [`Screen`] is a tree of [`ScreenNode`]s. Each node names a [`Component`] of this release's
//! [`CATALOGUE`] and gives it properties. The package says which component goes where, never where
//! data comes from: each component reads one fixed part of the screen's context (its `reads`), and
//! a property is only a literal of one of five [`PropertyKind`]s. An action property names a
//! catalogue act ([`crate::actions`]) with fixed parameters; the component supplies the contextual
//! ids it lists in `supplies`, such as a button its `menu_item_id`.
//!
//! The catalogue has three readers: `pos-core` validates a screen against it at publish and on
//! apply, the till's renderer mounts the component it names, and the console will offer it. It is
//! held by `docs/snapshots/components.txt` ([`render_snapshot`]), which `cargo xtask snapshot`
//! diffs against the base branch: a component, a property and its kind are deprecated, never
//! removed or renamed. The till's view is `ui/src/package/components.ts` ([`render_typescript`]).
//!
//! No `deny_unknown_fields`, as for every published node.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::devices::DeviceClass;
use crate::wire_enum;
use crate::wire_enum::WireEnum;

mod catalogue;
#[cfg(test)]
mod tests;
mod typescript;

pub use catalogue::CATALOGUE;
pub use typescript::{TYPESCRIPT_PATH, render_typescript};

/// Where the rendered snapshot is committed, relative to the repository root.
pub const SNAPSHOT_PATH: &str = "docs/snapshots/components.txt";

/// The most screens a package declares, counted by `screen_id`.
pub const SCREENS_MAX: usize = 64;

/// The most nodes in one screen (ADR-0190 decision 3).
pub const NODES_MAX: usize = 400;

/// The deepest a screen's tree goes, its root at depth 1 (ADR-0190 decision 3).
pub const DEPTH_MAX: usize = 12;

/// The longest `screen_id`, in characters of `a`–`z`, `0`–`9` and `_`.
pub const SCREEN_ID_CHARS_MAX: usize = 64;

wire_enum! {
    /// What a component property holds (ADR-0203 decision 2). A property is always a literal.
    PropertyKind, prefix = "PROPERTY_KIND";
    /// A key of the tenant's translation grid; the till shows its text in the till's language.
    TranslationKey = "TRANSLATION_KEY",
    /// One value from the list the property declares, such as a variant.
    Token = "TOKEN",
    /// A whole number within the property's bounds, such as a column count.
    Int = "INT",
    /// On or off.
    Bool = "BOOL",
    /// A catalogue act and its fixed parameters ([`ActionBinding`]).
    Action = "ACTION",
}

/// One property a component takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Property {
    /// The property's name, in `snake_case`.
    pub name: &'static str,
    /// What it holds.
    pub kind: PropertyKind,
    /// Whether a node of the component must give it.
    pub required: bool,
    /// The values a [`PropertyKind::Token`] may take. Empty for any other kind.
    pub tokens: &'static [&'static str],
    /// The least and the greatest a [`PropertyKind::Int`] may be. Both zero for any other kind.
    pub bounds: (i64, i64),
}

/// One entry of the component catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Component {
    /// The component's id, in `snake_case`.
    pub component_id: &'static str,
    /// The part of the screen's context it shows, such as `order` or `layout`; empty when it
    /// shows only its own properties.
    pub reads: &'static str,
    /// The properties it takes.
    pub properties: &'static [Property],
    /// Whether a node of it holds child nodes.
    pub holds_children: bool,
    /// The action parameters it supplies from what it shows, such as `menu_item_id`. An action
    /// property's required parameter is either here or fixed in the package (ADR-0203 decision 3).
    pub supplies: &'static [&'static str],
    /// The device classes it is drawn on; a node of it on any other class is refused at publish
    /// (ADR-0190 decision 5).
    pub device_classes: &'static [DeviceClass],
    /// What a till whose release lacks it draws instead, if anything; with none it draws a slot
    /// that says the till needs an update (ADR-0190 decision 4).
    pub fallback_component: Option<&'static str>,
    /// The first release that draws it.
    pub since: &'static str,
    /// What it shows, in one sentence.
    pub summary: &'static str,
}

impl Component {
    /// The entry for `component_id`, if the catalogue has one.
    #[must_use]
    pub fn find(component_id: &str) -> Option<&'static Self> {
        CATALOGUE
            .iter()
            .find(|component| component.component_id == component_id)
    }

    /// The property called `name`, if the component takes one.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<&'static Property> {
        self.properties
            .iter()
            .find(|property| property.name == name)
    }
}

/// One screen of a package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Screen {
    /// The screen's id within its package, which an `ui.screen.open` act and a flow step name. Its
    /// variants share it.
    pub screen_id: String,
    /// The tree the screen is drawn from.
    pub root: ScreenNode,
}

/// One node of a screen: a component and what the package gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ScreenNode {
    /// The catalogue component it draws.
    pub component_id: String,
    /// Its properties, by name. Left off the wire while it gives none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, PropertyValue>,
    /// The nodes it holds, in order, when its component holds children.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(no_recursion))]
    pub children: Vec<ScreenNode>,
}

/// A property's value: the literal itself, read by the kind the catalogue gives the property.
///
/// Untagged, so `true`, `3` and `"order.send"` are the values themselves. A
/// [`PropertyKind::TranslationKey`] and a [`PropertyKind::Token`] are both text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum PropertyValue {
    /// A [`PropertyKind::Bool`].
    Flag(bool),
    /// A [`PropertyKind::Int`].
    Number(i64),
    /// A [`PropertyKind::TranslationKey`] or a [`PropertyKind::Token`].
    Text(String),
    /// A [`PropertyKind::Action`].
    Action(ActionBinding),
}

/// An action property: a catalogue act and the parameters the package fixes for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActionBinding {
    /// The act, by its catalogue id.
    pub action_id: String,
    /// The parameters fixed here, by name. An amount and anything a person enters are never fixed
    /// by a package (ADR-0191 decision 2).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, ParamValue>,
}

/// A fixed parameter's value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum ParamValue {
    /// A count.
    Number(i64),
    /// An id, a token or text.
    Text(String),
    /// A list of ids.
    List(Vec<String>),
}

/// The catalogue as `docs/snapshots/components.txt` holds it: one fact per line. A bare
/// `component_id` and its `property=`, `children=`, `supplies=` and `device_class=` lines are
/// contracts a published package relies on; its `reads=`, `fallback=` and `since=` lines may
/// change.
#[must_use]
pub fn render_snapshot() -> String {
    let mut lines: Vec<String> = Vec::new();
    for component in CATALOGUE {
        let id = component.component_id;
        lines.push(id.to_owned());
        for property in component.properties {
            let need = if property.required {
                "required"
            } else {
                "optional"
            };
            lines.push(format!(
                "{id}\tproperty={}:{}:{need}",
                property.name,
                property.kind.as_wire()
            ));
            for token in property.tokens {
                lines.push(format!("{id}\tproperty={}:token={token}", property.name));
            }
        }
        if !component.reads.is_empty() {
            lines.push(format!("{id}\treads={}", component.reads));
        }
        lines.push(format!("{id}\tchildren={}", component.holds_children));
        for param in component.supplies {
            lines.push(format!("{id}\tsupplies={param}"));
        }
        for class in component.device_classes {
            lines.push(format!("{id}\tdevice_class={}", class.as_wire()));
        }
        if let Some(fallback) = component.fallback_component {
            lines.push(format!("{id}\tfallback={fallback}"));
        }
        lines.push(format!("{id}\tsince={}", component.since));
    }
    lines.sort();
    let mut out = String::from(
        "# Generated from crates/pos-proto/src/screen/catalogue.rs — do not edit.\n\
         # One line per fact. A bare component id and its property, children, supplies and\n\
         # device_class lines are contracts; its reads, fallback and since lines may change.\n\
         # ADR-0190 decision 2.\n",
    );
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}
