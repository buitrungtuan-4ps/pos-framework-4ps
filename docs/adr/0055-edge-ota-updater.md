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

---

**Amendment 1 (2026-09-04) — the installer needs no privilege, so the binary moves under the state
directory; and the self-test that decides a rollback happens *after* the restart, not before it.**

Writing the real Linux `UpdateInstaller` (roadmap **R4**, delivered with **R5**) turned up two things
this ADR assumed and the rest of the tree contradicts. Neither is visible from either side alone.

- **The edge cannot write `/usr/local/bin/pos-edge`, and must not be able to.**
  `deploy/edge/pos-edge.service` runs the store binary as an unprivileged `pos` user under
  `ProtectSystem=strict` and `NoNewPrivileges=true`. Under those two settings the entire filesystem
  is read-only to the process except its `StateDirectory`, and no `sudo`, `setuid` helper, or
  `systemctl` call can escalate out of it. So "the shipped binary implements them against the OS
  (write the binary, reboot)" describes something the shipped unit forbids. The tempting fixes —
  loosen the sandbox, add a `polkit` rule, install a root-owned helper — all end with a store
  process that can write outside its own state, which is a strictly worse posture than the one that
  blocked us: a remote-code-execution bug on a till would gain the ability to replace system
  binaries.

  **So the binary moves to where the service already has write access.** `ExecStart` becomes
  `/var/lib/pos-edge/bin/current`, a *symlink* the edge owns, pointing into
  `/var/lib/pos-edge/bin/versions/<version>`. `apply` writes a new version file; `commit` retargets
  the symlink with a single `rename(2)`, which is atomic — there is no instant at which `current`
  names nothing. `/usr/local/bin/pos-edge` remains the operator's copy and the thing `bootstrap`
  installs; it is no longer what runs. The sandbox stays exactly as strict as it is today.

  The cost is real and is accepted: a binary the service can rewrite is a binary an RCE on that
  service can rewrite. That is inherent to any self-update, and it is why
  [ADR-0047](0047-minisign-verification.md)'s verification gates the write rather than following it —
  `apply` is unreachable except with bytes a trusted, unrevoked signing key signed. What the move
  changes is the blast radius when the sandbox is the only thing standing: before, a compromised edge
  could not touch a binary at all; now it can replace the one binary it already *is*. It still cannot
  reach `/usr`, `/etc`, or another service's files.

- **`commit` does not reboot, and the self-test that matters is not the one it gates.** This ADR reads
  `self_test` as *the* verdict: pass → `commit`, fail → `rollback`. But
  [ADR-0048](0048-ota-rollout-model.md)'s highest-precedence rule compares `last_self_test.version`
  with the version the box is **running**, and `crates/pos-edge/src/ota_state.rs` exists precisely
  because that comparison has to survive the restart. Read together, a pre-commit self-test can never
  satisfy the rollback rule: a box that installs 1.4.0, fails a pre-commit test and reverts is still
  running 1.3.0, so a recorded `{1.4.0, failed}` does not match its running version, `decide_rollout`
  reads it as history, and the next tick installs the same bad build. Forever.

  **There are therefore two self-tests, and they answer different questions.**
  1. **The pre-commit smoke test** (`UpdateInstaller::self_test`) asks *can these bytes even run on
     this box* — the staged file is executed as `pos-edge --self-test`, which loads the config, opens
     the store read-only, and exits. It catches the wrong architecture, a truncated download, a
     missing shared library. It is cheap, it happens before anything is swapped, and its failure is
     the routine rollback this ADR already describes.
  2. **The boot confirmation** asks *did this version actually come up* — and it is the one the
     rollback rule reads. `commit` writes an *unconfirmed* marker naming the version it just staged;
     the new binary, once it has parsed its config, migrated and bound its socket, clears the marker
     and records `{running version, passed: true}` through `OtaStateAuthority`. A version that never
     reaches that point never records a pass.

  **And a binary that cannot start cannot decide to revert**, which is the failure the marker exists
  to catch. Each boot with an unconfirmed marker increments an attempt counter beside it; past
  `MAX_UNCONFIRMED_BOOTS` the edge retargets `current` at the previous version, restores the
  `.pre-update` database copy, records `{unconfirmed version, passed: false}` and exits for `systemd`
  to restart it on the binary that worked. The counter can only advance while a version has *never*
  been healthy — a confirmed boot deletes the marker — so an operator restarting a working store
  cannot trip it. This is the half that makes principle 3 ("dễ cập nhật") true in the direction that
  costs money: a bad release heals itself instead of needing somebody to drive to the shop.

**What stays as written.** The seam, its five methods, the fixed order, and the rule that
verification gates the disk are all unchanged. `UpdateInstaller` is still synchronous, which is still
the right trade for a rare one-shot — but note that the smoke test now waits on a child process, so a
tick can block its worker thread for up to `SELF_TEST_TIMEOUT`. The loop runs on its own task and an
install happens once per release per store, so this is bounded and rare exactly as the original
rejection assumed; if a future installer needs longer, that is when the seam goes async.

**What is still not exercised in the pull-request gate.** The `rename(2)` swap, the marker, the
counter and the revert are all ordinary filesystem operations and are tested against a temporary
directory. What CI still cannot prove is that `systemd` restarts into the retargeted symlink, and
that the store comes back trading — the real-box step `docs/gate-register.md` tracks.
