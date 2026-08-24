# ADR-0048 — OTA rollout: rings, canary, self-test rollback, and a kill switch, as one pure decision

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21
**Relates to** [ADR-0013](0013-async-strategy.md) · [ADR-0047](0047-minisign-verification.md) · `docs/architecture.md` §4 · `docs/roadmap.md` P9

**Context.** [ADR-0047](0047-minisign-verification.md) gave us "is this artifact validly signed?". P9's
next question is "given that it is, should *this* device install it *now*?" — the rollout. The
requirements from `docs/roadmap.md` P9: updates move in **rings** (lab → pilot → fleet), with a
**canary** slice of the fleet added at scale (the archive says 25%); a device that installs a version
and then **fails its self-test rolls back automatically**; a **kill switch** halts a bad rollout
fleet-wide; and a signature from a **revoked key** is refused even though the cryptography checks out.
The roadmap also flags that the docs count the rings three different ways and says to *pin it in
config* rather than in code.

The shape question: where does this logic live, and is it a state machine or a function? The rollout
decision is a **pure function of present facts** — the device's current version, its ring and canary
bucket, the last self-test result, and the update the cloud has published — with no I/O. That makes it
domain logic the simulator can exhaust (P12), not adapter code.

**Decision.**

- **One pure function, `pos_core::ota::decide_rollout`, with a fixed precedence.** It takes the
  device's state, the published update, and the revoked-key list, and returns exactly one
  `RolloutDecision`. The precedence is the whole safety argument, so it is fixed and tested in that
  order:
  1. **Roll back** if the running version failed its self-test — recovering a broken device outranks
     everything, including the kill switch (a bricked store must revert whatever else is true).
  2. **Skip (already current)** if the target is not newer than what is running — nothing to weigh.
  3. **Halt** if the kill switch is engaged — there is a newer target, but the operator pulled the
     brake, so the device holds where it is.
  4. **Refuse** if the update's signing key id is on the revocation list — a valid signature from a
     retired key is still not to be trusted ([ADR-0047](0047-minisign-verification.md): revocation is
     policy, decided here, not in the verifier).
  5. **Skip (not in ring)** if the device's ring is below the update's `min_ring`.
  6. **Skip (not in canary yet)** if the device is in the fleet ring and its canary bucket is at or
     above the published rollout percent.
  7. **Install** otherwise.

- **The ring set and the canary are pinned in the *published update*, not in code** — the resolution
  the roadmap asked for. `Ring` is the ordered `Lab < Pilot < Fleet`; the cloud publishes an update as
  a `(min_ring, fleet_rollout_percent)` pair, so "roll to lab, then pilot, then 25% of fleet, then
  100%" is data the cloud changes over the rollout's life, never a recompile. Lab and pilot are the
  test cohort and take everything at or above `min_ring`; only fleet devices are gated by the percent,
  by a stable per-device **canary bucket** (`0..100`) so a device's place in the ramp does not jump
  between evaluations.

- **The signing key id is raw bytes in the domain, mapped at the edge.** `pos-core` must not depend on
  `pos-ports` (the backbone sibling rule, [ADR-0013](0013-async-strategy.md)), and the key id lives in
  `pos_ports::signer::KeyId`. So the rollout domain represents it as the plain `[u8; 8]` it is, and the
  edge updater maps `KeyId ↔ [u8; 8]` at the boundary — the same way every other pos-ports type is
  kept out of the pure core.

- **The decision is stateless; the device's *persisted* state is just two facts.** What the edge must
  remember across a reboot is only its current version and its last self-test result; the ring and
  canary bucket are assigned once and the published update arrives over the wire. So this is a function
  over those facts, not a persistent state machine on the `pos_core::state_machine` framework — there
  is no lifecycle to store, only a verdict to compute each time an update is seen.

**Rejected.**

- **A rollout state machine on the `machines` framework** — rejected: the four business machines model
  a lifecycle with persisted transitions; the rollout is a recomputed verdict with nothing to persist
  beyond current-version and last-self-test. A machine would invent state the problem does not have.
- **Putting the logic in the edge binary or an adapter** — rejected: it must be pure so the simulator
  (P12) can prove a ring rollout, a failed self-test rolling back, and the kill switch, in
  milliseconds and without hardware.
- **`pos-core` depending on `pos-ports` for `KeyId`** — rejected outright: it would break the graph
  invariant that the domain performs no I/O and names no port. Raw `[u8; 8]` at the seam costs one
  `From` at the edge and keeps the core clean.
- **A fixed ring count in code** — rejected: the docs disagree on whether there are three rings or
  four, so the count is published data (`min_ring` + `fleet_rollout_percent`), and adding a "25% ring"
  is setting a number, not shipping a release.

**Consequences.**

- `pos-core` gains a pure `ota` module — `ReleaseVersion`, `Ring`, `DeviceState`, `PublishedUpdate`,
  `RolloutDecision`, and `decide_rollout` — with property tests binding each precedence rule. It uses
  only the core's own types; no new dependency, no `pos-ports`.
- What this does **not** cover, deliberately: the `.pre-update` database copy and the act of installing
  or reverting are edge I/O (the updater that drives this verdict, P9e); the revocation list's delivery
  is the config tree ([ADR-0033](0033-config-tree.md)); and verifying the artifact is
  [ADR-0047](0047-minisign-verification.md). This module decides; the edge acts on the decision.
