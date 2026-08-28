# ADR-0077 — Campaigns & scheduling: authoring promotions over the finished engine, and publishing them (and any config) on a future date

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-28
**Relates to** [ADR-0033](0033-config-tree.md) (the config tree the `campaigns` node publishes onto, and whose version history the publish preview/diff reuses) · [ADR-0066](0066-cloud-catalog.md) (the menu compiler and per-node publish shape this follows) · [ADR-0074](0074-localization-and-tax.md) (the `tax`-node authoring→publish→edge-applies pattern this mirrors exactly) · [ADR-0037](0037-api-keys.md) (the show-once, hashed-secret shape voucher batches reuse) · `docs/pos-spec.md` §7 (the pricing model the `pos_core::campaign` engine already implements) · `docs/cloud-admin-ux-plan.md` (Track M3)

**Context.** The pricing engine is done; everything around authoring, delivering, and *timing* it is missing.

1. **The engine is finished but has no inputs.** `pos_core::campaign` implements §7 in full — five kinds
   (item-level, combo, bill-level, voucher, manual), a deterministic evaluation order, the line-add vs
   payment-start timing split, weekly schedule windows, minimum-bill and channel conditions, exclusion
   groups, quotas, and the offline rule that skips only the voucher stage. But `Campaign` is a pure
   runtime type with no serde, there is no cloud storage for campaigns, no CRUD, no config node, and
   `session_from_config` reads nothing that would turn an authored promotion into the `Campaign`s the
   edge evaluates. Nobody can author a happy hour.
2. **Vouchers are a wire concept with no minting.** `VoucherId` exists, and the
   `PromotionVoucherReserved` / `PromotionVoucherRedeemed` events already model the atomic
   reserve→redeem uniqueness check. But there is no way to *generate* a batch of voucher codes, store
   them, or hand them out.
3. **Every publish is immediate — a Tet menu needs a human awake at midnight.** The config tree
   (ADR-0033) publishes *now*: a handler compiles a node, sets it on the Store layer, and
   `ConfigTree::publish` versions it on the spot. There is no effective date anywhere, so a menu, price,
   or campaign that should switch on at 00:00 on the first day of Tết requires someone to press publish
   at that moment. The plan calls this out directly.
4. **Publishing is blind.** An operator publishes without seeing what will change. The config version
   history added a *retrospective* diff between two published versions (G2), but nothing shows the diff
   *before* a publish — the change you are about to make against what is live.

**Decision.** Author promotions in the cloud over the existing engine, publish them as one more config
node, and add a general effective-dated publish mechanism the whole config tree can use. This track adds
*authoring, storage, delivery, and timing* — it does not touch the evaluation engine, which stays the
single source of pricing truth.

1. **A `campaigns` config node, faithful to the engine.** A new serializable
   `pos_proto::campaign::PublishedCampaigns` mirrors `pos_core::campaign::Campaign` field-for-field
   (kind, priority, exclusion group, action, conditions incl. the weekly schedule, quota) plus an
   operator-facing `name` the engine does not need to evaluate. `pos_core::campaign` owns the total,
   infallible `campaigns_from_published` conversion — the only place that sees both shapes — so what the
   operator authored is exactly what the store evaluates. Channels ride as `Open<SalesChannel>` so a
   newer cloud's channel token round-trips instead of failing the node, and a token this edge cannot
   understand is simply dropped from the restriction (it could never match a real channel).
2. **Campaign authoring, storage, CRUD.** A tenant-scoped `campaigns` table (migration `0032`, RLS on
   `app.tenant_id`) and a `CampaignStore` seam (`list` / `set` / `delete`) with a `store-postgres`
   adapter and a fake. Admin routes `GET`/`PUT`/`DELETE /admin/campaigns` behind a new
   `console.campaigns.manage`, audited (`campaign.set`, `campaign.delete`), validating each row against
   the engine's shape (a known kind, a rate in range, a well-formed window). Campaigns are authored
   per **tenant** and published per **store**, the split tax rates already use.
3. **Publish `campaigns`, edge applies it.** `admin_publish_campaigns` compiles the tenant's campaigns
   into `PublishedCampaigns`, writes it as the `campaigns` key on the Store layer, and versions it via
   `ConfigTree::publish` — behind `PublishConfig`, audited, exactly as `tax`/`floor`/`menu` do. The
   edge's `session_from_config` gains a `campaigns` branch that parses the node and holds the resulting
   `Vec<Campaign>` on `EdgeSession` for the pricing path, under the **never-blank** rule: an absent or
   unparseable node leaves the store running the campaigns it already had.
4. **Voucher batch generation.** A route mints a batch of *N* unique, high-entropy voucher codes for a
   voucher-kind campaign, stores each as a tenant-scoped voucher instance keyed by `VoucherId`, and
   lists/exports them once for distribution. Redemption stays the engine's existing online
   check-and-mark (the `PromotionVoucher*` events); the **online redemption endpoint that consumes a
   code atomically is a flagged follow-up** — generation and distribution are what M3 owes, and the
   offline rule already greys the voucher stage out when the store cannot reach the cloud.
