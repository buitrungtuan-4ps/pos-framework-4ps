// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till's view of the component catalogue and of a screen's shape: one TypeScript module
//! rendered from [`CATALOGUE`] and the wire types beside it, so the till's renderer reads the same
//! declaration the cloud and the edge validate against. A test holds the committed file to this
//! rendering, as the snapshots are held.

use core::fmt::Write as _;

use super::{CATALOGUE, PropertyKind};
use crate::wire_enum::WireEnum;

/// Where the rendered module is committed, relative to the repository root.
pub const TYPESCRIPT_PATH: &str = "ui/src/package/components.ts";

/// Where the same module is committed for the console's designers, which build without `ui/`
/// on disk.
pub const CONSOLE_TYPESCRIPT_PATH: &str = "dashboard/src/package/components.ts";

/// `"A" | "B" | …` over every token of a wire enum.
fn union<E: WireEnum>() -> String {
    E::ALL
        .iter()
        .map(|value| format!("\"{}\"", value.as_wire()))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// `["a", "b"]`.
fn strings<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let quoted: Vec<String> = values
        .into_iter()
        .map(|value| format!("\"{value}\""))
        .collect();
    format!("[{}]", quoted.join(", "))
}

/// The module: the shapes, every component one per line, and the limits.
#[must_use]
pub fn render_typescript() -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "// @generated from crates/pos-proto/src/screen.rs and screen/catalogue.rs — do not edit.\n\
         // Regenerate with `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot`.\n\
         //\n\
         // The component catalogue (ADR-0190, ADR-0203): every component a package's screen may\n\
         // name, with its properties, what it supplies to the acts it binds, the device classes it\n\
         // is drawn on, its fallback and the release that introduced it; and a screen's shape.\n\
         \n\
         export type PropertyKind = {};\n\
         \n\
         /** One property a component takes. */\n\
         export interface ComponentProperty {{\n    readonly name: string;\n    readonly kind: PropertyKind;\n    readonly required: boolean;\n    readonly tokens: readonly string[];\n    readonly min: number;\n    readonly max: number;\n}}\n\
         \n\
         /** One entry of the catalogue. */\n\
         export interface Component {{\n    readonly component_id: string;\n    readonly properties: readonly ComponentProperty[];\n    readonly holds_children: boolean;\n    readonly supplies: readonly string[];\n    readonly device_classes: readonly string[];\n    readonly fallback_component: string | null;\n    readonly since: string;\n}}\n\
         \n\
         /** A fixed parameter of an act a property binds. */\n\
         export type ParamValue = number | string | readonly string[];\n\
         \n\
         /** An action property: an act by its catalogue id, and the parameters the package fixes. */\n\
         export interface ActionBinding {{\n    readonly action_id: string;\n    readonly params?: Readonly<Record<string, ParamValue>>;\n}}\n\
         \n\
         /** A property's value, read by the kind the catalogue gives the property. */\n\
         export type PropertyValue = boolean | number | string | ActionBinding;\n\
         \n\
         /** One node of a screen. */\n\
         export interface ScreenNode {{\n    readonly component_id: string;\n    readonly properties?: Readonly<Record<string, PropertyValue>>;\n    readonly children?: readonly ScreenNode[];\n}}\n\
         \n\
         /** One screen of a package. */\n\
         export interface Screen {{\n    readonly screen_id: string;\n    readonly root: ScreenNode;\n}}\n\
         \n\
         /** The most nodes in one screen, and the deepest its tree goes. */\n\
         export const NODES_MAX = {};\n\
         export const DEPTH_MAX = {};\n\
         \n\
         /** Every component, in the catalogue's order. */\n\
         export const COMPONENTS = [\n",
        union::<PropertyKind>(),
        super::NODES_MAX,
        super::DEPTH_MAX,
    );
    for component in CATALOGUE {
        let properties: Vec<String> = component
            .properties
            .iter()
            .map(|property| {
                format!(
                    "{{ name: \"{}\", kind: \"{}\", required: {}, tokens: {}, min: {}, max: {} }}",
                    property.name,
                    property.kind.as_wire(),
                    property.required,
                    strings(property.tokens.iter().copied()),
                    property.bounds.0,
                    property.bounds.1,
                )
            })
            .collect();
        let fallback = component
            .fallback_component
            .map_or_else(|| "null".to_owned(), |id| format!("\"{id}\""));
        let _ = writeln!(
            out,
            "    {{ component_id: \"{}\", properties: [{}], holds_children: {}, supplies: {}, device_classes: {}, fallback_component: {fallback}, since: \"{}\" }},",
            component.component_id,
            properties.join(", "),
            component.holds_children,
            strings(component.supplies.iter().copied()),
            strings(component.device_classes.iter().map(|class| class.as_wire())),
            component.since,
        );
    }
    out.push_str(
        "] as const satisfies readonly Component[];\n\
         \n\
         /** The id of a component the catalogue holds. */\n\
         export type ComponentId = (typeof COMPONENTS)[number][\"component_id\"];\n",
    );
    out
}
