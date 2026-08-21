# ADR-0052 — The OTA rollout is published as configuration, validated by shared rules

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-21
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
