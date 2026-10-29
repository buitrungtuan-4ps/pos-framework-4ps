# ADR-0175 — The crate list follows what a move can reach

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-15
· Supersedes the crate list in [ADR-0170](0170-repository-layout-code-naming-and-size-rules.md)
decision 4, on three points · Relates to [ADR-0002](0002-one-binary-per-tier.md)

Approved by the orchestrator under the owner's standing approval of 2026-10-13 and 2026-10-14
([plan](../plans/standardisation.md), Decisions taken): the work self-approves what best practice
settles, and the orchestrator takes decision gate 2 on its measurements. Gate 2 was taken on
2026-10-15: continue, area by area.

## Context

ADR-0170 decision 4 names 14 crates: nine cloud crates, among them `pos-cloud-catalog`, `-fees`,
`-integrations` and `-admin`, and five edge crates, among them `pos-edge-app`, `-billing`, `-shift`
and `-floor`. Phase 6 extracts them by moving code only (decision 6). The boundary survey before the
first crates, and gate 2's [figures](../metrics/crates-2026-10.md), found three points where the
list asks for something a move cannot do:

- **Edge billing, shift and floor are methods of `Edge`.**
  - Their commands are `impl<S: EventStore> Edge<S>` blocks: `app/bills` (10 files, 2,949 lines),
    `app/shifts` (9 files, 1,661 lines) and `app/floor.rs` (343 lines). There are 29 such blocks
    across `app/`.
  - Rust allows an inherent impl only in the crate that defines the type (E0116). So these
    commands live wherever `Edge` lives, and the planned `pos-edge-app` holds `Edge`.
  - What could move further is their HTTP handlers: 1,078, 794 and 537 lines, using the edge
    router's helpers and each other.
- **Catalog and fees use each other.**
  - The menu publish republishes fees (`FeeRuleStore`, `FeePublishResult`, `TenantFeeRules`).
  - A fee rule is checked and compiled against the catalog (`CatalogStore`, `CatalogItem`,
    `ItemCategoryId`).
  - Two crates cannot depend on each other, so splitting them needs a new seam. A seam is not a
    move.
- **Integrations and admin take the whole application state.**
  - Their handlers take `State<CloudApp<…>>`: 21 in `http/admin/` and 4 in the integrations'
    webhook routes.
  - `CloudApp` composes every store the binary runs, so it cannot leave `pos-cloud`.
  - A crate would need a sub-state built by axum's `FromRef`, as `pos-cloud-reports` has with
    `ReportsState`. That changes every handler's signature, so it is not a move either.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Keep the list and add the seams, extension traits and sub-states it needs | Behaviour-bearing refactors inside move PRs; an extension trait per edge area changes every call site of `Edge` |
| B | Drop the areas that cannot move | Leaves large areas in the binaries without a decision on what would let them move |
| C | **Change the list where a move cannot reach it, and say what each remaining area needs first** | The list no longer matches ADR-0170 word for word |

## Decision

Option **C**. The rest of ADR-0170 decision 4 stands, and so does every other decision in it.

1. **Edge billing, shift and floor stay inside `pos-edge-app`.**
   - Their commands remain `impl Edge` blocks in `app/`, in the same crate as `Edge`.
   - `pos-edge-billing`, `pos-edge-shift` and `pos-edge-floor` are not created.
   - Their HTTP handlers may move into crates of their own later, only if a measurement shows
     that it is worth doing. That would be a later pull request with its own figures, not part of
     this list.
2. **Catalog and fees are one crate, `pos-cloud-catalog`.** `pos-cloud-fees` is not created. Since
   the two reference each other, one crate is the only form a move can take. Splitting them later
   needs its own record, with the seam it introduces.
3. **Integrations and admin become crates only after a written sub-state design.**
   - Until then they stay in `pos-cloud`.
   - The design states the `FromRef` sub-state each needs, and which store fields it carries.
   - For integrations, it also says where `webhook/` (3,259 lines) goes.
   - For admin, the design weighs folding it into `pos-cloud-api` instead. That crate already holds
     console authentication, sessions and API keys, which most of admin's handlers serve.

The resulting list:
- **Cloud:** `pos-cloud-catalog`, `-config`, `-people`, `-devices`, `-floor` and `-inventory` as
  crates, with `-integrations` and `-admin` once their design is written. All sit on
  `pos-cloud-api` and `pos-cloud-reports`.
- **Edge:** `pos-edge-app` and `pos-edge-printing`.

## Consequences accepted

- `pos-edge-app` is larger than ADR-0170 implied: about 18,000 lines with its billing, shift and
  floor commands. Changing a bill still means building that crate.
- `pos-cloud-catalog` holds about 13,800 lines with fees inside, more than any other area crate.
- `CloudApp` keeps the admin and integration routes in `pos-cloud` for now, so `pos-cloud` stays
  larger than the list once promised.
- Phase 6's order follows from this record: `pos-edge-app`, then `pos-edge-printing` on it (taking
  about 45 lines of `config.rs` with it), then `pos-cloud-catalog`, `-floor` and `-people`.
  Integrations and admin follow once their sub-state design is written.
