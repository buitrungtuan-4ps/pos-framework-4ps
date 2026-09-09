# ADR-0121 — One way to author an entity: a shell and a lifecycle, not a form framework

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-09
**Relates to** [ADR-0082](0082-catalog-and-layout-rebuild.md) (the F2 kit this extends: `DataTable`, `Modal`, `Drawer`, `ConfirmDialog`, `FormField`) · [ADR-0094](0094-console-optimistic-concurrency.md) (the `If-Match` a save carries, which is why a refused save must keep its form open) · [ADR-0120](0120-navigation-preserves-the-working-context.md) (U0, the sibling defect in the same report) · [`docs/cloud-admin-ux-plan.md`](../cloud-admin-ux-plan.md) §3w.6 (Track U) and its Stage 4/5 correction · [`docs/ui-ux.md`](../ui-ux.md) (the design principles this must not contradict)

**Context.** The owner's report from the deployed console named two things. One was a functional defect (ADR-0120). The other was that adding a record *"hiện nguyên cái card bự ra"* — shows a whole big card — and that the console's screens do not agree with each other about how anything is authored.

Measured against the tree on the day of the report:

| | |
|---|---|
| create/edit inside an inline `<Card>` | **~100** |
| `<Drawer>` | 31 |
| `<Modal>` | 7 |
| `FormField` adopted | **11 of 40** files |
| raw `<select>` (the kit has no `SelectField`) | **53**, across 22 files |
| raw `<input>` (bypassing `TextField`) | 52 |
| `createSignal` in `screens/` | **523** |
| distinct `pending*` signal names for one idea | **20** |

`Stores.tsx` is the concrete case the owner saw: it ends in two permanently-visible cards, "Create store" and "Create brand", present whether or not anyone wants to create anything, occupying the bottom half of the screen below the table.

None of this is for want of a kit. F2 shipped `Modal`, `Drawer`, `ConfirmDialog` and `FormField`, and migrated five screens — its own declared scope. The other thirty-five were never migrated, and nothing prevented new ones from arriving hand-rolled. `docs/cloud-admin-ux-plan.md` Stage 4 item 15 says, in as many words, *"Create actions move into the page header as a primary button opening a `Modal`/`Drawer`, instead of a form card below the empty table. Stores first."* It was recorded as delivered. It was not built.

So the problem is not "the console lacks components". It is that **there is no single answer to "how do I add a record", and nothing makes an answer stick.**

**Decision.**

1. **A shell owns the chrome; the screen keeps its fields.** `FormPanel` renders as a `Drawer` (default) or a `Modal` (`as="modal"`, for a form of about three fields or fewer), titles itself from the lifecycle state, and owns the footer, the submit button's disabled-while-saving state, the error banner, the Escape/backdrop close and the unsaved-changes guard. What it does **not** own is the fields: each screen writes its own `FormField` children, as JSX.

2. **Deliberately not a declarative field schema.** A `fields: [{name, type, …}]` array would compress the common screens further, and it was rejected. The console's authoring surfaces are not uniform: the floor plan is a spatial editor, `Layout` is a per-channel grid of device-shaped buttons, `Translations` is a locale matrix, `TaxRates` is a class × channel grid, `Config` is a capability form generated from a catalogue. A schema wide enough for those stops being a schema, and a schema that they escape is a convention that half the console ignores — which is the failure mode this ADR exists to end, not to repeat one level up. A shell plus real JSX has no escape hatch to need.

3. **`useEntityCrud` owns the lifecycle, per entity type.** One state — `idle | creating | editing | confirming` — plus the subject, a `saving` flag and the last refusal's message, replacing the per-screen `busy`/`error`/`editing`/`pending*` sprawl. Crucially it is **per instance, not per screen**: the audit's finding that "one shared `busy` flag disables every button on a screen during any single call" is a consequence of screens holding one flag, and a screen that authors two entity types now holds two independent lifecycles.

4. **`run()` is the single write path.** `crud.run(() => api.save(…))` clears the previous error, sets `saving`, awaits, and then **closes on success and stays open on failure with the refusal's own message**. Staying open matters beyond tidiness: under ADR-0094 a save carries `If-Match` and can be refused as stale, and the operator's typing is the only copy of their intent. Forty hand-rolled versions of this each got to be subtly wrong; there is now one.

5. **`SelectField` joins `TextField` in the kit**, with the same shape (`label`, `value`, `onChange`, optional `hint`), so the 53 raw `<select>` have somewhere to go. A raw `<select>` is not a styling problem: each one re-invents the label association, and `FormField`'s error slot cannot reach it.

6. **Create moves into the `PageHeader`** as a primary button that opens the panel. The permanently-visible create cards go. This is Stage 4 item 15, finally built.

7. **A gate, in the same track.** U3 adds two checks to the dashboard lint chain: no raw `<select>`/`<input>` under `screens/`, and no create/edit form inside an inline `Card`. F2 built a kit and the tree drifted back because nothing enforced it; a convention with no gate is a preference. The gate is not optional and it is not a later slice.

**Consequences.**

*What an operator gets.* "Add" is a button where the eye already is — beside the title — and it opens a panel over the list rather than pushing the list up. Cancel and Escape both close it; closing with unsaved text asks first. A refused save leaves the form and the typing intact and says why, instead of clearing it.

*What a screen author gets.* Roughly the whole lifecycle stops being their problem. The cost is a real constraint: the panel decides where a form lives and what its footer looks like, so a screen wanting a different arrangement has to change `FormPanel` for everyone rather than doing its own thing quietly. That is the intended trade — it is exactly the freedom that produced ~100 inline cards.

*What this does not do.* It does not touch any read path: `Panel<T>` (`lib/panel.ts`) keeps owning per-read loading/ready/failed, and the resource helper Stage 5 asked for is **not** in this ADR. Combining a write lifecycle and a read abstraction in one change would make both harder to review, and the read side is the larger of the two. It also does not restyle anything: tokens, spacing and typography are untouched, so no screen changes appearance except by moving its create form off the page and into a panel.

*What is left visibly unfinished.* Five of Stage 4's six primitives — `Tooltip`, `KpiTile`, `BulkActions`, `Kebab`, `Tabs` — remain absent; only `Skeleton` was ever built. They are real gaps, they are recorded in the plan's correction, and none of them is on the path from "I want to add a store" to "the store exists", which is what Track U is for.

**Tested by** `dashboard/tests/entity-crud.test.ts` (the lifecycle, and that a refused save keeps the form open with its message) and `dashboard/tests/form-panel.test.tsx` (drawer and modal chrome, the disabled submit, Escape, and the unsaved-changes guard).
