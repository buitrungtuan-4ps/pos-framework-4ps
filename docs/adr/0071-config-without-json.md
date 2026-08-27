# ADR-0071 — Config without JSON: a form-driven capability editor, and an edge that applies the structured nodes

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-27
**Relates to** [ADR-0004](0004-cloud-owned-configuration.md) (cloud-owned config) · [ADR-0033](0033-config-tree.md) (config tree) · [ADR-0039](0039-config-delivery.md) (config delivery) · `pos-spec.md` §10 (capabilities) · `docs/cloud-admin-ux-plan.md` (Track M8)

**Context.** Configuration is cloud-owned (ADR-0004): a store never edits its own capability profile, it
*receives* it. Two gaps sit between that intent and reality.

1. **The edge silently ignores published capability flags.** The config-pull rebuild
   (`session_from_config`, ADR-0033/0039) reconstructs only the `menu` node — and, since M1, the
   `permissions` node. The store's **capability flags** (§10 — `tables_enabled`, `pay_first_enabled`,
   `kds_enabled`, …) are top-level booleans in the effective config document, but the edge never reads
   them into its `CapabilityContext`. So publishing a flag from the console changes nothing on the
   counter: turning off table service, switching a store to pay-first, enabling QR ordering — all
   no-ops. This is a correctness bug, not a missing feature: the cloud *validates* the flags (the
   config tree runs the §10 inter-flag rules on every publish), so an operator reasonably believes a
   published profile takes effect.
2. **The console authors config as raw JSON.** The Config screen publishes a hand-typed JSON document
   per level. Capability flags — the most-changed config — are the worst case for that: an operator
   must know the exact key names and the inter-flag rules by heart, and a typo is a `422` at best or a
   surprising store behaviour at worst.

**Decision.**

- **One shared reader for capability flags, in `pos-core`.** `CapabilityContext::from_flags(is_on)`
  takes a closure answering "is the flag with this key set?" and rebuilds the context from the §10
  catalogue (`Capability::ALL` + each flag's `key`/`default_on`), a flag the source does not mention
  falling to its declared default. It is serde-free — the caller supplies the lookup over its own JSON
  — so it lives in pure `pos-core` and **both** the cloud validator and the edge runtime read a
  published profile through it. The cloud's `capability_context` (config-tree validation) is refactored
  onto it; the edge calls it too. The invariant the repo prizes holds: the cloud and the edge cannot
  disagree on what a config document *means*.
- **The edge applies the capability flags on every config pull.** `session_from_config` gains a
  `capabilities` branch: if the pulled document carries **at least one** known flag key, the session's
  `CapabilityContext` is rebuilt from the document (an unnamed flag falls to its default — the profile
  is authoritative once it names any flag); if it carries **none**, the base profile is left unchanged.
  That gate preserves the same **never-blank** contract the `menu` and `permissions` branches keep — a
  publish that says nothing about capabilities never resets a trading store's profile — while making a
  publish that *does* name flags take effect. Flags are top-level keys (not a nested node), matching
  the document the validator already reads, so no new node or `PROTOCOL_VERSION` change is needed.
- **A form-driven capability editor in the console** (later M8 slices). The Config screen offers the
  §10 catalogue as labelled toggles with the three presets (full-service / counter / retail), previews
  the §10 inter-flag conflicts inline before publish (the same `conflicts` rules the cloud enforces, so
  the operator sees a violation the moment they create it, not as a `422`), and shows a diff of the
  resulting flags against the current effective profile before publishing. Publishing merges the flag
  booleans into the store's config layer (the node-merge the catalog/people publishes use), so it never
  clobbers the other keys on that level. Raw-JSON publish stays for everything a form does not yet
  cover.

**Consequences.** Publishing a capability profile now changes the store — the silent no-op is fixed,
and the fix is one shared reader rather than a second copy of the flag rules on the edge. The console
stops demanding JSON for the most-edited config and shows conflicts before, not after, a publish.
Additive throughout: no schema change, no new config node, no `PROTOCOL_VERSION` bump (the flags are
already in the effective document; the edge simply starts reading them). Tax-rate overrides per store
are a parallel structured node the same machinery can carry later; this ADR scopes the capability
flags, which are the live correctness gap.

**Slices (Track M8, one PR).**

1. **Edge applies capabilities (this):** the shared `CapabilityContext::from_flags` reader in
   `pos-core`, the cloud validator refactored onto it, the edge `session_from_config` capabilities
   branch, an `EdgeSession::with_capabilities` builder, and tests.
2. **Capability catalogue + presets + conflict-preview API:** the cloud serves the §10 catalogue
   (key / description / default), the presets, and the inter-flag rules for the console's form editor.
3. **Form-driven capability editor:** the Config screen's toggles + presets + inline conflict preview
   + diff-before-publish, publishing via a layer-merge so other keys survive.
