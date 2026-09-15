# ADR-0125 — A release is one decision and many writes

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-15
**Answers** findings F8 / F9 / F10 / F17 and decision D8 of [`docs/cloud-admin-ux-plan.md`](../cloud-admin-ux-plan.md) §4w
**Relates to** [ADR-0077](0077-campaigns-and-scheduling.md) (the scheduled-publish table and activator this is a grouping over) · [ADR-0122](0122-a-store-group-is-a-delivery-cohort.md) (the cohort a release may target, and the prerequisite rules it inherits) · [ADR-0095](0095-conditional-writes-for-collections.md) (the version history that is the only undo for an applied write) · [ADR-0014](0014-datetime-library.md) (the cutoff a wall-clock release is quietly adjacent to) · [ADR-0114](0114-region-is-required-recorded-visible.md) (the `locale.country_code` that made a store's timezone mandatory)

**Context.** The console can publish one node to one store, now ([ADR-0033](0033-config-tree.md)); one node to a cohort, now ([ADR-0122](0122-a-store-group-is-a-delivery-cohort.md)); and one node to one store, later ([ADR-0077](0077-campaigns-and-scheduling.md)'s `scheduled_publishes` table and its activator). What does not exist is the thing an operator actually does.

A Tết menu is not one node. It is a menu, the tax rates it prices against, the campaigns that discount it, the reason codes the staff will void it with, and the layout that shows it — five to nine nodes, to forty stores, all switching on together. Today that is forty-five to eighty separate publishes, each with its own button, each landing whenever the operator got to it. Three failures follow, and they are the four findings:

- **F8** · Nothing groups the writes. A half-published Tết is a shop selling last year's menu at this year's prices, and nothing in the console can even name the set that was supposed to go together.
- **F9** · Nothing reports node × store. After eighty publishes the only record is eighty audit rows; "did Ginza get the new tax table?" is a question an operator answers by opening Ginza.
- **F10** · Nothing is scheduled as a set. `scheduled_publishes` takes one row at a time and has no id above the row, so "cancel Tết" is not expressible.
- **F17 / D8** · "Monday 04:00 at each store" is not a time. It is one instant per timezone, and nothing converts it. A fleet spanning Ho Chi Minh City and Tokyo has a two-hour spread that nobody has ever had to write down, so today an operator picks one UTC instant and half the fleet switches over during service.

The mechanism to build on already exists and already says so. ADR-0077's module header records that the activator is node-agnostic — *"menu, tax, and campaign publishes can all be future-dated; this track wires the campaign schedule route, the rest reuse the same store and activator."* This record is that reuse, plus the two things a grouping needs that a row does not have: an identity, and a timezone.

**Decision.**

1. **A release is a name, a set of node snapshots, a target, and a moment. It is not a new publish path.** Creating a release expands to one `scheduled_publishes` row per `(node, store)` pair, each carrying the release's id, and the existing activator applies them. No second activator, no second way for a config version to be born, and no node that can be published through a release but not directly.

   The alternative — a release table that holds values and applies them itself — was rejected for the reason the config tree exists at all: two code paths that write config versions become two sets of prerequisite checks, two audit shapes, and two ways to be wrong about what a store is running. A release is bookkeeping over writes that already had a home.

2. **The wall-clock time is converted to a per-store instant on the cloud, at schedule time.** An operator says "Monday 04:00, local"; the cloud reads each target store's published `locale.timezone` and computes that store's instant. This is decision **D8, option O2**, and the two rejected options are recorded because both are defensible:

   - **Convert at fire time** (keep the wall-clock string, resolve it when the activator wakes). Rejected because the operator cannot then be *shown* what they are committing to. A release that says "04:00 local, at these forty stores" and lists forty instants is reviewable; one that says "trust me on Monday" is not. It also makes a timezone change between schedule and fire silently move a publish an operator already approved.
   - **Convert at the edge** (ship the wall-clock time; let each store fire it). This is option O3, and it is the better answer in the long run — it survives a cloud outage over the switchover, which O2 does not. It is deferred to the **B·W7** line of `docs/roadmap.md` because it needs the edge to hold and honour a schedule, which is a second scheduler at the one place in the system with no operator, and because the cloud already holds one that works.

   A store whose `locale` node has never been published **cannot be scheduled into a wall-clock release**, and the refusal names the store and the node it is waiting for. This is not a gap to paper over: "04:00 at this store" has no meaning at a store whose timezone nobody recorded, and quietly assuming UTC would put the publish in the middle of a Vietnamese dinner service. A release given an **instant** instead of a wall-clock time needs no locale and is always available, which is the escape hatch for a store being set up.

   The business-date cutoff ([ADR-0014](0014-datetime-library.md)) is *not* used to place the publish. It is offered in the console as the default hour, because "before the shop opens" is what an operator means, but the value that lands in the row is a plain wall-clock hour they can change. A cutoff is an accounting boundary; conflating it with a deployment window would make changing one move the other.

3. **The snapshot is taken at schedule time, and so is the store list.** ADR-0077 already decided snapshot-at-schedule for the value, on the grounds that what fires should be what was reviewed. A release carries that decision up one level and applies it to the *target* as well: a release aimed at a store group expands to concrete store ids the moment it is scheduled.

   A group's membership can change between Thursday and Monday. If the release expanded at fire time, a store added on Friday would receive a menu nobody checked it against — same failure as a value edited on Friday leaking into Monday's publish, which is the failure ADR-0077 exists to prevent. The release records the group it came from so the report can say "the Hanoi cohort, as it stood on Thursday", and a store added since is a new release, not a surprise.

4. **Five states, and `partial` is the one the record exists for.** `draft` → `scheduled` → `applying` → `applied` | `partial`.

   - **`draft`** because a release across nine nodes and forty stores is assembled over minutes, and half-assembled must not fire. Nothing is scheduled until the operator says so.
   - **`applying`** because 360 writes are not instantaneous, and a console that shows `scheduled` while the activator is halfway through is lying about a state an operator may be watching during a switchover.
   - **`partial`** because N × M writes fail individually. A release that can only report "done" or "failed" is useless at forty stores: the operator needs the two shops that did not take it, not a red cross over the fleet. `partial` is a terminal state with a per-pair report, not a retry loop — what to do about two failed stores is a judgement, and the console offers the pairs for a new release rather than guessing.

   There is no `rolled_back`. An applied write is a config version, and the way back is [ADR-0095](0095-conditional-writes-for-collections.md)'s version history — the same rollback an immediate publish gets. A release-shaped undo would be a second rollback path with its own prerequisite ordering, and the failure it guards against (nine nodes rolled back in the wrong order) is one the config tree's own ordering already owns.

5. **Cancel is only before `applying`, and it is per release or per pair.** Once a pair has published there is nothing to cancel — see above. Cancelling a release cancels its still-pending rows and leaves the applied ones alone, which is exactly the shape that produces a `partial`, and the report says so rather than pretending the release never happened.

6. **Prerequisites are checked twice: at schedule time and again at fire time.** [ADR-0122](0122-a-store-group-is-a-delivery-cohort.md) §7's rules (tax and locale before menu) apply unchanged, and a release is the first place where the *order within a set* matters: a release carrying both `tax` and `menu` satisfies its own prerequisite, and the activator must apply them in dependency order for that to be true.

   Checked at schedule time so the operator is told now, while they can fix it. Checked again at fire time because the world moves in between — a tax class archived on Sunday for a menu release on Monday is a real sequence, and the fire-time failure becomes a named `partial` rather than a store that boots, syncs, shows the menu, and raises `TaxRateNotConfigured` at the payment screen. That is the same failure [ADR-0122](0122-a-store-group-is-a-delivery-cohort.md) §7 was written for, and it is worth failing twice to avoid.

7. **A release is tenant-scoped and audited as one action with many effects.** One `release.scheduled` audit row naming the release, its nodes, its store count and its moment; then the ordinary per-publish audit rows the config tree already writes, each carrying the release id. The reason for both is that the two questions are different: "who decided to ship Tết" has one answer, and "when did Ginza get it" has forty.

**Consequences.**

- `scheduled_publishes` gains a nullable `release_id`. Nullable because ADR-0077's per-store campaign schedule keeps working and is not a release; a row with no release id is exactly today's behaviour.
- A new `releases` table holds the identity, the name, the state, the target description, the wall-clock or instant moment, and the actor. The per-pair outcome lives on the `scheduled_publishes` rows, because that is where a publish's outcome already lives; the release's state is derived from them and stored so a list read does not have to aggregate.
- The activator gains dependency ordering within a release and a state transition when the last pair of a release settles. It stays one loop.
- The console gains two screens: a publish centre by store, and a calendar by tenant. `PublishBar` gains "Add to a release" beside "Publish", which is the first time the bar offers a choice rather than an action — the plan (§4w, L1–L4) records this as the point of the whole track: an operator who has assembled nine nodes should not have to remember to press nine buttons on Monday morning.
- **A store with no published `locale` becomes visible as a gap**, because a wall-clock release refuses it by name. That is a change in what the console demands of a half-provisioned store, and it is intended: [ADR-0114](0114-region-is-required-recorded-visible.md) already made the country mandatory on the locale publish for a related reason.
- Cloud-side conversion means a **cloud outage across the switchover delays the release** rather than firing it locally. The activator applies overdue rows when it recovers, so the publish is late, not lost — and "late" is the honest failure mode of option O2, recorded here rather than discovered during Tết. Option O3 is the fix, and it is on the roadmap.
