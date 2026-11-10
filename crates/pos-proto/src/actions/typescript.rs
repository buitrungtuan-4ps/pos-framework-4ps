// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till's view of the catalogue: one TypeScript module rendered from [`CATALOGUE`], so the
//! till binds a button to an act by the same declaration the edge and the cloud read (ADR-0191
//! decision 3). A test holds the committed file to this rendering, as the snapshots are held.

use core::fmt::Write as _;

use super::{ActionEffect, ActionFrequency, ActionParamKind, CATALOGUE};
use crate::wire_enum::WireEnum;

/// Where the rendered module is committed, relative to the repository root.
pub const TYPESCRIPT_PATH: &str = "ui/src/package/actions.ts";

/// Where the same module is committed for the console's designers, which build without `ui/`
/// on disk.
pub const CONSOLE_TYPESCRIPT_PATH: &str = "dashboard/src/package/actions.ts";

/// `"A" | "B" | …` over every token of a wire enum.
fn union<E: WireEnum>() -> String {
    E::ALL
        .iter()
        .map(|value| format!("\"{}\"", value.as_wire()))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// `["a", "b"]`.
fn strings(values: &[&str]) -> String {
    let quoted: Vec<String> = values.iter().map(|value| format!("\"{value}\"")).collect();
    format!("[{}]", quoted.join(", "))
}

/// The module: the token types, the entry shape and every entry, one per line.
#[must_use]
pub fn render_typescript() -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "// @generated from crates/pos-proto/src/actions/catalogue.rs — do not edit.\n\
         // Regenerate with `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot`.\n\
         //\n\
         // The action catalogue (ADR-0191): every act a package's button, flow step or reaction may\n\
         // name, with its effect class, tap frequency, parameters, edge routes, permissions, the\n\
         // refusals the till can predict and the release that introduced it.\n\
         \n\
         export type ActionEffect = {};\n\
         export type ActionFrequency = {};\n\
         export type ActionParamKind = {};\n\
         \n\
         /** One parameter of an act. */\n\
         export interface ActionParam {{\n    readonly name: string;\n    readonly kind: ActionParamKind;\n    readonly required: boolean;\n}}\n\
         \n\
         /** One entry of the catalogue. */\n\
         export interface Action {{\n    readonly action_id: string;\n    readonly effect: ActionEffect;\n    readonly frequency: ActionFrequency;\n    readonly params: readonly ActionParam[];\n    readonly routes: readonly string[];\n    readonly permissions: readonly string[];\n    readonly refusals: readonly string[];\n    readonly since: string;\n}}\n\
         \n\
         /** Every act, in the catalogue's order. */\n\
         export const ACTIONS = [\n",
        union::<ActionEffect>(),
        union::<ActionFrequency>(),
        union::<ActionParamKind>(),
    );
    for action in CATALOGUE {
        let params: Vec<String> = action
            .params
            .iter()
            .map(|param| {
                format!(
                    "{{ name: \"{}\", kind: \"{}\", required: {} }}",
                    param.name,
                    param.kind.as_wire(),
                    param.required
                )
            })
            .collect();
        let _ = writeln!(
            out,
            "    {{ action_id: \"{}\", effect: \"{}\", frequency: \"{}\", params: [{}], routes: {}, permissions: {}, refusals: {}, since: \"{}\" }},",
            action.action_id,
            action.effect.as_wire(),
            action.frequency.as_wire(),
            params.join(", "),
            strings(action.routes),
            strings(action.permissions),
            strings(action.refusals),
            action.since,
        );
    }
    out.push_str(
        "] as const satisfies readonly Action[];\n\
         \n\
         /** The id of an act the catalogue holds. */\n\
         export type ActionId = (typeof ACTIONS)[number][\"action_id\"];\n",
    );
    out
}
