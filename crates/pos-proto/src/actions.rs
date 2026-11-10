// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The action catalogue: every act a package's buttons, flow steps and reactions may name, typed
//! ([ADR-0191](../../../docs/adr/0191-every-act-is-in-a-typed-catalogue.md)).
//!
//! One declaration with three readers. The cloud validates a package against it, the edge maps each
//! entry to the command its routes call, and the till's renderer binds a button to it by
//! `action_id`, never by code. Each [`Action`] states:
//!
//! - its `action_id`, `<domain>.<resource>.<verb>` like a permission id;
//! - its [`ActionEffect`], which decides how the till paints it (ADR-0196) and whether a reaction may
//!   run it (ADR-0193 decision 5);
//! - its [`ActionFrequency`], for the tap budget (ADR-0190 decision 7);
//! - its typed [`ActionParam`]s;
//! - the edge routes it calls, as `docs/snapshots/routes.txt` writes them, and the permissions a
//!   person needs for them, which a test in `pos-edge` holds equal to that route table;
//! - the refusals the till can predict before it sends (ADR-0196 safeguard 2); the permission
//!   check is implied by `permissions` and not repeated there;
//! - the release that introduced it, from which the cloud computes a version's minimum edge release
//!   (ADR-0189 decision 5).
//!
//! The catalogue is held by `docs/snapshots/actions.txt`, rendered by [`render_snapshot`] and diffed
//! against the base branch by `cargo xtask snapshot`. An action is deprecated, never removed or
//! renamed, so a published package keeps working (ADR-0191 decision 4). A package composes these
//! acts and cannot declare one (decision 5).

mod catalogue;
#[cfg(test)]
mod tests;
mod typescript;

use crate::wire_enum;
use crate::wire_enum::WireEnum;

pub use catalogue::CATALOGUE;
pub use typescript::{CONSOLE_TYPESCRIPT_PATH, TYPESCRIPT_PATH, render_typescript};

/// Where the rendered snapshot is committed, relative to the repository root.
pub const SNAPSHOT_PATH: &str = "docs/snapshots/actions.txt";

wire_enum! {
    /// What an act does beyond the screen (ADR-0191 decision 2). `ACTION_EFFECT_UNSPECIFIED` is
    /// refused.
    ActionEffect, prefix = "ACTION_EFFECT";
    /// It changes only what the till shows: navigating, opening a panel, filtering a list.
    Ui = "UI",
    /// One edge command that writes events and moves no money.
    EdgeWrite = "EDGE_WRITE",
    /// One existing edge money command. Its amount parameters are bound only to what a person
    /// types or to a figure the edge answered with, never to a rule's result.
    Money = "MONEY",
    /// A call through the cloud gateway (ADR-0198). The till never calls the internet.
    External = "EXTERNAL",
}

wire_enum! {
    /// How often an operator does an act, which sets its tap ceiling from the role's home screen:
    /// two for a common act, three for a rare one (ADR-0109, ADR-0190 decision 7).
    ActionFrequency, prefix = "ACTION_FREQUENCY";
    /// At most two taps.
    Common = "COMMON",
    /// At most three taps.
    Rare = "RARE",
}

wire_enum! {
    /// What a parameter of an act holds.
    ActionParamKind, prefix = "ACTION_PARAM_KIND";
    /// One identifier, named `<resource>_id`.
    Id = "ID",
    /// Several identifiers of one resource, named `<resource>_ids`.
    IdList = "ID_LIST",
    /// A whole number that is not money: a quantity, a guest count, a seat.
    Count = "COUNT",
    /// Money in the currency's minor unit. Bound only to what a person types or to a figure the
    /// edge answered with, never to a rule's result (ADR-0191 decision 2).
    Amount = "AMOUNT",
    /// Text a person types, such as a note for the kitchen.
    Text = "TEXT",
    /// One token from a fixed list the package names, such as a screen id.
    Token = "TOKEN",
    /// A structured value a person enters on the act's own screen (a settle's payments, a split's
    /// parts, a count's denominations). A package never binds it.
    Entered = "ENTERED",
}

/// One parameter of an act.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionParam {
    /// The parameter's name, in `snake_case`.
    pub name: &'static str,
    /// What it holds.
    pub kind: ActionParamKind,
    /// Whether the act cannot run without it.
    pub required: bool,
}

/// A parameter the act cannot run without.
#[must_use]
pub const fn required(name: &'static str, kind: ActionParamKind) -> ActionParam {
    ActionParam {
        name,
        kind,
        required: true,
    }
}

/// A parameter the act may run without.
#[must_use]
pub const fn optional(name: &'static str, kind: ActionParamKind) -> ActionParam {
    ActionParam {
        name,
        kind,
        required: false,
    }
}

/// One entry of the catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    /// The act's id, `<domain>.<resource>.<verb>`.
    pub action_id: &'static str,
    /// What it does beyond the screen.
    pub effect: ActionEffect,
    /// How often it is done, for the tap budget.
    pub frequency: ActionFrequency,
    /// What it takes.
    pub params: &'static [ActionParam],
    /// The edge routes it calls, `METHOD /path` as `docs/snapshots/routes.txt` writes them. Empty
    /// for an [`ActionEffect::Ui`] act.
    pub routes: &'static [&'static str],
    /// The permission ids a person needs for those routes, as their route table gates them.
    pub permissions: &'static [&'static str],
    /// The edge's refusal tokens the till can decide before it sends, beyond the permission.
    pub refusals: &'static [&'static str],
    /// The first release that runs the act.
    pub since: &'static str,
    /// What the act does, in one sentence.
    pub summary: &'static str,
}

impl Action {
    /// The entry for `action_id`, if the catalogue has one.
    #[must_use]
    pub fn find(action_id: &str) -> Option<&'static Self> {
        CATALOGUE
            .iter()
            .find(|action| action.action_id == action_id)
    }
}

/// The catalogue as `docs/snapshots/actions.txt` holds it: one fact per line. A bare `action_id`,
/// its `effect=` and its `param=` lines are contracts a published package relies on; its
/// `frequency=`, `route=`, `permission=`, `refusal=` and `since=` lines may change.
#[must_use]
pub fn render_snapshot() -> String {
    let mut lines: Vec<String> = Vec::new();
    for action in CATALOGUE {
        let id = action.action_id;
        lines.push(id.to_owned());
        lines.push(format!("{id}\teffect={}", action.effect.as_wire()));
        lines.push(format!("{id}\tfrequency={}", action.frequency.as_wire()));
        for param in action.params {
            let need = if param.required {
                "required"
            } else {
                "optional"
            };
            lines.push(format!(
                "{id}\tparam={}:{}:{need}",
                param.name,
                param.kind.as_wire()
            ));
        }
        for route in action.routes {
            lines.push(format!("{id}\troute={route}"));
        }
        for permission in action.permissions {
            lines.push(format!("{id}\tpermission={permission}"));
        }
        for refusal in action.refusals {
            lines.push(format!("{id}\trefusal={refusal}"));
        }
        lines.push(format!("{id}\tsince={}", action.since));
    }
    lines.sort();
    let mut out = String::from(
        "# Generated from crates/pos-proto/src/actions/catalogue.rs — do not edit.\n\
         # One line per fact. A bare action id, its effect and its params are contracts; its\n\
         # frequency, route, permission, refusal and since lines may change. ADR-0191 decision 4.\n",
    );
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}
