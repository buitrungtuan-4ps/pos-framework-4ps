# ADR-0214 — A till's external lookup is one catalogue act, answered by the gateway through its edge

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-12
· Extends [ADR-0198](0198-a-store-reaches-the-internet-only-through-the-cloud.md) (the till's end of
the path) and [ADR-0191](0191-every-act-is-in-a-typed-catalogue.md) (the first `ACTION_EFFECT_EXTERNAL`
act) · Relates to [ADR-0190](0190-a-screen-is-drawn-from-the-package.md),
[ADR-0192](0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md),
[ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md)

## Context

ADR-0198 fixes the path of an external call from the edge onwards: the edge calls
`POST /sync/stores/{store_id}/gateway/calls`, and the cloud calls a tenant's declared endpoint
behind the webhook guard. It says the till reaches its edge first, and that a required external
flow step declares an offline branch, but not how. Phase 5's exit criterion needs a till's
external lookup to go through the gateway. Four things are undecided, and each touches a
contract this repository holds to an ADR:

- **The act.** The catalogue has the effect class `ACTION_EFFECT_EXTERNAL` and no act in it.
  An act is a `pos-proto` entry with parameters, a permission and a frequency.
- **The permission** a person needs to run it. The store permission catalogue is a snapshot.
- **The edge route** the till calls, which is an `/api/*` route (ADR-0111: additive only).
- **Where the answer shows**, and the flow step's offline branch, both `pos-proto` shapes.

## Options considered

1. **The till calls the cloud.** Refused by ADR-0198.
2. **One act per use** (`loyalty.balance.read`, `voucher.code.check`). Every new lookup is a
   release, which is what endpoints-as-data exist to avoid.
3. **One generic lookup act, parameterised by the endpoint id, whose answer is read-only data
   drawn by one component** (proposed).

## Decision (proposed)

1. **One act, `integrations.lookup.run`**, effect `ACTION_EFFECT_EXTERNAL`, frequency `RARE`.
   Parameters: `gateway_endpoint_id` (fixed by the package, never typed) and `values`, the
   endpoint's request fields by name, each supplied by the screen (`order_id`, `table_id`) or by a
   `lookup_field` component on the same screen that a person types into. A value's kind is the
   endpoint's declared kind; the edge refuses a mismatch before calling out.
2. **A permission, `integrations.lookup.run`**, not PIN-flagged. Every role that exists when it
   lands is granted it directly once by migration, as `0073` did for the till's acts; the owner
   may say otherwise.
3. **The edge route `POST /api/gateway/calls`**, behind the till session and that permission. The
   edge mints the `idempotency_key` per act (the till sends its own on a retry of the same act),
   calls the cloud with ADR-0198's deadline plus one second, and answers the typed result or a
   refusal: `GATEWAY_OFFLINE` at once when the cloud is unreachable, `GATEWAY_TIMEOUT`,
   `GATEWAY_REFUSED` (the target's own error), `GATEWAY_BUSY` (a limit or an open breaker). Each
   refusal has a translation key the till shows.
4. **The answer is drawn, never kept.** A new catalogue component, `lookup_result`, draws the last
   answer of the screen's lookup: each result field the endpoint declares, by its label key. The
   till paints the act pending first (ADR-0196). A lookup writes no event and moves no money; a
   result that should change the order needs a separate act a person runs, so nothing a target
   says reaches the event log by itself. The answer is held in the till's memory for the screen
   only and never logged.
5. **A flow step's offline branch.** `FlowStep` gains `offline`, absent by default: either
   `{ "skip_note_key": … }` (skip, showing the note) or `{ "step_id": … }` (go to another step of
   the flow). The cloud refuses at publish a required step whose act is `ACTION_EFFECT_EXTERNAL`
   without one (ADR-0198 decision 8), with reason `NO_OFFLINE_BRANCH`.
6. **Personal data.** A `values` entry whose endpoint field is declared personal is refused by the
   edge until ADR-0198 decision 9's processor declaration is built; until then no endpoint may
   declare a personal field.
7. **A `min_edge_version` floor** for a version that names the act or the component.

## Consequences accepted

- Additive `pos-proto` changes (one act, one component, one `FlowStep` field), one permission and
  one edge route, each held by its snapshot, with the owner's review of `pos-proto`.
- A lookup depends on the cloud, as ADR-0198 accepts; the till says so at once when it is offline.
- A lookup cannot act on what it reads. That is deliberate: acting is a separate, reviewed act.
