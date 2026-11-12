// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! How many taps each act a version offers costs from a role's home, and which go over their
//! budget ([ADR-0210](../../../../docs/adr/0210-a-role-names-its-home-screen-in-the-package.md)
//! decisions 2 to 5). An act over its budget is a **warning**: the version still publishes.
//!
//! For each role a selector of the version states, and the base ("any other role"), and for each
//! device class, every screen and the home are resolved as a till of that class would resolve
//! them. A screen is a node of a graph. A node property of kind `ACTION` whose act is
//! `ui.screen.open` with a fixed `screen_id` is an edge of one tap; any other action property
//! offers its act at the screen's distance plus one. A flow is entered by its start act: a screen
//! one of its steps shows is reached at what the start act costs. A screen the version does not
//! declare is the release's own and ends the graph. An act no screen offers is not offered to the
//! role, and is not warned. Permissions play no part: an act a role may not perform still counts.

use std::collections::{BTreeMap, BTreeSet};

use pos_proto::actions::{Action, ActionFrequency};
use pos_proto::devices::DeviceClass;
use pos_proto::package::{PackageContent, Variant, VariantSelector};
use pos_proto::screen::{ParamValue, PropertyValue, Screen, ScreenNode};
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::{Open, WireEnum};

use super::{VariantContext, resolve};

/// The most taps a common act may cost from the home.
pub const COMMON_TAPS_MAX: u32 = 2;

/// The most taps a rare act may cost from the home.
pub const RARE_TAPS_MAX: u32 = 3;

/// The act that opens a screen: with a fixed `screen_id`, an edge of the graph.
const OPEN_SCREEN: &str = "ui.screen.open";

/// One act that costs more taps than its budget, for one role on one device class, at the node
/// that offers it most cheaply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TapWarning {
    /// The role, or `None` for the base: every role without a variant of its own.
    pub role_id: Option<Ulid>,
    /// The device class.
    pub device_class: DeviceClass,
    /// The act.
    pub action_id: String,
    /// The screen variant offering it most cheaply, by its position in the version's `screens`.
    pub screen_index: usize,
    /// The node offering it, by its position in that screen's tree in pre-order, the root being 0.
    pub node_index: usize,
    /// What it costs from the home, in taps.
    pub taps: u32,
}

impl TapWarning {
    /// The stable reason a dry run names it with.
    pub const REASON: &'static str = "TAP_BUDGET";
}

/// Where an act is cheapest: its cost, and the screen variant and node offering it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Offer {
    taps: u32,
    screen_index: usize,
    node_index: usize,
}

/// Every act over its budget, by role (the base first, then each role in id order), then device
/// class, then act id. None for a version that states no homes: there is no start to count from.
#[must_use]
pub fn tap_warnings(content: &PackageContent) -> Vec<TapWarning> {
    if content.home_screens.is_empty() {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    for role_id in roles(content) {
        for class in DeviceClass::ALL
            .iter()
            .copied()
            .filter(|class| *class != DeviceClass::UNSPECIFIED)
        {
            let resolving = VariantContext {
                device_class: Open::from_known(class),
                role_ids: role_id.into_iter().collect(),
                ..VariantContext::default()
            };
            for (action_id, offer) in offers(content, &resolving) {
                if over_budget(&action_id, offer.taps) {
                    warnings.push(TapWarning {
                        role_id,
                        device_class: class,
                        action_id,
                        screen_index: offer.screen_index,
                        node_index: offer.node_index,
                        taps: offer.taps,
                    });
                }
            }
        }
    }
    warnings
}

/// The base (`None`), then every role a selector of the version states, in id order.
fn roles(content: &PackageContent) -> Vec<Option<Ulid>> {
    let selectors = content
        .home_screens
        .iter()
        .map(|variant| &variant.selector)
        .chain(content.screens.iter().map(|variant| &variant.selector))
        .chain(content.flows.iter().map(|variant| &variant.selector));
    let stated: BTreeSet<Ulid> = selectors
        .filter_map(|selector: &VariantSelector| selector.role_id)
        .collect();
    core::iter::once(None)
        .chain(stated.into_iter().map(Some))
        .collect()
}

/// Whether `taps` is over the budget of the act's frequency. An act the catalogue lacks has none.
fn over_budget(action_id: &str, taps: u32) -> bool {
    match Action::find(action_id).map(|act| act.frequency) {
        Some(ActionFrequency::Common) => taps > COMMON_TAPS_MAX,
        Some(ActionFrequency::Rare) => taps > RARE_TAPS_MAX,
        _ => false,
    }
}

/// The variant of each declared screen `context` resolves, by `screen_id`, with its position in
/// `screens`.
fn resolved_screens<'a>(
    screens: &'a [Variant<Screen>],
    context: &VariantContext,
) -> BTreeMap<&'a str, (usize, &'a Screen)> {
    let ids: BTreeSet<&str> = screens
        .iter()
        .map(|variant| variant.body.screen_id.as_str())
        .collect();
    ids.into_iter()
        .filter_map(|id| {
            let own: Vec<(usize, &Variant<Screen>)> = screens
                .iter()
                .enumerate()
                .filter(|(_, variant)| variant.body.screen_id == id)
                .collect();
            let candidates: Vec<Variant<&Screen>> = own
                .iter()
                .map(|(_, variant)| Variant {
                    selector: variant.selector.clone(),
                    body: &variant.body,
                })
                .collect();
            let chosen = *resolve(&candidates, context)?;
            own.iter()
                .find(|(_, variant)| core::ptr::eq(core::ptr::from_ref(&variant.body), chosen))
                .map(|(index, _)| (id, (*index, chosen)))
        })
        .collect()
}

