# Console designers at the end of phase 4, November 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-11-12

The exit record of roadmap-v4 phase 4 (console designers): what the exit criterion asked, what shows
it is met, and what the phase's scope still leaves open. Like
[custom-data-2026-11-phase-3-exit.md](custom-data-2026-11-phase-3-exit.md), it records one moment
and is not regenerated.

**Measured at** `ca7182b` on `main`.

## The exit criterion is met

The criterion: HQ builds and publishes a package without writing JSON or CEL by hand, and the
preview matches the till.

### Built and published without JSON or CEL

| Part of a package | Where HQ builds it | Pull requests |
|---|---|---|
| A version as a draft, checked by a dry run before publish, every refusal shown where it is | Package designer | #1221 |
| Screens per device class, by drag and drop from the component catalogue | Screen designer | #1227 |
| Flows: steps, prerequisites (no edit can make a cycle), applies-when and done-when | Flow designer | #1228, #1229 |
| Conditions written as CEL from choices and read back exactly; hand-written CEL only replaceable | Rule builder | #1229 |
| Reactions: when, if, then; an act that moves money is never offered | Reactions panel | #1229 |
| Fields, lists and object types; "personal data?" has no default, and a personal field needs a retention period and a consent purpose | Data designer | #1229 |
| Receipt header, footer and sections as structure; a section a country requires always prints | Receipt template editor | #1236 |
| The home screen after sign-in ([ADR-0210](../adr/0210-a-role-names-its-home-screen-in-the-package.md)) | Screen designer | #1256 |
| Two versions compared part by part, with their tap-budget warnings, before pinning or rolling back | Version comparison | #1226, #1257 |

Publishing checks every label for English text (#1232) and counts taps from each home, warning, never
refusing, on an act over its budget (#1257, ADR-0210). The console replay publishes through these
screens against a real cloud and PostgreSQL, and no test types JSON or CEL.

### The preview matches the till

[ADR-0209](../adr/0209-the-console-previews-a-screen-with-the-tills-renderer.md) option 4: the
till's renderer core moved into `packages/package-renderer/` (#1252), and the screen designer previews
through it (#1253). One committed fixture package, the default package and a designed screen, is drawn
by the till's real component table and by the console's preview. Both must equal
`packages/package-renderer/fixtures/preview-shape.txt`, so either front end's test fails if the two
drift. Regions are drawn in the preview as outlines that name their component.

## The till draws what HQ publishes

| What | Pull requests |
|---|---|
| A pinned version's screens, per device class | #1231 |
| A screen the version adds, at `/screen/:id` | #1243 |
| Package buttons run the acts reactions run (open a screen, print a check, print an X report), never money | #1244 |
| The device class each terminal names, set in the console | #1245, #1246 |
| The home screen a version names, after sign-in | #1256 |

Every till change passed the full replay, 230 of 230 normally and on two CPUs, and the perf gate at
2× ([ADR-0208](../adr/0208-a-tap-is-measured-to-its-frame-ready-and-gated-per-pull-request.md)).

## The floor plan

Tables are placed by drag and drop (#1234). Areas nest
([ADR-0211](../adr/0211-an-area-may-sit-inside-another-area.md)): the cloud refuses a cycle or a branch
deeper than eight levels and publishes the tree resolved (#1250); the console shows the tree, moves a
branch and warns past three levels (#1251); the till shows the top-level areas as tabs and the levels
below as breadcrumbs, while a store with no nesting draws exactly what it drew before (#1254).

## What the phase's scope still leaves open

- **Roles on a store.** The published staff carry no role ids, so a role's home screen and a role's
  screen variant both resolve to the base on a store. An additive `role_ids` on the published staff
  member needs its own ADR.
- **Reports by area.** No report groups by area or by table today, so there is nothing to roll up
  along the area path. Area reporting is new work with its own ADR: an order names its table, not its
  area.
- **A preview drawn with an older till release's renderer** (ADR-0209 option 5), for when stores run
  several releases at once.
- **A store's own people choosing their package version** still waits on store-scoped console
  sign-in (roadmap v4, "where roadmap v3's open items went").
- **Two replay races found on the way.** The Pay-screen helper's one-instant decision is fixed (#1255).
  A sign-out race, where a read answered after sign-out could write the store's snapshot back, is
  being fixed separately.
