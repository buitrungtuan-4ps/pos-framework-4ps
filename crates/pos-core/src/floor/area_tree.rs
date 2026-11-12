// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The tree of areas ([ADR-0211](../../../../docs/adr/0211-an-area-may-sit-inside-another-area.md)).
//!
//! An area may sit inside another. The tree is stored as a parent pointer and published resolved:
//! the cloud orders the areas and computes each one's `depth` and `path`, and a reader never walks
//! the tree. Two concerns live here, both pure:
//!
//! - **The write rule** ([`area_change_refusal`]): what the cloud refuses before it stores a new or
//!   changed area. A parent that is not one of the store's areas, a parent that is archived while the
//!   area is live, a cycle, a branch deeper than [`MAX_AREA_LEVELS`], and archiving an area that still
//!   has live children.
//! - **The resolve** ([`resolve_area_tree`]): the published order (depth first, the top level and
//!   each area's children in id order) with each area's `depth` and `path` filled in. It is
//!   forgiving, like the rest of the compile: an area whose parent is not published, or that a
//!   broken store has put in a cycle or too deep, is published at the top rather than lost, so its
//!   tables stay reachable.

use std::collections::{BTreeMap, BTreeSet};

use pos_proto::floor::{FloorArea, MAX_AREA_LEVELS};
use pos_proto::ids::AreaId;

/// One area as the tree rule reads it: its identity, its parent, and whether it is live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AreaLink {
    /// The area.
    pub area_id: AreaId,
    /// The area it sits inside, or `None` at the top.
    pub parent_area_id: Option<AreaId>,
    /// Whether it is active (not archived).
    pub active: bool,
}

/// Why the cloud refuses a new or changed area. Each has a stable token the console translates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AreaTreeRefusal {
    /// The parent is not one of this store's areas.
    ParentNotFound,
    /// The area is live and its parent is archived.
    ParentArchived,
    /// The parent is the area itself, or sits inside it.
    Cycle,
    /// The branch would be deeper than [`MAX_AREA_LEVELS`].
    TooDeep,
    /// The area is being archived while an area inside it is still live.
    HasLiveChildren,
}

impl AreaTreeRefusal {
    /// The stable `UPPER_SNAKE_CASE` reason the cloud answers with.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::ParentNotFound => "AREA_PARENT_NOT_FOUND",
            Self::ParentArchived => "AREA_PARENT_ARCHIVED",
            Self::Cycle => "AREA_CYCLE",
            Self::TooDeep => "AREA_TOO_DEEP",
            Self::HasLiveChildren => "AREA_HAS_LIVE_CHILDREN",
        }
    }
}

/// Why `change` may not be stored among a store's `areas` as they stand, or `None` when it may.
///
/// `areas` are the store's areas before the write, the changed area among them or not (a new one is
/// not). The rule is checked on the store as it would stand after the write.
#[must_use]
pub fn area_change_refusal(areas: &[AreaLink], change: &AreaLink) -> Option<AreaTreeRefusal> {
    let mut parents: BTreeMap<AreaId, Option<AreaId>> = BTreeMap::new();
    let mut active: BTreeSet<AreaId> = BTreeSet::new();
    for area in areas.iter().chain(core::iter::once(change)) {
        parents.insert(area.area_id, area.parent_area_id);
        if area.active {
            active.insert(area.area_id);
        } else {
            active.remove(&area.area_id);
        }
    }
    if !change.active
        && areas.iter().any(|area| {
            area.active
                && area.area_id != change.area_id
                && area.parent_area_id == Some(change.area_id)
        })
    {
        return Some(AreaTreeRefusal::HasLiveChildren);
    }
    let Some(parent) = change.parent_area_id else {
        return levels_refusal(1, change.area_id, &parents);
    };
    if parent == change.area_id {
        return Some(AreaTreeRefusal::Cycle);
    }
    if !parents.contains_key(&parent) {
        return Some(AreaTreeRefusal::ParentNotFound);
    }
    if change.active && !active.contains(&parent) {
        return Some(AreaTreeRefusal::ParentArchived);
    }
    // Walk up from the parent: meeting the area itself is a cycle. The walk is bounded by the
    // number of areas, so a store that is already broken cannot hold it forever.
    let mut level: usize = 1;
    let mut cursor = Some(parent);
    let mut seen = BTreeSet::new();
    while let Some(current) = cursor {
        if current == change.area_id {
            return Some(AreaTreeRefusal::Cycle);
        }
        if !seen.insert(current) {
            break;
        }
        level += 1;
        cursor = parents.get(&current).copied().flatten();
    }
    levels_refusal(level, change.area_id, &parents)
}

