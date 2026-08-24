// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The two JSON operations the config tree is built from: **layer merge** (composing the four levels
//! into one effective document) and the **merge patch** (RFC 7386) that is the delta format
//! ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
//!
//! They are deliberately different operations. Layer merge composes *authored* documents where a
//! more-specific level overrides a less-specific one, so an explicit `null` at a level is a real
//! value a store should see. A merge patch is a *diff*, where `null` is the sentinel that means
//! "delete this key" — the one thing a plain overwrite cannot express. Keeping them separate is what
//! lets a delta remove a key without a layer accidentally doing the same.

use serde_json::{Map, Value};

/// Deep-merges an ordered list of config layers into one effective document, least-specific first.
///
/// Objects merge recursively; any non-object value (a scalar, an array, or `null`) from a
/// more-specific layer replaces what a less-specific layer had. This is the Tenant → Brand → Store →
/// Device composition: pass the four documents in that order and the last one wins each key.
#[must_use]
pub fn merge_layers(layers: &[&Value]) -> Value {
    let mut effective = Value::Object(Map::new());
    for layer in layers {
        merge_into(&mut effective, layer);
    }
    effective
}

/// Merges `overlay` onto `base` in place, objects recursively and everything else by replacement.
fn merge_into(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (key, overlay_value) in overlay_map {
                match base_map.get_mut(key) {
                    Some(base_value) => merge_into(base_value, overlay_value),
                    None => {
                        base_map.insert(key.clone(), overlay_value.clone());
                    }
                }
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

/// Applies an RFC 7386 merge patch to `target` in place.
///
/// In a patch, an object recurses, a `null` deletes the key, and any other value replaces. This is
/// exactly what a store does to a snapshot to reach the next version, so sender and receiver share
/// this one definition ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
pub fn apply_merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch_map) = patch else {
        // A non-object patch replaces the target wholesale (RFC 7386 §2).
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Some(target_map) = target.as_object_mut() else {
        return;
    };
    for (key, patch_value) in patch_map {
        if patch_value.is_null() {
            target_map.remove(key);
        } else if let Some(existing) = target_map.get_mut(key) {
            apply_merge_patch(existing, patch_value);
        } else {
            let mut fresh = Value::Null;
            apply_merge_patch(&mut fresh, patch_value);
            target_map.insert(key.clone(), fresh);
        }
    }
}

/// Computes the minimal RFC 7386 merge patch that turns `from` into `to`.
///
/// The invariant this file rests on: `apply_merge_patch(&mut from.clone(), &diff(from, to)) == *to`.
/// A key dropped in `to` becomes an explicit `null`; a key added becomes its value; a changed nested
/// object recurses; anything else is replaced.
#[must_use]
pub fn diff(from: &Value, to: &Value) -> Value {
    let (Value::Object(from_map), Value::Object(to_map)) = (from, to) else {
        // Not both objects: the whole value is the patch (unless equal, handled by the caller).
        return to.clone();
    };

    let mut patch = Map::new();
    // Deletions: keys in `from` that `to` no longer has.
    for key in from_map.keys() {
        if !to_map.contains_key(key) {
            patch.insert(key.clone(), Value::Null);
        }
    }
    // Additions and changes.
    for (key, to_value) in to_map {
        match from_map.get(key) {
            None => {
                patch.insert(key.clone(), to_value.clone());
            }
            Some(from_value) if from_value != to_value => {
                let nested = diff(from_value, to_value);
                patch.insert(key.clone(), nested);
            }
            Some(_) => {}
        }
    }
    Value::Object(patch)
}

#[cfg(test)]
mod tests {
    use super::{apply_merge_patch, diff, merge_layers};

    use serde_json::{Value, json};

    #[test]
    fn layers_override_from_least_to_most_specific() {
        let tenant = json!({"currency": "VND", "tips_enabled": false, "tax": {"rate": 8}});
        let brand = json!({"tips_enabled": true});
        let store = json!({"tax": {"rate": 10}});
        let device = json!({"printer": "kitchen-1"});

        let effective = merge_layers(&[&tenant, &brand, &store, &device]);
        assert_eq!(
            effective,
            json!({
                "currency": "VND",
                "tips_enabled": true,           // brand overrode tenant
                "tax": {"rate": 10},            // store overrode tenant, nested-merged
                "printer": "kitchen-1"          // device added
            })
        );
    }

    #[test]
    fn layer_merge_recurses_objects_but_replaces_scalars_and_arrays() {
        let base = json!({"a": {"x": 1, "y": 2}, "list": [1, 2, 3]});
        let over = json!({"a": {"y": 20, "z": 30}, "list": [9]});
        assert_eq!(
            merge_layers(&[&base, &over]),
            json!({"a": {"x": 1, "y": 20, "z": 30}, "list": [9]}),
            "nested objects merge; arrays replace wholesale"
        );
    }

    #[test]
    fn a_merge_patch_adds_changes_and_deletes() {
        let mut target = json!({"keep": 1, "change": "old", "drop": true});
        let patch = json!({"change": "new", "drop": null, "add": 5});
        apply_merge_patch(&mut target, &patch);
        assert_eq!(target, json!({"keep": 1, "change": "new", "add": 5}));
    }

    #[test]
    fn a_diff_then_apply_round_trips_for_any_pair() {
        let cases = [
            (json!({"a": 1}), json!({"a": 2})),
            (json!({"a": 1, "b": 2}), json!({"a": 1})),
            (json!({}), json!({"nested": {"deep": [1, 2]}})),
            (
                json!({"tax": {"rate": 8, "inclusive": true}}),
                json!({"tax": {"rate": 10}}),
            ),
            (json!({"same": 1}), json!({"same": 1})),
        ];
        for (from, to) in cases {
            let patch = diff(&from, &to);
            let mut applied = from.clone();
            apply_merge_patch(&mut applied, &patch);
            assert_eq!(
                applied, to,
                "diff({from}) -> {patch} did not reproduce {to}"
            );
        }
    }

    #[test]
    fn an_identical_pair_diffs_to_an_empty_patch() {
        let value = json!({"a": 1, "b": {"c": 2}});
        assert_eq!(diff(&value, &value), Value::Object(serde_json::Map::new()));
    }
}
