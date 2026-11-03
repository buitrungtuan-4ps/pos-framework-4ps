# ADR-0191 — Every act is in a typed catalogue

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Relates to
[ADR-0115](0115-reason-codes-are-a-managed-list.md),
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md),
[ADR-0173](0173-the-till-declares-each-piece-of-store-data-once.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0190](0190-a-screen-is-drawn-from-the-package.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

A package's buttons, flow steps and reactions must do things: add a line, seat a table, settle a
bill. Today an act is a handler in a screen that calls one `api.*` function. Its rules are spread
over three places:

- **The permission** a route needs is in `crates/pos-edge/src/http/route_permissions.rs`, held to
  `docs/snapshots/routes.txt` by a test ([ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md)).
- **Whether it paints first** is an allow-list of four acts in `ui/src/state/optimistic.ts`
  (ADR-0173 decision 4, ADR-0188).
- **Whether it is common or rare** is in `ui/scripts/step-tasks.mjs`.

The owner decided that a flow may only invoke an existing money operation and never computes an
amount, and that external calls go through the cloud. Nothing today says which acts touch money or
leave the store.

## Options considered

1. **Let a package call edge routes by path.** No types, no effect class, and a route rename would
   break packages silently.
2. **A catalogue in the till only.** The cloud could not validate a package, nor compute its minimum
   edge version.
3. **One catalogue in `pos-proto`, read by the cloud, the edge and the till** (chosen).

## Decision

1. **An action is a catalogue entry.** Each has:
   - an `action_id` in the permission taxonomy's shape, `<domain>.<resource>.<verb>`
     (`sales.line.add`, `billing.bill.settle`);
   - typed parameters;
   - the permissions a person needs, which a test holds equal to the route table for the route it
     calls;
   - an **effect class**;
   - a frequency, `ACTION_FREQUENCY_COMMON` or `ACTION_FREQUENCY_RARE`, for the tap budget
     ([ADR-0190](0190-a-screen-is-drawn-from-the-package.md) decision 7);
   - the refusals the till can predict before sending ([ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md));
   - the release that introduced it, for the minimum edge version ([ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md)).
2. **Four effect classes** (`ACTION_EFFECT_UNSPECIFIED` is refused):
   - `ACTION_EFFECT_UI`: changes only what the till shows (navigate, open a panel, filter a list).
   - `ACTION_EFFECT_EDGE_WRITE`: one edge command that writes events and moves no money (add a line,
     seat a table, record a guest count, fire to the kitchen).
   - `ACTION_EFFECT_MONEY`: one existing edge money command (settle, void, discount, comp, split,
     paid in, paid out, drawer open). **Its amount parameters can be bound only to what a person
     types or to a figure the edge has answered with, never to a rule's result**, and the command's
     own checks (ceilings, approvals, reasons) run as they do today.
   - `ACTION_EFFECT_EXTERNAL`: a call through the cloud gateway (ADR-0198). The till never calls the
     internet.
3. **One declaration, three readers.** The catalogue is Rust in `pos-proto`; the till's types come
   through the existing generation. The cloud validates packages against it, the edge maps each
   entry to its command, and the till's renderer binds buttons to it.
4. **Additive only.** A catalogue snapshot under `docs/snapshots/` is diffed in CI like the other
   snapshots. An action is deprecated, never removed or renamed, so a published package keeps
   working.
5. **No package-defined money act.** A package composes catalogue actions; it cannot declare a new
   one. A new act is a release, with its own route and, for money, its own ADR.

## Consequences accepted

- **Additions to `pos-proto`:** the catalogue types and entries, additive, with the owner's review.
- **Today's handlers move behind catalogue ids** in phase 2. The step tasks and the optimistic
  allow-list become properties of entries rather than separate lists.
- **A test per entry** checks its permission against the route table and its effect class against
  the command it calls, so a money command cannot be declared as an edge write.
- **The catalogue grows with the product.** Each new act is a reviewed change, which is the point.