/// [`AreaTreeRefusal::TooDeep`] when an area at `level` (the top counting as one), with the branch
/// below it, holds more than [`MAX_AREA_LEVELS`] levels.
fn levels_refusal(
    level: usize,
    area_id: AreaId,
    parents: &BTreeMap<AreaId, Option<AreaId>>,
) -> Option<AreaTreeRefusal> {
    let mut children: BTreeMap<AreaId, Vec<AreaId>> = BTreeMap::new();
    for (child, parent) in parents {
        if let Some(parent) = parent {
            children.entry(*parent).or_default().push(*child);
        }
    }
    // Breadth first below the area, one level at a time, never visiting an area twice.
    let mut deepest = level;
    let mut frontier = vec![area_id];
    let mut seen = BTreeSet::from([area_id]);
    while !frontier.is_empty() {
        let next: Vec<AreaId> = frontier
            .iter()
            .filter_map(|area| children.get(area))
            .flatten()
            .copied()
            .filter(|child| seen.insert(*child))
            .collect();
        if next.is_empty() {
            break;
        }
        deepest += 1;
        frontier = next;
    }
    (deepest > usize::from(MAX_AREA_LEVELS)).then_some(AreaTreeRefusal::TooDeep)
}

/// The published order of `areas`, each with its `depth` and `path` computed from its
/// `parent_area_id`.
///
/// Depth first: the top-level areas in id order, each followed by the areas inside it, also in id
/// order. An area whose parent is not among `areas`, or that sits in a cycle, or that would sit
/// deeper than [`MAX_AREA_LEVELS`] allows, is published at the top with no parent. A plan whose
/// areas are all at the top comes back in id order and otherwise unchanged.
#[must_use]
pub fn resolve_area_tree(areas: Vec<FloorArea>) -> Vec<FloorArea> {
    let known: BTreeSet<AreaId> = areas.iter().map(|area| area.area_id).collect();
    let mut by_id: BTreeMap<AreaId, FloorArea> = BTreeMap::new();
    let mut children: BTreeMap<AreaId, Vec<AreaId>> = BTreeMap::new();
    for area in areas {
        if let Some(parent) = area.parent_area_id.filter(|parent| known.contains(parent)) {
            children.entry(parent).or_default().push(area.area_id);
        }
        by_id.insert(area.area_id, area);
    }
    // Ulid order is id order, the order the plan was published in before areas nested.
    for siblings in children.values_mut() {
        siblings.sort_unstable();
    }
    let max_depth = usize::from(MAX_AREA_LEVELS.saturating_sub(1));
    let mut ordered: Vec<FloorArea> = Vec::with_capacity(by_id.len());
    loop {
        // The next root: the lowest-id area left whose parent is not left (it is at the top, its
        // parent is unpublished, or its parent was cut off for depth), or failing that (a cycle)
        // the lowest-id area left. Either is published at the top.
        let root = by_id
            .values()
            .find(|area| {
                area.parent_area_id
                    .is_none_or(|parent| !by_id.contains_key(&parent))
            })
            .or_else(|| by_id.values().next())
            .map(|area| area.area_id);
        let Some(root) = root else { break };
        let mut stack: Vec<(AreaId, Vec<AreaId>)> = vec![(root, Vec::new())];
        while let Some((area_id, path)) = stack.pop() {
            let Some(mut area) = by_id.remove(&area_id) else {
                continue;
            };
            if path.is_empty() {
                area.parent_area_id = None;
            }
            area.depth = u8::try_from(path.len()).unwrap_or(u8::MAX);
            if path.len() < max_depth
                && let Some(inside) = children.get(&area_id)
            {
                let mut below = path.clone();
                below.push(area_id);
                for child in inside.iter().rev() {
                    if by_id.contains_key(child) {
                        stack.push((*child, below.clone()));
                    }
                }
            }
            area.path = path;
            ordered.push(area);
        }
    }
    ordered
}

