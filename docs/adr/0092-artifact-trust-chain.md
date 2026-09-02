# ADR-0092 — The edge cannot fetch an artifact without its signature, and its trusted keys come only from the build

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-02
**Amends** [ADR-0053](0053-cloud-sync-port.md) (`CloudSync::fetch_update`'s return type) · [ADR-0054](0054-edge-cloud-http-client.md) (the adapter's response shape) · **Relates to** [ADR-0047](0047-minisign-verification.md) (verification, and the keys baked into the binary) · [ADR-0048](0048-ota-rollout-model.md) (the rollout decision this feeds) · [ADR-0055](0055-edge-ota-updater.md) (`OtaUpdater`, which holds both facts) · [ADR-0088](0088-ota-artifact-hosting.md) (the cloud stores the signature beside the artifact; Correction 2 already settled `arch`) · [ADR-0026](0026-port-shapes.md) (the shape a port must take) · [ADR-0003](0003-cattle-not-pets.md) (replace the box) · `docs/roadmap-v3.md` (slice R5, and the gaps its scoping found)

**Context.** `OtaUpdater::install` does the right thing in the right order: fetch the artifact, verify
its detached minisign signature against a trusted key, and only then touch the disk. Verification is
what makes hosting binaries acceptable at all — the cloud is a dumb host, and a compromised or spoofed
one can make an update *fail*, never make a box install code
([ADR-0088](0088-ota-artifact-hosting.md)).

Two of the three things that verification needs have no production source.

**The signature has no producer.** `UpdatePlan` (`crates/pos-edge/src/ota.rs`) carries
`signature: &'a Signature`, and `OtaUpdater::verify` reads its claimed key id, checks it against the
revocation list, selects the matching trusted key, and verifies. But `CloudSync::fetch_update` returns
`Vec<u8>` — the artifact bytes and nothing else — and no other port method offers a `.minisig`. So in
the running binary there is no value to put in that field. The only construction of `UpdatePlan`
anywhere in the tree is `crates/pos-edge/tests/ota.rs`, which builds the signature itself.

**The trusted keys have no source either.** `OtaUpdater::new(cloud, signer, installer, trusted_keys:
Vec<PublicKey>)` takes the keys as a constructor argument, and nothing in production ever builds that
vector. ADR-0047 says the keys are "baked into this binary"; the mechanism to bake them was never
written.

Neither is a live defect, because R5 has not wired `OtaUpdater` into the running edge at all — the
whole updater has zero production callers. That is exactly why they must be settled now rather than
during the wiring slice: R5 is where they would otherwise be discovered, one at a time, under pressure
to just make it compile. A `trusted_keys` vector that is easiest to fill from the config tree is the
kind of shortcut that gets taken at that moment, and it would hand a compromised cloud the ability to
introduce its own signing key — turning the dumb host into a trust boundary and silently deleting the
property ADR-0047 and ADR-0088 were both built to preserve.

**What is *not* in question.** `arch` is already decided:
[ADR-0088](0088-ota-artifact-hosting.md) Correction 2 records that the request body gains an additive
`arch` field which **the adapter fills from its own build target**, because artifacts are one blob per
`(release, architecture)`. That reasoning is the same one this ADR applies to the keys, and it is not
re-opened here.

**Decision.**

