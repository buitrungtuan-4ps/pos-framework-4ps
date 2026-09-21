# ADR-0127 — Modifier groups reach the edge

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-21
**Extends** [ADR-0063](0063-store-menu-catalog.md) (the compiled price book this adds to) · [ADR-0066](0066-cloud-catalog.md) (entities 4 and 5, which author the groups)
**Relates to** [ADR-0074](0074-localization-and-tax.md) (the translation shape reused here) · [ADR-0024](0024-protocol-version-negotiation.md) (why this is additive rather than a bump)

**Context.** A modifier is already an ordinary catalog item with its own price and its own recipe ([ADR-0066](0066-cloud-catalog.md) entity 4), and a **modifier group** is already a min/max selection rule attached to items (entity 5). The console authors both: `dashboard/src/screens/catalog/Modifiers.tsx` creates, renames and archives a group with its rule and its members, and `pos_cloud::catalog::ModifierGroup` stores it.

None of it reaches a store. `crates/pos-cloud/src/catalog.rs` says so in place:

> This is **authoring only** today — the compiled `pos_proto::MenuEntry` carries no modifier reference yet; wiring modifiers to the edge is a `pos-proto`/ADR-0063 extension and its own resolver slice.

So the till cannot ask what a pizza's sizes are, because nothing it syncs knows. What it *can* do is record the answer: `sales.order_line.added` has carried `modifier_menu_item_ids` since the field was added, a fired line consumes the base recipe plus one recipe per modifier (§8), and `POST /api/tables/{id}/lines` accepts the ids today. **The write path is complete and the read path does not exist** — an operator can be charged for a large pizza only if some other device tells the till which item id "large" is.

This record is the extension the code asks for. It is deliberately the *smallest* one that closes that gap, and it says at the end what it is leaving open.

**Decision.**

1. **`MenuCatalog` gains modifier groups; `MenuEntry` gains the ids of the groups attached to it.** Both additive, both `#[serde(default)]`, in the pattern `display_name_translations` and `modifier_menu_item_ids` already set: an edge that predates the field ignores it, a book that omits it loads unchanged, and a store replaying its own log at start-up does not fail to boot. `PROTOCOL_VERSION` is **not** incremented — protocol changes are additive by construction, and incrementing it would force two versions to run in parallel for two releases ([ADR-0024](0024-protocol-version-negotiation.md)) for a field nothing older reads.

2. **The edge is served the compiled view, not the authoring one.** A published group carries its id, its name (with per-locale names, exactly as `MenuEntry` does), `min_select`, `max_select`, and its member `menu_item_id`s. It does **not** carry `tenant_id`, `etag` or `status`: an archived group is simply not published, the same way an unavailable item is published-and-flagged rather than reasoned about at the till. This is the `CatalogItem` → `MenuEntry` relationship applied to a second entity, not a new kind of sync.

3. **Attachment is inverted on the way down.** The cloud holds `attached_item_ids` on the group, because that is how an operator authors it — one "Size" group, pinned to forty pizzas. A till asks the opposite question, once per tap: *what must I ask about this item?* So the compiled form carries `modifier_group_ids` on the entry. The resolver inverts; the till never scans every group looking for itself.

4. **Required is `min_select >= 1`.** No second flag. One number cannot disagree with itself, and "required" and "minimum one" would be two spellings of one fact — the kind of pair that drifts and then has to be reconciled in a refund.

5. **The edge validates the selection; the till is not trusted to have asked.** `add_line` refuses a line whose modifiers break an attached group's rule — too few for `min_select`, too many for `max_select`, or a member of no attached group. The till opens the picker and enforces the same rule for the operator's sake, but a required choice that can be skipped by a device that forgot to ask is a required choice in name only, and the money is already wrong by the time the kitchen reads the ticket.

**Consequences accepted.**

- **A group's rule is checked against the menu the edge holds, which may be a version behind the cloud.** That is the offline contract everywhere else (a line never re-reads the live menu, §14.2) and the same answer applies: the store sells what it was last told it sells.
- **Two round trips become one config.** Nothing new syncs — this rides the `menu` node of the config tree that already arrives ([ADR-0004](0004-cloud-owned-configuration.md), [ADR-0033](0033-config-tree.md)), so a store gains modifiers on the next config it receives and needs no new endpoint, permission or migration.
- **Prices stay where they are.** A modifier's price is its own item's `unit_price`, and a line's `unit_price` is already the summed figure the guest is charged — these ids are what the kitchen and the stock ledger need, not a second source of truth for money. This record adds no pricing rule, which is why it needs no change to how money or tax works (§7).

**Deliberately not decided here.** Two things `docs/pos-spec.md` §27 asks for are out of scope, and saying so is the point rather than an omission:

- **Nesting.** §27 allows a group to contain a group. The authoring shape has never supported it — `member_item_ids` are items — so nesting is not something this record could carry down; it would have to be authored first. A flat group answers "what size, what toppings", which is every case Pizza 4P's has today.
- **Half-and-half (`SPLIT_ITEM`).** §27 describes one line with two halves, configurable pricing defaulting to the dearer half, and a bill of materials computed per fraction. `Quantity::HALF` exists for it. It is **not a selection rule**: it changes the shape of a *line*, how that line is priced, and how its recipe is consumed. Folding it into the group mechanism would put a pricing rule inside a picker, and the two would then have to be untangled. It gets its own record.