5. **Effective-dated & scheduled publishes (the headline).** A generic `scheduled_publishes` table
   (migration `0032`) holds a **snapshot** of a node value to publish, its target (store + node key),
   an `effective_at`, and a status. A schedule route captures the candidate node *as it is authored
   now* and stores it pending; a background **activator** loop — same shape as the retention and alert
   loops (a `ClockSource`-driven `pass()` that finds due, unapplied rows and runs them through the
   normal publish path, with task-health self-reporting and errors logged-and-retried, never fatal) —
   applies each when its time arrives. Snapshot-at-schedule (not recompute-at-fire) is deliberate: a Tết
   menu locked in on the 20th publishes exactly what was reviewed, not whatever later edits happened to
   be sitting in the authoring tables. The mechanism is node-agnostic, so menu, tax, and campaign
   publishes can all be future-dated; a scheduled publish can be cancelled while still pending.
6. **Publish preview/diff.** A preview computes `diff(current_effective, candidate_effective)` with the
   existing RFC 7386 merge-patch `diff` (ADR-0033) **without saving**, so an operator sees exactly what a
   publish — immediate or scheduled — will change against what is live, before committing it.

**Permissions.** One new `ConsolePermission`, `console.campaigns.manage` (Owner/Admin — the norm for
authoring), gates campaign CRUD and voucher generation. Publishing campaigns and scheduling any publish
reuse the existing `PublishConfig` (Owner/Admin/Ops), exactly as every other node publish does; preview
and reads reuse `Read`. Deny-by-default and the role table are otherwise unchanged.

**Consequences.**
- A promotion authored in the console reaches the edge and is applied to real bills by the engine that
  was already correct — no pricing logic is duplicated or reinterpreted on the way.
- The config tree gains one Store-layer key (`campaigns`); the merge-on-publish path already preserves
  siblings, so publishing campaigns does not clobber the menu, tax, or floor.
- Scheduled publishing is a capability the *whole* tree gains, not a campaign-only feature: the Tết-menu
  case and a midnight price change are the same mechanism, and it is additive — a store that schedules
  nothing behaves exactly as today.
- The activator is one more supervised background task alongside the five that already run; a failed
  activation is retried and surfaced through task health, never dropped silently, and never fires twice
  (a row moves to applied under the same transaction that publishes it).
- Campaign pricing models and voucher terms are **T2 (Confidential)**: they are configuration, never
  reproduced verbatim in shareable outputs, and the node carries no customer identifier or other T1
  field — a voucher instance is a code, not a person.

**Alternatives considered.**
- *Make `pos_core::campaign::Campaign` itself `Serialize`/`Deserialize` and publish it directly.*
  Rejected: it couples the wire format to a runtime type and breaks the layer rule that `pos-proto`
  owns the wire and `pos-core` owns evaluation. A faithful mirror plus one total conversion keeps both
  honest and lets the wire evolve (e.g. `Open`-wrapped channels) without touching the engine.
- *Recompute the scheduled node from the authoring tables at fire time.* Rejected: it would let edits
  made after review leak into a publish nobody looked at again. Snapshotting what was scheduled is the
  predictable, auditable choice for a Tết menu.
- *A cron/at expression per campaign instead of a scheduled-publish table.* Rejected as both narrower
  and more complex: the operator's mental model is "publish this, then, once", not "run this rule on a
  recurring schedule", and recurrence is already expressed inside a campaign by its weekly window. A
  one-shot effective-dated publish covers the stated need and generalizes across every node.
- *Full online voucher redemption in this track.* Deferred: minting and distributing codes is the M3
  deliverable, and the atomic check-and-mark against the cloud is a runtime online flow (the events
  already exist) better landed with the payment path than with authoring.

**Deferred / flagged follow-ups.**
- **Live bill-flow evaluation of campaigns.** This track *delivers* the `campaigns` node to
  `EdgeSession.campaigns` (and proves the wire→session→engine path in a test), but wiring
  `campaign::evaluate` into the edge's live sale — building the `EvalContext` from the bill and clock,
  honouring the line-add vs payment-start timing split, and rendering each applied campaign as its own
  bill line (§7) — is a runtime-pricing change distinct from authoring. It is deferred deliberately:
  the edge bill assembly today carries a single scalar `bill_discount`, and a partial integration that
  summed campaign reductions into it would violate §7's one-line-per-campaign rule, so this lands whole
  with the sale flow rather than half-done here.
- The online voucher **redemption** endpoint (atomic reserve→redeem consuming a minted code).
- Combo-price and free-item campaign **actions**, and customer-group conditions — named in §7 but
  dependent on the menu/line model the `decide` orchestration carries (the engine already flags these).
- Recurring/rolling scheduled publishes (this track ships one-shot effective-dated publishes).
- A per-campaign overall start/end **date range** distinct from the weekly window, if authoring demand
  appears — today an operator schedules the publish and unpublishes (or schedules an empty node) to end
  it.