/// Every node of `root` in pre-order, with its position.
fn preorder(root: &ScreenNode) -> Vec<(usize, &ScreenNode)> {
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        found.push((found.len(), node));
        pending.extend(node.children.iter().rev());
    }
    found
}

/// The screen a binding opens, when it is `ui.screen.open` with a fixed `screen_id`.
fn opens(action_id: &str, params: &BTreeMap<String, ParamValue>) -> Option<String> {
    if action_id != OPEN_SCREEN {
        return None;
    }
    match params.get("screen_id") {
        Some(ParamValue::Text(screen_id)) => Some(screen_id.clone()),
        _ => None,
    }
}

/// Every act the screens reachable from `resolving`'s home offer, at its cheapest.
fn offers(content: &PackageContent, resolving: &VariantContext) -> BTreeMap<String, Offer> {
    let mut offered: BTreeMap<String, Offer> = BTreeMap::new();
    let Some(home) = resolve(&content.home_screens, resolving) else {
        return offered;
    };
    let screens = resolved_screens(&content.screens, resolving);
    let flows: Vec<_> = content
        .flows
        .iter()
        .map(|variant| variant.body.flow_id.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|flow_id| {
            let own: Vec<_> = content
                .flows
                .iter()
                .filter(|variant| variant.body.flow_id == flow_id)
                .cloned()
                .collect();
            resolve(&own, resolving).cloned()
        })
        .collect();
    let mut distance: BTreeMap<&str, u32> = BTreeMap::new();
    if screens.contains_key(home.screen_id.as_str()) {
        distance.insert(home.screen_id.as_str(), 0);
    }
    // Distances only fall and are bounded below, so this settles; each pass is linear in the
    // version's nodes and a version holds at most 64 screens.
    loop {
        let mut changed = false;
        let reached: Vec<(&str, u32)> = distance.iter().map(|(id, taps)| (*id, *taps)).collect();
        for (screen_id, taps) in reached {
            let Some((screen_index, screen)) = screens.get(screen_id) else {
                continue;
            };
            for (node_index, node) in preorder(&screen.root) {
                for value in node.properties.values() {
                    let PropertyValue::Action(binding) = value else {
                        continue;
                    };
                    if let Some(target) = opens(&binding.action_id, &binding.params) {
                        if let Some((target_id, _)) = screens.get_key_value(target.as_str()) {
                            changed |= lower(&mut distance, target_id, taps + 1);
                        }
                        continue;
                    }
                    let offer = Offer {
                        taps: taps + 1,
                        screen_index: *screen_index,
                        node_index,
                    };
                    changed |= cheaper(&mut offered, &binding.action_id, offer);
                }
            }
        }
        for flow in &flows {
            let Some(start) = offered.get(&flow.start_action_id).map(|offer| offer.taps) else {
                continue;
            };
            for step in &flow.steps {
                if let Some(screen_id) = &step.screen_id
                    && let Some((target_id, _)) = screens.get_key_value(screen_id.as_str())
                {
                    changed |= lower(&mut distance, target_id, start);
                }
            }
        }
        if !changed {
            return offered;
        }
    }
}

/// Sets `screen_id`'s distance to `taps` if that is lower; whether it was.
fn lower<'a>(distance: &mut BTreeMap<&'a str, u32>, screen_id: &'a str, taps: u32) -> bool {
    match distance.get(screen_id) {
        Some(known) if *known <= taps => false,
        _ => {
            distance.insert(screen_id, taps);
            true
        }
    }
}

