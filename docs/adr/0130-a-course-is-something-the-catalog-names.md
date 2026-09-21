# ADR-0130 — A course is something the catalog names

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-21
**Extends** [ADR-0066](0066-cloud-catalog.md) (the authoring model this adds an entity to) · [ADR-0063](0063-store-menu-catalog.md) (the compiled book that carries it down)
**Relates to** [ADR-0072](0072-floor-and-kitchen.md) (the routing rule that already matches on a course) · [ADR-0127](0127-modifier-groups-reach-the-edge.md) (the same move, made for the same reason, one entity earlier)

**Context.** A course is built. All of it, except the thing itself.

- `CourseId` is a wire identifier, and its doc says what it is for: *"A course: starter, main, dessert. Configured per brand, so it is data rather than an enumeration."*
- `sales.order_line.added` carries `course_id`, and `LineRecord` keeps it.
- `POST /api/tables/{id}/lines` accepts one.
- `pos_proto::floor::RoutingRule` matches on one — *"Match any line on a course"* — and `pos_core::floor::route_station` honours it, with an item match taking precedence.
- The cloud's `POST /admin/floor/routing-rules` accepts a `course_id` and refuses a rule that names both an item and a course.
- `Capability::Courses` exists, defaults on, and `decide_line` **refuses a fire-by-course when it is off** — with a test.
- `docs/pos-spec.md` §3 promises *"Courses group lines (starter, main, dessert). Fire by course or fire everything."*

**Nothing anywhere creates, names, orders, lists or publishes a course.** There is no `Course` entity in `pos_cloud::catalog`, no node in the config tree, no screen in the console, and no picker on the till. Every `course_id` in the tree is a foreign key to a table that does not exist.

The sharpest form of it is in the cloud's admin API: an operator can author a routing rule that sends "any line on course X" to the pastry station, and the write is accepted after checking only that `menu_item_id` and `course_id` are not both set. It cannot check that the course exists, because there is nowhere to look. The rule is then published to a store, which honours it, and it matches nothing — for ever, silently, because no line can carry that id either.

This is the eighth time in this run of work that a facility has turned out to be built end to end around something never authored, and it is the most complete: the event, the projection, the route, the wire type, the routing function, the capability gate and the domain refusal are all present and correct.

**Decision.**

1. **A course is an authoring entity in the cloud catalog** — `pos_cloud::catalog::Course`, tenant-scoped like [`ModifierGroup`](0066-cloud-catalog.md), with a `CourseId`, a name, a `sort`, and an `EntityStatus`. `CourseId` is `pos-proto`'s, crossing the seam the way `MenuItemId` and (since ADR-0127) `ModifierGroupId` do, because a course id is on a published routing rule and on a published event already.

2. **The `sort` is the point, not decoration.** "Starter before main before dessert" is the entire meaning of the grouping; a set of names with no order is a taxonomy, not a service sequence. Ties break by id, as everywhere else in the compiler, so a re-compile of unchanged authoring stays byte-identical.

3. **An item declares its course; the till does not ask.** `CatalogItem` gains an optional `course_id`, exactly as it carries `tax_class_id` — a pizza is a main, and making a server say so on every tap would be a tap per dish for a fact the catalog knows. An item with none is on no course, which is every item in every store today and stays correct.

   The **per-line override is deliberately not decided here.** The request it would serve — *"bring my main with the starters"* — is about **when the food goes**, and firing already answers that: a server fires that line early. Relabelling its course to mean "sooner" would put a timing decision inside a taxonomy, and the two would then have to be untangled in the kitchen. If a case appears that firing cannot express, it gets its own record.

4. **Courses ride the `menu` node, with the entry naming its course.** `MenuCatalog` gains the published courses (id, name, per-locale names, sort) and `MenuEntry` gains `course_id` — both additive and `#[serde(default)]`, the pattern ADR-0127 set one entity ago and `display_name_translations` before it. `PROTOCOL_VERSION` is **not** incremented ([ADR-0024](0024-protocol-version-negotiation.md)).

   On the `menu` node and not a node of its own, because a course is a property of the price book's items: a store that receives a new menu and an old course list would group its dishes wrongly, and one document cannot disagree with itself.

5. **An archived course is not published**, and an entry naming a course the catalog does not carry is drawn ungrouped rather than refused — the same forgiving posture `MenuCatalog::groups_for` already takes for a modifier group, and for the same reason: a partial publish is a real state, and a till that stopped selling over a late-arriving grouping would be worse than one that groups a dish under nothing.

6. **A routing rule's course must exist.** Once there is somewhere to look, `POST`/`PATCH` on a routing rule refuses a `course_id` no active course carries, the way the menu graph's cycle is refused at the write rather than at publish ([`would_cycle`](../../crates/pos-cloud/src/catalog_compiler.rs)). A rule that can never match is a rule an operator will never find out about.

7. **Firing by course is the existing command, given its argument.** `LineCommand::Fire { course }` and its `courses_enabled` gate are already written and tested; what is added is a route that fires every unfired line on one course of an order, and a control that offers the courses in `sort` order. **Fire-everything stays exactly as it is** — one button, one transaction — because that is what most orders are, on every store with the capability off and most with it on.

**Consequences accepted.**

- **A store gains courses on its next config, and needs no migration.** Both fields are additive on a document every store already receives, and an edge that predates them ignores them.
- **Grouping the kitchen board by course is not in this record.** `docs/pos-spec.md` §5 asks for KDS cards *"grouped by order and course"*; the board can only group by what a line carries, so this is its prerequisite and not its implementation.
- **Two taxonomies now sit on an item** — its `item_category_id` (operational, what it reports under) and its `course_id` (service, when it goes out). They are deliberately distinct for the reason a display category is distinct from an item category (ADR-0066 entity 11): a dessert wine reports under drinks and goes out with dessert, and collapsing them would make one of those two facts unsayable.
- **The `sort` is the store's, not the guest's.** A store that serves dessert before the main course for a tasting menu authors it that way; nothing in the model privileges a particular sequence.

**Deliberately not decided here.**

- **Fire rounds.** `docs/roadmap-v3.md` B2.7 pairs "course entity" with "fire-round and kitchen-ticket print". A round is a *repeatable* fire across courses ("away on two"), which is a different act from firing a course, and it needs the ticket printer to say which round it is. It gets its own record once courses exist to round over.
- **A course-level hold.** Holding every line on a course is expressible today as holding each line, and whether it deserves one act is a question about the screen, not the model.
- **Per-course timing or pacing.** Nothing here measures how long a course waits, alerts on it, or paces the kitchen. That is a KDS concern with its own data.