/// The tree problems in a published list of areas, in reading order: each area's parent comes
/// before it, and its `depth` and `path` are its parent's plus one level. Empty when the list is
/// one the cloud resolved.
#[must_use]
pub fn area_tree_violations(areas: &[FloorArea]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut paths: BTreeMap<AreaId, &[AreaId]> = BTreeMap::new();
    for area in areas {
        let expected: Option<Vec<AreaId>> = match area.parent_area_id {
            None => Some(Vec::new()),
            Some(parent) => paths.get(&parent).map(|above| {
                let mut path = above.to_vec();
                path.push(parent);
                path
            }),
        };
        match expected {
            None => violations.push(format!(
                "area {} names a parent that does not come before it",
                area.area_id
            )),
            Some(path) if path != area.path || usize::from(area.depth) != path.len() => {
                violations.push(format!(
                    "area {} carries a depth or path that does not match its parent",
                    area.area_id
                ));
            }
            Some(_) => {}
        }
        if area.depth >= MAX_AREA_LEVELS {
            violations.push(format!(
                "area {} sits deeper than {MAX_AREA_LEVELS} levels",
                area.area_id
            ));
        }
        paths.insert(area.area_id, &area.path);
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::{
        AreaLink, AreaTreeRefusal, area_change_refusal, area_tree_violations, resolve_area_tree,
    };
    use pos_proto::floor::{FloorArea, MAX_AREA_LEVELS};
    use pos_proto::ids::AreaId;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;

    fn id(n: u128) -> AreaId {
        AreaId::new(Ulid::from_u128(n))
    }
    fn link(n: u128, parent: Option<u128>) -> AreaLink {
        AreaLink {
            area_id: id(n),
            parent_area_id: parent.map(id),
            active: true,
        }
    }
    fn area(n: u128, parent: Option<u128>) -> FloorArea {
        FloorArea {
            parent_area_id: parent.map(id),
            ..FloorArea::top(id(n), DisplayName::new(format!("A{n}")), Vec::new())
        }
    }
    /// A chain 1 ⊃ 2 ⊃ … ⊃ n.
    fn chain(n: u128) -> Vec<AreaLink> {
        (1..=n).map(|k| link(k, (k > 1).then(|| k - 1))).collect()
    }

    #[test]
    fn a_top_level_area_and_a_child_of_a_live_area_are_stored() {
        let store = [link(1, None), link(2, None)];
        assert_eq!(area_change_refusal(&store, &link(3, None)), None);
        assert_eq!(area_change_refusal(&store, &link(3, Some(1))), None);
        assert_eq!(area_change_refusal(&store, &link(2, Some(1))), None);
    }

    #[test]
    fn a_parent_that_is_not_the_stores_or_is_archived_is_refused() {
        let mut archived = link(1, None);
        archived.active = false;
        assert_eq!(
            area_change_refusal(&[link(1, None)], &link(3, Some(9))),
            Some(AreaTreeRefusal::ParentNotFound)
        );
        assert_eq!(
            area_change_refusal(&[archived], &link(3, Some(1))),
            Some(AreaTreeRefusal::ParentArchived)
        );
        // An archived area may sit inside an archived one.
        let mut child = link(3, Some(1));
        child.active = false;
        assert_eq!(area_change_refusal(&[archived], &child), None);
    }

    #[test]
    fn a_cycle_is_refused() {
        let store = chain(3);
        assert_eq!(
            area_change_refusal(&store, &link(1, Some(1))),
            Some(AreaTreeRefusal::Cycle)
        );
        assert_eq!(
            area_change_refusal(&store, &link(1, Some(3))),
            Some(AreaTreeRefusal::Cycle)
        );
        // Moving the middle under a sibling branch is fine.
        let mut store = chain(3);
        store.push(link(4, None));
        assert_eq!(area_change_refusal(&store, &link(2, Some(4))), None);
    }

    #[test]
    fn eight_levels_are_stored_and_a_ninth_is_refused() {
        let levels = u128::from(MAX_AREA_LEVELS);
        assert_eq!(
            area_change_refusal(&chain(levels - 1), &link(levels, Some(levels - 1))),
            None
        );
        let store = chain(levels);
        assert_eq!(
            area_change_refusal(&store, &link(levels + 1, Some(levels))),
            Some(AreaTreeRefusal::TooDeep)
        );
    }

    #[test]
    fn moving_a_branch_counts_the_levels_below_it() {
        // 1 ⊃ 2 ⊃ … ⊃ 5, and a separate 10 ⊃ 11 ⊃ 12 ⊃ 13.
        let mut store = chain(5);
        store.extend([
            link(10, None),
            link(11, Some(10)),
            link(12, Some(11)),
            link(13, Some(12)),
        ]);
        // Moving 10 under 4 makes 4 + 4 = 8 levels: stored.
        assert_eq!(area_change_refusal(&store, &link(10, Some(4))), None);
        // Under 5 it would be 9.
        assert_eq!(
            area_change_refusal(&store, &link(10, Some(5))),
            Some(AreaTreeRefusal::TooDeep)
        );
    }

    #[test]
    fn archiving_an_area_with_live_children_is_refused_until_they_move() {
        let store = [link(1, None), link(2, Some(1))];
        let mut archive = link(1, None);
        archive.active = false;
        assert_eq!(
            area_change_refusal(&store, &archive),
            Some(AreaTreeRefusal::HasLiveChildren)
        );
        let moved = [link(1, None), link(2, None)];
        assert_eq!(area_change_refusal(&moved, &archive), None);
        let mut child_archived = link(2, Some(1));
        child_archived.active = false;
        assert_eq!(
            area_change_refusal(&[link(1, None), child_archived], &archive),
            None
        );
    }

    #[test]
    fn every_refusal_has_a_distinct_token() {
        let tokens: std::collections::BTreeSet<&str> = [
            AreaTreeRefusal::ParentNotFound,
            AreaTreeRefusal::ParentArchived,
            AreaTreeRefusal::Cycle,
            AreaTreeRefusal::TooDeep,
            AreaTreeRefusal::HasLiveChildren,
        ]
        .into_iter()
        .map(AreaTreeRefusal::reason)
        .collect();
        assert_eq!(tokens.len(), 5);
    }

    #[test]
    fn a_flat_plan_resolves_to_id_order_unchanged() {
        let flat = vec![area(3, None), area(1, None), area(2, None)];
        let resolved = resolve_area_tree(flat);
        let ids: Vec<AreaId> = resolved.iter().map(|area| area.area_id).collect();
        assert_eq!(ids, vec![id(1), id(2), id(3)]);
        assert!(
            resolved
                .iter()
                .all(|area| area.depth == 0 && area.path.is_empty())
        );
        assert!(area_tree_violations(&resolved).is_empty());
    }

    #[test]
    fn a_tree_resolves_depth_first_with_depth_and_path() {
        // 1 ⊃ {4 ⊃ 5, 2}, 3.
        let tree = vec![
            area(5, Some(4)),
            area(3, None),
            area(2, Some(1)),
            area(4, Some(1)),
            area(1, None),
        ];
        let resolved = resolve_area_tree(tree);
        let order: Vec<(AreaId, u8, Vec<AreaId>)> = resolved
            .iter()
            .map(|area| (area.area_id, area.depth, area.path.clone()))
            .collect();
        assert_eq!(
            order,
            vec![
                (id(1), 0, vec![]),
                (id(2), 1, vec![id(1)]),
                (id(4), 1, vec![id(1)]),
                (id(5), 2, vec![id(1), id(4)]),
                (id(3), 0, vec![]),
            ]
        );
        assert!(area_tree_violations(&resolved).is_empty());
    }

    #[test]
    fn a_broken_store_is_published_whole_with_the_breaks_at_the_top() {
        // 1 and 2 name each other; 3 names an area that is not published.
        let resolved =
            resolve_area_tree(vec![area(1, Some(2)), area(2, Some(1)), area(3, Some(9))]);
        assert_eq!(resolved.len(), 3);
        assert!(area_tree_violations(&resolved).is_empty());
        let tops = resolved
            .iter()
            .filter(|area| area.parent_area_id.is_none())
            .count();
        assert_eq!(tops, 2, "one of the cycle and the orphan");
    }

    #[test]
    fn a_branch_deeper_than_the_limit_is_cut_and_its_rest_published_at_the_top() {
        let deep: Vec<FloorArea> = (1..=10).map(|k| area(k, (k > 1).then(|| k - 1))).collect();
        let resolved = resolve_area_tree(deep);
        assert_eq!(resolved.len(), 10);
        assert!(area_tree_violations(&resolved).is_empty());
        assert!(resolved.iter().all(|area| area.depth < MAX_AREA_LEVELS));
    }

    #[test]
    fn a_published_list_out_of_order_or_with_a_wrong_path_is_a_violation() {
        let mut child = area(2, Some(1));
        child.depth = 1;
        child.path = vec![id(1)];
        assert_eq!(
            area_tree_violations(&[child.clone(), area(1, None)]).len(),
            1
        );
        child.path = Vec::new();
        assert_eq!(area_tree_violations(&[area(1, None), child]).len(), 1);
    }
}
