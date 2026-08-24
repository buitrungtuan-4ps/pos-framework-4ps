# ADR-0055 — The edge OTA updater orchestrates behind an install seam; the OS steps are gated

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21
**Relates to** [ADR-0003](0003-cattle-not-pets.md) · [ADR-0047](0047-minisign-verification.md) · [ADR-0048](0048-ota-rollout-model.md) · [ADR-0049](0049-single-active-lease.md) · [ADR-0053](0053-cloud-sync-port.md)

**Context.** [ADR-0048](0048-ota-rollout-model.md) made the rollout *decision* a pure, total function
(`pos_core::ota::decide_rollout`), and [ADR-0047](0047-minisign-verification.md) made update
*verification* a `Signer` port. [ADR-0053](0053-cloud-sync-port.md) added `CloudSync::fetch_update` for
the artifact. What is still missing is the thing that runs on the box and *ties them together*: read
the published rollout, decide, fetch, verify, and — only if all of that holds — install, self-test,
and either keep or revert. That orchestration is what this ADR fixes.

The hard part is not the logic; it is the boundary. Half of the steps are ordinary, testable
orchestration (decide, fetch, verify, choose to install or roll back). The other half — copying the
live database to `.pre-update`, writing the new binary, running the post-install self-test, rebooting,
reverting to the last-good binary — are **operations on the real machine** that `docs/roadmap.md` P9
calls out as the layer that needs a human and real hardware (the 5–10 minute Windows swap). Faking
them in the ten-minute pull-request gate would be theatre. So the boundary between the two is the
decision.

**Decision.**

- **The edge composes an `OtaUpdater` over three seams: `CloudSync` (fetch), `Signer` (verify), and a
  new `UpdateInstaller` (the OS steps).** The updater holds the two public keys baked into the binary
  ([ADR-0047](0047-minisign-verification.md)). `run` takes the device's persisted `DeviceState`, the
  `PublishedUpdate` and revoked-key list read from configuration ([ADR-0048](0048-ota-rollout-model.md),
  [ADR-0052](0052-ota-rollout-config.md)), the artifact's detached signature, and the box's lease
  standing.

- **Fixed order, and verify before anything touches the disk.** `run` is:
  1. If the lease is `Superseded` ([ADR-0049](0049-single-active-lease.md)) — another machine holds the
     store — do nothing but report `ReadOnly`. A demoted box never updates.
  2. `decide_rollout(...)`. `RollBack` → `installer.rollback()`. `Halt`/`Refuse`/`Skip` → report and
     stop, touching nothing.
  3. `Install`: `fetch_update` the artifact; **verify it before trusting it** — read the signature's
     claimed key id, refuse a revoked key id outright, select the matching baked-in key, and
     `Signer::verify`. A transport is not a trust boundary ([ADR-0053](0053-cloud-sync-port.md)), so a
     spoofed cloud fails here, not after install.
  4. Only then the OS steps, in order: `stage_backup` (the `.pre-update` database copy), `apply` (write
     the verified artifact), `self_test`. A passing self-test → `commit`; a failing one → `rollback`.
     A self-test that fails is a routine rollback, not an error.

- **`UpdateInstaller` is the gated seam, and it is the *only* thing not exercised in the pull-request
  gate.** Its methods are the real-machine operations: `stage_backup`, `apply`, `self_test`, `commit`,
  `rollback`. The shipped binary implements them against the OS (write the binary, reboot, run the
  smoke test) — the human-and-hardware step. The orchestration around it is generic over the seam and
  proven against a fake installer plus the in-memory `CloudSync`/`Signer` fakes: every branch — read
  only, roll back, halt, refuse, skip, verify-fails-so-nothing-is-written, self-test-fails-so-rollback,
  install-succeeds — is a test.

- **A failed verification never reaches `apply`, and a self-test failure never reaches `commit`.**
  These two orderings are the safety argument, and both are asserted: a bad signature leaves the disk
  untouched (no `stage_backup`, no `apply`), and a failed self-test leaves the box on its old binary
  (`rollback`, never `commit`).

**Rejected.**

- **Putting the orchestration in `pos-core`** — rejected. It drives ports (`CloudSync`, `Signer`) and
  the OS, so it is application layer, exactly like the activation flow
  ([ADR-0050](0050-activation-code-exchange.md)); `pos-core` stays sans-I/O
  ([ADR-0013](0013-async-strategy.md)).
- **Making `UpdateInstaller` async** — rejected for now. An install is a rare, one-shot operation that
  ends in a reboot; a synchronous seam the orchestrator calls between its `await`s is simpler and the
  blocking is bounded and rare. The seam can go async later behind the same trait if a real installer
  needs it.
- **Fetching-and-installing without a separate verify step** (trusting the transport) — rejected by
  [ADR-0047](0047-minisign-verification.md)/[ADR-0053](0053-cloud-sync-port.md): the signature check is
  the whole point, and it must gate the disk write.
- **Faking the install in CI to claim P9's exit** — rejected. The real Windows swap, the real
  install/reboot, and the minisign keypair generation stay gated, human-and-hardware steps
  (`docs/roadmap.md` P9); this ADR draws that line in the code (the seam) rather than pretending past
  it.

**Consequences.**

- **The edge OTA path is real and tested up to the machine's edge.** Every decision and the verify
  gate run in the pull-request `test` job against the fakes; only the five `UpdateInstaller` calls wait
  for a real box. The simulator scenarios ([ADR-0048](0048-ota-rollout-model.md),
  `crates/pos-core/tests/ota_rollout.rs`) already prove the *decision* over a virtual fleet; this
  proves the *orchestration* around it.
- **Composition into the shipped binary waits on the real `UpdateInstaller`** — the OS-specific writer
  and rebooter — which is the same gated hardware/OS handoff as the OS-keyring `KeyVault`
  ([ADR-0053](0053-cloud-sync-port.md) follow-ups). How the detached signature reaches the box (a
  `<release>` companion fetch via `CloudSync`, or a configuration field) is a composition-layer choice
  the updater does not fix: it takes the signature as an argument.
- **Nothing is foreclosed.** New rollout policy stays in `decide_rollout`; a new verification backend
  stays behind `Signer`; a new install mechanism stays behind `UpdateInstaller`.
