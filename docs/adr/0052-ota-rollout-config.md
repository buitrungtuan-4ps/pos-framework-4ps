# ADR-0052 — The OTA rollout is published as configuration, validated by shared rules

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-02
**Relates to** [ADR-0004](0004-cloud-owned-configuration.md) · [ADR-0033](0033-config-tree.md) · [ADR-0047](0047-minisign-verification.md) · [ADR-0048](0048-ota-rollout-model.md) · `docs/roadmap.md` P9

**Context.** [ADR-0048](0048-ota-rollout-model.md) made the rollout *decision* pure:
`pos_core::ota::decide_rollout` takes a `PublishedUpdate` (target, ring gate, ramp, kill switch,
signing key id), the device's ring and canary bucket, and a revoked-key list — all as data. P9e needs
the cloud to actually *publish* that data for the fleet, and the edge to consume it. All configuration
is cloud-owned and reaches a store through the four-level config tree
([ADR-0004](0004-cloud-owned-configuration.md), [ADR-0033](0033-config-tree.md)); the open questions
are where the rollout data lives, who validates it, and whether it warrants a new delivery mechanism.

**Decision.**

- **The rollout is configuration, not a new channel.** It rides the existing config tree as two keys: a
  fleet-wide **`fleet_update`** (`target_version`, `min_ring`, `rollout_percent`, `halted`,
  `signing_key_id`, `revoked_key_ids`), set at the tenant or brand level, and a per-device
  **`device_ota`** (`ring`, `canary_bucket`), set at the device level. The cloud publishes a config
  version and each store *pulls* it, exactly like every other setting
  ([ADR-0033](0033-config-tree.md)) — no push, no new port, no second delivery path. Ramping a percent,
  pulling the kill switch, or revoking a key is a config publish, so it needs no redeploy.

- **The schema and its rules live in `pos-core`, shared by both sides.**
  `pos_core::ota::FleetUpdateConfig` and `DeviceOtaConfig` are typed views of the two keys, and their
  `validate` methods parse them into the `PublishedUpdate`, revoked-key list, and `(Ring, bucket)` that
  `decide_rollout` consumes. The **cloud** runs them before publishing — rejecting an incoherent
  version — and the **edge** runs them before trusting a pulled version. One rule set, so the two can
  never disagree about what a legal rollout is; this is the discipline
  [ADR-0033](0033-config-tree.md) already applies to the §10 capability rules.

- **Validation denies with every reason at once.** The cloud's config validator now also checks
  `fleet_update` and `device_ota` when a document sets them, and returns the whole list of violations
  (a bad target version, an unknown ring, a ramp past 100, a malformed key id), so an operator fixing a
  rejected rollout sees all of them, not one per attempt.

- **An absent key means no rollout configured, not a violation.** A store whose effective document sets
  no `fleet_update` simply has nothing to install; the edge treats a missing key as "no update", never
  an error.

**Rejected.**

- **A dedicated OTA push channel or a new port** — rejected: the config tree already delivers
  cloud-owned state to stores with deltas, snapshots, and last-known-good
  ([ADR-0004](0004-cloud-owned-configuration.md), [ADR-0033](0033-config-tree.md)); a second mechanism
  would duplicate all of it and fight the fixed sixteen-port list ([ADR-0021](0021-corrected-port-list.md))
  for no gain.
- **Validating the rollout on only one side** — rejected: divergent rules are exactly how the cloud
  publishes something the edge then refuses. Shared `pos-core` rules keep the two identical, as with the
  capability rules.
- **Encoding the rollout in the binary or an environment variable** — rejected: it must change without a
  redeploy, which is what the config tree is for.

**Consequences.**