/// Keeps `offer` for `action_id` if it is the cheapest yet (the first found on a tie); whether it
/// was kept.
fn cheaper(acts: &mut BTreeMap<String, Offer>, action_id: &str, offer: Offer) -> bool {
    match acts.get(action_id) {
        Some(known) if known.taps <= offer.taps => false,
        _ => {
            acts.insert(action_id.to_owned(), offer);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::devices::DeviceClass;
    use pos_proto::package::PackageContent;

    use super::{TapWarning, tap_warnings};

    /// A content whose screens, homes and flows are the given JSON arrays.
    fn content(screens: &str, homes: &str, flows: &str) -> PackageContent {
        serde_json::from_str(&format!(
            r#"{{"package_id":"01J00000000000000000000P01","package_version_id":"01J00000000000000000000V01",
                "version_label":"March","min_edge_version":"0.14.1",
                "screens":{screens},"home_screens":{homes},"flows":{flows}}}"#
        ))
        .expect("content parses")
    }

    /// A screen holding the buttons given, each `(act, opened screen or "")`.
    fn screen(screen_id: &str, buttons: &[(&str, &str)]) -> String {
        let children: Vec<String> = buttons
            .iter()
            .map(|(act, opened)| {
                let params = if opened.is_empty() {
                    String::from("{}")
                } else {
                    format!(r#"{{"screen_id":"{opened}"}}"#)
                };
                format!(
                    r#"{{"component_id":"action_button","properties":{{"label_key":"x.y","action":{{"action_id":"{act}","params":{params}}}}}}}"#
                )
            })
            .collect();
        format!(
            r#"{{"body":{{"screen_id":"{screen_id}","root":{{"component_id":"stack","children":[{}]}}}}}}"#,
            children.join(",")
        )
    }

    const HOME: &str = r#"[{"body":{"screen_id":"a"}}]"#;

    /// `a` opens `b` opens `c` opens `d`: an act on `d` costs four taps, on `c` three.
    fn chain(on_c: &str, on_d: &str) -> String {
        format!(
            "[{},{},{},{}]",
            screen("a", &[("ui.screen.open", "b")]),
            screen("b", &[("ui.screen.open", "c")]),
            screen("c", &[("ui.screen.open", "d"), (on_c, "")]),
            screen("d", &[(on_d, "")]),
        )
    }

    fn acts(warnings: &[TapWarning]) -> Vec<(&str, u32)> {
        let mut found: Vec<(&str, u32)> = warnings
            .iter()
            .filter(|warning| warning.device_class == DeviceClass::Terminal)
            .map(|warning| (warning.action_id.as_str(), warning.taps))
            .collect();
        found.dedup();
        found
    }

    #[test]
    fn a_version_without_homes_has_nothing_to_count() {
        let version = content(
            &chain("billing.check.print", "billing.check.print"),
            "[]",
            "[]",
        );
        assert!(tap_warnings(&version).is_empty());
    }

    #[test]
    fn a_common_act_is_warned_over_two_taps_and_a_rare_one_over_three() {
        // `billing.check.print` is common; `cash.movement.safe_drop` is rare.
        let version = content(
            &chain("billing.check.print", "cash.movement.safe_drop"),
            HOME,
            "[]",
        );
        let warnings = tap_warnings(&version);
        assert_eq!(
            acts(&warnings),
            vec![("billing.check.print", 3), ("cash.movement.safe_drop", 4)]
        );
        let print = warnings
            .iter()
            .find(|warning| warning.action_id == "billing.check.print")
            .expect("warned");
        assert_eq!(print.role_id, None, "the base");
        assert_eq!(
            (print.screen_index, print.node_index),
            (2, 2),
            "screen c, its second button"
        );
        let rare_in_budget = content(&chain("billing.check.print", "cash.movement.safe_drop")
            .replace(r#""screen_id":"a","root":{"component_id":"stack","children":["#,
                     r#""screen_id":"a","root":{"component_id":"stack","children":[{"component_id":"action_button","properties":{"label_key":"x.y","action":{"action_id":"ui.screen.open","params":{"screen_id":"c"}}}},"#), HOME, "[]");
        assert_eq!(
            acts(&tap_warnings(&rare_in_budget)),
            Vec::<(&str, u32)>::new(),
            "the shortcut makes c one tap, d two"
        );
    }

    #[test]
    fn a_screen_no_home_reaches_and_one_the_version_does_not_declare_count_nothing() {
        let version = content(
            &format!(
                "[{},{}]",
                screen("a", &[("ui.screen.open", "floor")]),
                screen("far", &[("billing.check.print", "")]),
            ),
            HOME,
            "[]",
        );
        assert!(tap_warnings(&version).is_empty());
    }

    #[test]
    fn a_flow_reaches_its_steps_screens_at_its_start_acts_cost() {
        // The start act is on `a` (one tap); its step shows `d`, so an act there costs two.
        let version = content(
            &format!(
                "[{},{}]",
                screen("a", &[("sales.table.seat", "")]),
                screen("d", &[("cash.movement.safe_drop", "")]),
            ),
            HOME,
            r#"[{"body":{"flow_id":"seat","start_action_id":"sales.table.seat",
                 "steps":[{"step_id":"s","screen_id":"d","kind":"STEP_KIND_OPTIONAL"}]}}]"#,
        );
        assert!(
            tap_warnings(&version).is_empty(),
            "two taps is within budget"
        );
    }

    #[test]
    fn each_role_a_version_states_is_counted_from_its_own_home() {
        let homes = r#"[{"body":{"screen_id":"a"}},
                        {"selector":{"role_id":"01J00000000000000000000R01"},"body":{"screen_id":"c"}}]"#;
        let version = content(
            &chain("billing.check.print", "billing.check.print"),
            homes,
            "[]",
        );
        let warnings = tap_warnings(&version);
        assert!(warnings.iter().any(|warning| warning.role_id.is_none()));
        assert!(
            warnings.iter().all(|warning| warning.role_id.is_none()),
            "from c, every act is within two taps"
        );
    }
}