- **`CloudSync::fetch_update` returns the artifact and its signature together**, as one
  `SignedArtifact { bytes: Vec<u8>, signature: Signature }` in `pos-ports`, instead of `Vec<u8>`.

  The point is not convenience, it is that **skipping verification stops being expressible**. With
  two independent calls — the artifact from one, the signature from another — a caller can obtain
  30 MB of executable bytes while holding no signature at all, and the type system has no objection.
  Every such call site then relies on a human remembering the rule. Returning the pair means the only
  way to get the bytes is to also be handed the thing that judges them, and a future caller who wants
  to skip the check has to visibly discard it. On a trust boundary that is worth an amendment to an
  existing port method: this is the one place in the system where forgetting a step means installing
  someone else's code.

  It amends [ADR-0053](0053-cloud-sync-port.md) rather than adding a method. `CloudSync` has two
  implementations (`HttpCloudSync` and `pos-fakes`' `FakeCloudSync`), one shared contract suite, and
  no external consumer, so the change is cheap now and gets less cheap every slice.

- **On the wire, the artifact stays the raw body and the signature travels in a response header.**
  `POST /internal/ota/artifact` keeps returning the bytes verbatim; the signature comes back
  hex-encoded (see Correction 1) in `X-Pos-Artifact-Signature`, following the existing
  `X-Pos-Webhook-Signature` naming. The `HttpTransport` seam's `HttpResponse` gains a `headers`
  field to carry it — additive, and local to the one adapter.

  A detached minisign signature is a few hundred bytes; the artifact is tens of megabytes. Wrapping
  both in a JSON object would mean base64-encoding the artifact, costing about a third of its size in
  extra transfer on every download by every store in every ring, to move a payload that would fit in
  a header many times over. That is the wrong trade at fleet scale, and it is the reason the body is
  not JSON today.

- **Trusted keys are compiled in, and there is no runtime path that can supply them.** `pos-edge`
  reads them at build time through `option_env!`, the same mechanism R1b used for the release version
  (`crates/pos-edge/src/version.rs`) — so no build script, no new dependency, and a binary built
  without them says so honestly rather than trusting whatever arrives.

  **Nothing reads a signing key from the cloud-published config tree, and nothing may.** The config
  tree is authored in the cloud and delivered over the network; a key taken from it would be a key an
  attacker who controls the cloud can choose, which makes the signature check a formality that
  verifies the attacker's artifact against the attacker's key. The whole chain rests on the trust
  anchor being outside the channel it protects. This is stated as a prohibition, not a preference,
  because the convenient wrong answer is one line away — the config tree is already parsed, already
  typed, and already sitting in `EdgeSession`.

  A fork replaces the baked-in keys with its own, because the keys are the fork's, not this
  repository's. That is a build-time input, exactly like `POS_EDGE_RELEASE_VERSION`.

**Consequences.**

- **No `PROTOCOL_VERSION` bump.** `/internal` is unversioned, the added header is additive, and no
  event or config payload changes.
- **Two adapters and one contract suite change** with the return type: `cloud-sync-http`'s
  `fetch_update` and `parse_fetch`, `FakeCloudSync`, and the `CloudSync` suite, which gains the
  obligation that a fetched artifact arrives with a signature.
- **The `HttpResponse` seam widens by one field**, so the stub transports in the adapter's own tests
  gain a `headers` value. Additive; nothing existing reads it.
- **A release must publish the signature where the cloud can store it.** R1 already produces the
  `.minisig` files and publishes them to the GitHub Release
  ([ADR-0088](0088-ota-artifact-hosting.md)), and ADR-0088's storage model already puts the signature
  beside the artifact as a second blob. So the supply chain has the artifact; what R2's route slice
  must do is serve it, and this ADR fixes where it goes in the response.
- **A fork that does not set the trusted keys cannot install an update**, which is the correct
  failure: an updater with no trust anchor must refuse, not proceed. The fork checklist gains the
  variable.
- **This does not make R5 reachable on its own.** `POST /internal/ota/artifact` still does not exist
  (ADR-0088's remaining slices, gated on the S3 credentials the deployment does not provision), and
  `DeviceState.last_self_test` is still not persisted across the reboot an install performs. Both are
  named here so this ADR is not mistaken for closing the chain.

**Alternatives considered.**

- **Add `fetch_signature(release)` beside `fetch_update`.** Purely additive — no existing signature
  changes — and rejected for exactly the property this ADR exists to buy: two calls can drift, and a
  caller can make the first without the second. It also introduces a failure mode where 30 MB
  downloads successfully and the signature fetch then fails, wasting the transfer. If the port is
  going to guarantee anything about the trust chain, "you cannot hold the bytes without the
  signature" is the guarantee, and a second method is the shape that cannot express it.
- **Carry the signature in `PublishedUpdate`, through the config tree.** No port change at all, and
  the signature would still be checked against a baked-in key, so it is not *insecure* — a config
  tree cannot forge a signature any more than a cloud can. Rejected on layering and on size: it puts
  a per-release binary blob into a document [ADR-0078](0078-sync-and-ota-closure.md) deliberately
  keeps small, makes the rollout announcement and the artifact's integrity proof travel as one thing
  when they have different lifetimes, and means a signature correction requires republishing config.
- **One JSON response carrying both, artifact base64-encoded.** No seam change and no header
  handling. Rejected on cost: roughly +33 % on every artifact download, fleet-wide, to avoid one
  additive struct field.
- **Read trusted keys from a file on disk beside the binary.** More convenient for a fork than a
  rebuild, and rejected because it moves the trust anchor to the least protected place on the box: a
  file an attacker who reaches the filesystem can replace, on a machine that is deliberately treated
  as cattle ([ADR-0003](0003-cattle-not-pets.md)) and is expected to be re-imaged rather than
  audited. Compiled-in means replacing the anchor requires replacing the binary, and replacing the
  binary requires a signature from the anchor.

**Corrections, made while implementing.** One claim above did not survive contact with the tree and
is corrected here rather than quietly worked around.

1. **"base64-encoded" was wrong, and hex replaces it.** There is no base64 crate anywhere in this
   workspace — checked across every `Cargo.toml` — so honouring the word as written would have meant
   adding a third-party dependency, which `docs/adr/README.md` makes an ADR-first change in its own
   right. An ADR cannot require, as an implementation detail, a step that its own rules say needs
   another ADR.

   Hex needs nothing new: `pos_ports::device_registry::TokenDigest` already hand-rolls the same
   encode/decode pair for exactly this reason, and the adapter now does too. The cost is arithmetic
   — hex is 2 characters per byte against base64's 4-per-3, so a signature of a few hundred bytes
   grows by roughly a third of a kilobyte. Set against an artifact of tens of megabytes that is
   nothing, and it is *not* the comparison the decision above rests on: the point was never
   base64-versus-hex, it was header-versus-body, and encoding the 30 MB body would have cost about a
   third of its size on every download. That argument is untouched.

   The encoding is fixed as **lowercase** hex and the decoder rejects uppercase, so there is one
   spelling on the wire rather than two that happen to work. Header *names* stay case-insensitive,
   as HTTP defines them — a proxy or a fork's stub may send any casing, and reading the name
   case-sensitively would turn every store's fetch into a failure at once.