- `pos-core::ota` gains `FleetUpdateConfig`, `DeviceOtaConfig`, `FleetRollout`, `DeviceOtaAssignment`,
  `Ring::from_wire`/`as_wire`, `ReleaseVersion::parse`, and `parse_signing_key_id` — pure, tested, and
  with no new dependency (serde's derive only). The cloud's `CapabilityValidator` rejects an incoherent
  `fleet_update` or `device_ota` on publish.
- Deliberately elsewhere (P9e-4): the edge updater that reads these keys from its pulled config, feeds
  `decide_rollout`, verifies the artifact ([ADR-0047](0047-minisign-verification.md)), takes the
  `.pre-update` database copy, self-tests, and rolls back. Fetching the update *artifact bytes* is
  distinct from this rollout config and rides the edge→cloud request/response transport that is still
  to be decided.

**Corrections, made while wiring the two keys end to end.** Two claims above did not survive contact
with the tree and are corrected here rather than quietly worked around.

1. **"Set at the device level" promises a granularity the delivery mechanism does not have.** A
   `ConfigTree` is keyed by `StoreId` ([ADR-0033](0033-config-tree.md)) and its Device layer is *one*
   document the store's terminals share — there is no per-terminal tree, and the config pull is not
   terminal-scoped. So `device_ota` on the Device layer places every device in the store in the same
   ring at the same bucket, however many terminals the store runs.

   The wording is corrected rather than the mechanism, because per-store is the granularity a shop
   actually wants: a counter running two releases at once is a worse failure than a counter a week
   behind, and the canary ramp does its job at store granularity — a 10 % ramp reaches a tenth of the
   *stores*, which is the unit an operator watches and rolls back. Making the tree per-terminal would
   be a large change to ADR-0033 for a property nobody asked for. `PUT /admin/config/ota/placement`
   therefore authors one placement per store, and the domain's per-device `DeviceState` is filled
   from it — every terminal in the store reading the same ring and bucket.

   Similarly, `fleet_update` is described above as "set at the tenant or brand level" and the lever
   writes it to the **Store** layer. That is not a divergence with any effect: a tree is per store, so
   its Tenant layer is no more fleet-wide than its Store layer — "fleet-wide" describes the operator's
   intent, not one document reaching many stores. Publishing a rollout to N stores is N publishes
   either way.

2. **"The edge runs them before trusting a pulled version" was true of the rules and false of the
   edge.** `FleetUpdateConfig` and `DeviceOtaConfig` were written, tested, and run by the cloud
   validator on publish, and the edge never read either key: `session_from_config` had no branch for
   them, so `EdgeSession` carried no rollout and no placement, and `decide_rollout` — pure, total, and
   fully tested — had no production caller with anything to decide about. The shared-rules discipline
   this ADR is built on was only ever exercised on one side.

   The edge now reads both nodes into `EdgeSession::fleet_update` and `EdgeSession::device_ota`
   through those same `validate` methods. Two consequences of the never-blank rule are worth stating,
   because they cut the opposite way from the other config nodes:

   - **An absent or invalid node leaves the previous value**, as everywhere else in
     `session_from_config` — and here that is load-bearing in a way it is not for a menu. A store that
     *lost* its rollout or its placement would become eligible for nothing, so one bad publish would
     strand a fleet off security fixes. Halting a rollout is `halted` inside the node, never a
     deletion, so stopping the fleet never depends on a delete arriving.
   - **`device_ota` is an `Option` with no default ring**, because every default is wrong in the
     dangerous direction. `Ring::Lab` reads like the cautious choice and is the least cautious one
     available: lab is the first ring a rollout opens to and `decide_rollout` exempts it from the
     canary ramp entirely, so a Lab default installs at the stage where an update has been proven on
     nothing. `Ring::Fleet` at bucket 0 is the first fleet device in, at any ramp above zero. An
     unplaced store installing nothing is the safe end of that trade, and the console says so rather
     than leaving the operator to notice a store that never updates.

   What is still missing is named in `docs/roadmap-v3.md` rather than closed here:
   `DeviceState.last_self_test` has no durable home across the reboot an install performs, so the
   rollback arm of `decide_rollout` cannot yet fire from real state, and `POST /internal/ota/artifact`
   does not exist ([ADR-0088](0088-ota-artifact-hosting.md)). This correction closes the *decision*
   inputs, not the install path.
