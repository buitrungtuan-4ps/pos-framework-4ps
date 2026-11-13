// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use std::collections::BTreeSet;

use super::{Action, ActionEffect, ActionParamKind, CATALOGUE, SNAPSHOT_PATH, render_snapshot};
use crate::wire_enum::WireEnum as _;

fn is_snake(word: &str) -> bool {
    !word.is_empty()
        && word.starts_with(|c: char| c.is_ascii_lowercase())
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_upper_snake(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// An action id has the permission taxonomy's shape, `<domain>.<resource>.<verb>`, and is listed
/// once (ADR-0191 decision 1).
#[test]
fn every_action_id_is_three_snake_case_words_and_unique() {
    let mut seen = BTreeSet::new();
    for action in CATALOGUE {
        let id = action.action_id;
        let words: Vec<&str> = id.split('.').collect();
        assert!(
            words.len() == 3 && words.iter().all(|word| is_snake(word)),
            "{id} is `<domain>.<resource>.<verb>` in snake_case"
        );
        assert!(seen.insert(id), "{id} is listed once");
        assert_eq!(Action::find(id).map(|found| found.action_id), Some(id));
    }
    assert!(Action::find("sales.line.teleport").is_none());
}

/// `ACTION_EFFECT_UNSPECIFIED` is refused.
#[test]
fn every_action_states_its_effect_and_its_frequency() {
    for action in CATALOGUE {
        let id = action.action_id;
        assert_ne!(action.effect, ActionEffect::Unspecified, "{id}");
        assert_ne!(
            action.frequency,
            super::ActionFrequency::Unspecified,
            "{id}"
        );
        assert!(!action.since.is_empty(), "{id} names its release");
        assert!(!action.summary.is_empty(), "{id} says what it does");
    }
}

/// A UI act calls no route and needs no permission; every other act calls at least one route and
/// names what a person needs for it.
#[test]
fn only_an_act_beyond_the_screen_calls_the_edge() {
    for action in CATALOGUE {
        let id = action.action_id;
        if action.effect == ActionEffect::Ui {
            assert!(action.routes.is_empty(), "{id} only changes the screen");
            assert!(action.permissions.is_empty(), "{id} needs no permission");
            assert!(action.refusals.is_empty(), "{id} cannot be refused");
        } else {
            assert!(!action.routes.is_empty(), "{id} names its route");
            assert!(!action.permissions.is_empty(), "{id} names its permission");
            for route in action.routes {
                assert!(
                    route.starts_with("POST /api/"),
                    "{id}: {route} is a state-changing edge route"
                );
            }
        }
    }
}

/// An amount belongs to a money act: an act that moves no money has nothing to bind one to.
#[test]
fn only_a_money_act_takes_an_amount() {
    for action in CATALOGUE {
        for param in action.params {
            assert!(
                is_snake(param.name),
                "{}: {} is snake_case",
                action.action_id,
                param.name
            );
            assert_ne!(param.kind, ActionParamKind::Unspecified);
            if param.kind == ActionParamKind::Amount {
                assert_eq!(
                    action.effect,
                    ActionEffect::Money,
                    "{} takes an amount, so it is a money act",
                    action.action_id
                );
            }
            let names_one = param.name.ends_with("_id");
            assert_eq!(
                names_one,
                param.kind == ActionParamKind::Id,
                "{}: an identifier, and only an identifier, is named `<resource>_id`",
                action.action_id
            );
            let names_many = param.name.ends_with("_ids");
            assert_eq!(
                names_many,
                param.kind == ActionParamKind::IdList,
                "{}: a list of identifiers, and only one, is named `<resource>_ids`",
                action.action_id
            );
        }
    }
}

/// Refusal tokens are the edge's `UPPER_SNAKE_CASE` reasons, and the permission check is not
/// repeated among them, since `permissions` already states it.
#[test]
fn every_refusal_is_a_token_and_none_is_the_permission() {
    for action in CATALOGUE {
        for refusal in action.refusals {
            assert!(is_upper_snake(refusal), "{}: {refusal}", action.action_id);
            assert_ne!(*refusal, "PERMISSION_DENIED", "{}", action.action_id);
        }
    }
}

#[test]
fn the_committed_snapshot_matches_the_catalogue() {
    let rendered = render_snapshot();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(SNAPSHOT_PATH);
    // Set POS_UPDATE_SNAPSHOTS=1 to rewrite; a check that silently fixes itself is not a check.
    if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, &rendered).expect("write the snapshot");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == rendered,
        "the action catalogue no longer matches {SNAPSHOT_PATH}. Regenerate it with:\n\
         \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot\n"
    );
}

/// The module both front ends build from is the action catalogue as rendered, so neither can drift from
/// the validator.
#[test]
fn the_tills_snapshot_of_the_catalogue_matches_it() {
    let rendered = super::render_typescript();
    let module = super::TYPESCRIPT_PATH;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(module);
    if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, &rendered).expect("write the module");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == rendered,
        "{module} no longer matches the action catalogue. Regenerate it with:\n\
         \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot\n"
    );
}

#[test]
fn every_action_appears_in_the_snapshot_with_its_effect() {
    let rendered = render_snapshot();
    for action in CATALOGUE {
        let line = format!("{}\teffect={}", action.action_id, action.effect.as_wire());
        assert!(rendered.lines().any(|l| l == line), "{line}");
    }
}

/// An act goes out through the cloud gateway only as the one lookup ADR-0214 accepted, and that act
/// calls the edge's gateway route alone: no act reaches the internet any other way, and a new
/// external act is a new ADR (ADR-0198, ADR-0214 decision 1).
#[test]
fn only_the_lookup_goes_out_through_the_gateway() {
    for action in CATALOGUE {
        let id = action.action_id;
        let external = action.effect == ActionEffect::External;
        assert_eq!(external, id == "integrations.lookup.run", "{id}");
        assert_eq!(
            external,
            action.routes.contains(&"POST /api/gateway/calls"),
            "{id}"
        );
        if external {
            assert_eq!(action.routes, &["POST /api/gateway/calls"], "{id}");
        }
    }
}
