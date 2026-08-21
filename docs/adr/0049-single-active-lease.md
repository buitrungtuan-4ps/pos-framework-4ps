# ADR-0049 — The single-active lease: generation-based, offline-durable, with a disjoint invoice range

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21
**Relates to** [ADR-0013](0013-async-strategy.md) · [ADR-0025](0025-receipt-number-authority.md) · [ADR-0048](0048-ota-rollout-model.md) · `docs/architecture.md` §4 · `docs/roadmap.md` P9

**Context.** A store runs on exactly one active machine at a time, but machines get swapped — a dead
mini-PC replaced in five minutes, the whole "cattle not pets" promise. P9 needs the **lease** that makes
"exactly one active" true across a swap, with three hard requirements from `docs/roadmap.md`: the lease
**does not expire while the machine is offline** (a store cut off from the cloud for days must keep
selling), taking the lease **revokes the old machine to read-only**, and the replacement gets a **fresh
invoice-number range** so that even a window where the old machine is still (wrongly) live cannot
produce a legal invoice number the new machine also produces. The wire already carries an opaque
`LeaseToken` ([`pos_proto::protocol`]); what is missing is the ordering and the invoice-range handoff
that decide who is active and stop a duplicate invoice number.

**Decision.**

- **The lease is ordered by a generation, not a clock.** `lease_standing(held, authoritative)` is a pure
  function of two `LeaseGeneration`s — equal ⇒ `Active`, held-behind ⇒ `Superseded`, held-ahead ⇒
  `Invalid` — with **no time input at all**. That is the whole point: a lease cannot lapse while the
  machine is offline, because nothing about the passage of time can change the verdict. A machine stops
  being active only when a *newer* generation exists, which only happens when the cloud deliberately
  issues one. This is the deliberate opposite of the super-admin session, which is time-bounded
  ([ADR-0034](0034-super-admin-auth.md)) precisely because it is not the sell-offline path.

- **Supersession is explicit and flips the old machine to read-only.** When a replacement is activated,
  the cloud issues the next grant (generation + 1). The old machine, on its next contact, computes
  `Superseded` and must go read-only — it may still be *reachable* and even still *think* it is live
  while offline, but the moment it talks to the cloud it learns it has been replaced and stops selling.
  The verdict is the domain's; enforcing read-only is the edge's (P9e).

- **Each grant carries a disjoint invoice-number range, handed forward, never reused.**
  `issue_replacement(previous, range_size)` starts the new range exactly where the previous one ended,
  so the two are disjoint by construction and the numbers only ever move forward. This is the belt to
  supersession's braces: if the old machine is offline and still issuing invoices from *its* range while
  the new machine issues from the fresh one, the two ranges cannot collide, so **no legal invoice number
  is ever minted twice** — the one duplication a tax authority will not forgive. Legal invoice numbering
  is distinct from the per-store gapless *receipt* counter ([ADR-0025](0025-receipt-number-authority.md));
  the lease guards the former across a swap.

- **It is pure `pos-core`, holding the invariant; the allocation is I/O elsewhere.** The module lives in
  `pos_core::lease`, names no port ([ADR-0013](0013-async-strategy.md)), and works on plain
  `LeaseGeneration` / `InvoiceRange` value types so the simulator (P12) can prove a swap end to end. The
  *actual* allocation of a legal invoice range is a `Fiscalization` call at the cloud (allocate-range),
  which lands with `fiscal-vn` in P10 (gated on A2); the domain carries the range as data and the
  disjointness rule, and the cloud fills real numbers into it.

**Rejected.**

- **A time-to-live on the lease** — rejected outright: a TTL would brick a store that is merely offline,
  which is the normal state P9 is built around, not an error. Supersession is explicit, never temporal.
- **Reusing or merely adjacent (non-disjoint) invoice ranges** — rejected: any overlap between the old
  and new holder's ranges reintroduces exactly the duplicate-legal-invoice-number risk the fresh range
  exists to remove. Disjoint-and-forward is the invariant.
- **Deciding standing by comparing opaque tokens** — rejected: the wire `LeaseToken` is a redacted
  credential with no order, so it cannot answer "who is newer". The generation is the order; the token
  stays the secret the edge presents.
- **Putting the lease logic in the cloud binary or an adapter** — rejected: it must be pure so the
  machine-swap scenario is a simulator test, not a hardware ritual.

**Consequences.**

- `pos-core` gains a `lease` module — `LeaseGeneration`, `InvoiceRange`, `LeaseGrant`, `LeaseStanding`,
  `lease_standing`, and `issue_replacement` — with property tests binding the generation verdict and the
  disjoint-forward range invariant. No new dependency; no `pos-ports`.
- Deliberately elsewhere: allocating a real invoice range and persisting the authoritative generation are
  cloud I/O (`Fiscalization`, P10); presenting the `LeaseToken` and honouring `Superseded` → read-only are
  the edge (P9e); credential storage on the box is the `KeyVault` activation path (P9d).
