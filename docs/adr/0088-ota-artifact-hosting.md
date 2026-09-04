# ADR-0088 — The cloud hosts the update artifact, and stays a dumb host

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-04
**Relates to** [ADR-0003](0003-cattle-not-pets.md) (replace the box, don't nurse it) · [ADR-0037](0037-api-keys.md) (the scoped store key) · [ADR-0044](0044-fork-and-deploy.md) (what runs on the VPS) · [ADR-0047](0047-minisign-verification.md) (the edge verifies the signature, always) · [ADR-0048](0048-ota-rollout-model.md) (rings, revocation, the rollout decision) · [ADR-0053](0053-cloud-sync-port.md) (`CloudSync::fetch_update`) · [ADR-0054](0054-edge-cloud-http-client.md) (the adapter that calls it) · [ADR-0078](0078-sync-and-ota-closure.md) (`report()` and the rollout progress model) · `docs/release-runbook.md` (R1: how an artifact is built and signed) · [ADR-0097](0097-internal-route-authentication.md) (why `/internal` is private-network-only) · `docs/roadmap-v3.md` (roadmap v3, slice R2)

**Context.** The over-the-air path is built at both ends and joined in the middle by nothing.

The **edge** end is complete: `OtaUpdater` decides against the published rollout ([ADR-0048](0048-ota-rollout-model.md)), calls `CloudSync::fetch_update(release)` for the bytes, verifies the detached minisign signature against its trusted keys before staging anything ([ADR-0047](0047-minisign-verification.md)), runs the self-test, and rolls back on failure. The `cloud-sync-http` adapter implements that call as `POST /internal/ota/artifact` with a release tag. The **cloud** end publishes the rollout through the config tree and ingests the outcome at `/internal/ota/report` (O3). **`/internal/ota/artifact` does not exist.** A store that decides it should update asks for the bytes and gets a 404: the OTA path is dark at exactly the place the order relay and the outbox were before E3.

R1 fixed the other half of the supply chain: the release workflow cross-compiles for both store architectures, signs every artifact with minisign — keys in GitHub secrets, never on a VPS (debate D1) — and publishes the artifacts and their `.minisig` files to a GitHub Release. So signed artifacts exist; nothing serves them to a store.

Two things make this more than "add a route". First, hosting binaries is a **new responsibility** for the cloud, with a storage cost and a bandwidth profile (a fleet of a thousand boxes fetching a 30 MB binary in a ring is 30 GB) that the single-VPS deployment ([ADR-0044](0044-fork-and-deploy.md)) has to absorb. Second, an endpoint that serves executable bytes is a **security surface**, and the tree's rule is that a transport is not a trust boundary — so the question is not "how do we make the download trustworthy" but "what is the blast radius when it is not".

**Decision.**

- **The cloud stores artifacts in Garage over the existing `BlobStore` port.** No new infrastructure: `blob-garage` is in the tree and Garage is already deployed and running on the box ([ADR-0044](0044-fork-and-deploy.md)). An artifact is one blob per (release, architecture), keyed by a stable, path-safe convention, with its detached signature stored beside it as a second blob. Postgres holds the small row that says a release exists and which blobs it points at; the bytes never go in the database.

- **`POST /internal/ota/artifact` serves the bytes, exactly as the edge already asks for them.** The route the `cloud-sync-http` adapter calls today is the route the cloud grows. No wire change, no adapter change to the request shape, no `PROTOCOL_VERSION` bump — the reason the edge's OTA path can go from dark to live without touching `pos-proto`.

- **The artifact route requires the store's scoped key.** It joins the store-facing family and authenticates like the rest of it ([ADR-0037](0037-api-keys.md)): the box already holds a key for config-pull, heartbeat, and the relay, so this costs no new provisioning. It is *not* about keeping the binary secret — it is signed, and secrecy is not what protects it — but about not running an open binary-distribution host on the VPS that anyone can point a downloader at. Which scope it takes is settled below.

- **The cloud never signs, and never becomes a trust boundary.** It stores bytes an operator uploaded and hands them back. The edge verifies the minisign signature against its own trusted keys before staging ([ADR-0047](0047-minisign-verification.md)), so a compromised cloud, a swapped blob, or a spoofed host can make an update *fail*, never make a box install code. That is what "the cloud stays a dumb host" means in the roadmap, and it is the property that makes hosting binaries an acceptable thing for the cloud to do at all.

- **Uploading is an admin action, and so is promoting.** `POST /admin/ota/releases` takes the artifact and its `.minisig` (the pair R1's workflow produced) and records the release; promote-release moves it through the rings the rollout model already defines ([ADR-0048](0048-ota-rollout-model.md)), over the config-tree publish that already exists. Both are audited like every other `/admin` write (G2). The operator's job is to move a *signed* pair from the release to the cloud; the cloud's job is to remember and serve it.

- **A release is immutable once uploaded.** Re-uploading the same release tag with different bytes is refused, not overwritten. An artifact a ring has already installed must keep meaning the same thing, or a rollback target stops being a known quantity — and silently redefining a version is how a fleet ends up in a state nobody can describe.

**Deliberately deferred (flagged, not silently dropped).**

- **Streaming rather than buffering.** The `BlobStore` port is `get(&BlobKey) -> Vec<u8>`, so serving a 30 MB artifact holds it in memory for the length of the response. That is tolerable for a ring of a few boxes and wrong for a fleet-wide push; a streaming `get` is a port change with its own contract-suite work and belongs to the performance wave, not to making the path exist. The first implementation buffers, and says so at the call site.
- **Which scope the route takes.** `read_config` is what every provisioned box already carries, so requiring it makes the path work with no re-provisioning; a dedicated `fetch_update` scope is cleaner and is what a fleet with untrusted stores would want. This ADR requires *a* valid store key and leaves the exact scope to the implementation slice, which will state its choice in the changelog — because the answer depends on whether we are willing to re-issue keys, which is an operational question, not an architectural one.
- **The adapter's bearer.** `cloud-sync-http`'s transport sends no `Authorization` header today (activation is deliberately unauthenticated, and it was the only caller). Giving it the store key for this one call is part of the implementation slice.
- **Bandwidth and egress.** A ring of a thousand boxes pulling 30 MB is 30 GB off one VPS. Rings already stagger that ([ADR-0048](0048-ota-rollout-model.md)); a CDN or a peer-to-peer tier is a real answer if the fleet outgrows the staggering, and is not this slice.
- **Garbage-collecting old artifacts.** Releases accumulate. A retention rule (keep the current ring targets and the last known-good rollback target) belongs with the retention runner that already exists, once there is more than one release to collect.

**Rejected.**

- **Pointing the edge at GitHub Releases.** Tempting — the artifacts are already there, and it costs no storage. Rejected: it makes every store's update path depend on a third party being reachable from the shop's network, which is exactly the dependency ADR-0001 spends the rest of the system avoiding; it leaks the fleet's update cadence to an outside observer; and it puts a URL the cloud does not control on the path that installs code. The cloud already has to be reachable for the store to know an update exists at all, so serving the bytes adds no new dependency.
- **Serving the artifact unauthenticated.** The bytes are signed, so this is not a confidentiality failure — but it turns the VPS into an open download host, and the cost of avoiding it is zero because the box already holds a key.
- **Having the cloud verify (or re-sign) the artifact.** Rejected twice over: verification at the cloud proves nothing to the edge (the edge must verify anyway, or the cloud becomes a trust boundary), and signing at the cloud would put a signing key on the VPS, which debate D1 settled — keys never touch a VPS.
- **Storing artifacts in Postgres.** A 30 MB blob per release per architecture in the transactional database, backed up in every WAL archive, for data that is immutable and content-addressable. Garage exists for exactly this.
- **A new port for artifact storage.** `BlobStore` is already the tree's "bytes by key" seam and `blob-garage` already passes its contract suite. A second seam for the same shape would be a port with one implementor and no distinct contract.
- **Mutable releases.** Covered above: a version that can change under a fleet is not a version.

**Consequences.**

- **The OTA path stops being dark.** With this slice a store that decides to update can actually fetch the artifact, verify it, install it, self-test, and report the outcome — every one of those already built, and the chain finally joined. It is the third "written but never wired" gap this program has closed, after the order relay and the outbox.
- **No wire, protocol, or `pos-proto` change.** The route is the one the adapter already calls; the rollout model, the report ingest, and the signature verification are as ADR-0047/0048/0078 built them. Additive routes and one Postgres table.
- **No new dependency.** `blob-garage` and the `BlobStore` port are in the tree; Garage is already deployed.
- **The VPS gains a storage and bandwidth cost** proportional to release size × architectures × retained releases, and a per-ring egress spike. Both are bounded by the deferred retention rule and by rings, and both are named here so the first operator to see the disk graph knows why.
- **An operator gains one step in the release runbook**: move the signed pair from the GitHub Release to the cloud, then promote. The runbook is updated in the implementation slice.
- **Delivery shape.** This ADR is PR A. The implementation follows as: the release registry, then artifact storage + the `/internal/ota/artifact` route + the adapter's bearer (so an edge can fetch), then the `/admin` upload and promote-release with their audit entries and the runbook update.

**Corrections, made while implementing (R2, the release registry).** Two claims above were checked against the tree during the first implementation slice and did not survive. They are corrected here rather than quietly worked around, because both change what the remaining slices have to do.

1. **"Garage is already deployed *for media*" was wrong.** Garage runs on the box, but for backups and WAL shipping; media renditions live in a Postgres `bytea` table (`media_assets`, migration `0030`) — [ADR-0042](0042-image-pipeline.md)/[ADR-0031](0031-cloud-adapter-transports.md) deliberately chose that over the condemned `blob-garage` port, and `pos-cloud` does not depend on `blob-garage` today. The decision above is unaffected — a ≤150 KB rendition and a 30 MB binary are genuinely different calls, and the size argument for keeping artifacts out of the database stands — but "no new dependency" was too strong: the artifact-storage slice has to add `blob-garage` to `pos-cloud`, and with it S3 credentials the deployment does not currently provision (`bootstrap.sh` writes `garage.toml`; the access keys are minted at runtime with `garage key create`). That plumbing — env vars, a bucket, the runbook step — belongs to that slice and is named here so it is not discovered as a surprise.
2. **The wire request carries no architecture.** [ADR-0054](0054-edge-cloud-http-client.md) pinned `fetch_update` as `POST /internal/ota/artifact` with a body of `{"release": "…"}`, and R1's workflow builds *two* targets (`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`). So "one blob per (release, architecture)" is right, but the route as pinned cannot say *which* architecture it is being asked for. The registry slice keys artifacts by `(release, target)` accordingly; the route slice must add `arch` to the request body as an **additive** field the adapter fills from its own build target. That is a change to ADR-0054's request shape — still no `PROTOCOL_VERSION` bump, since `/internal` is unversioned and the field is additive, but it is a wire change, and the claim above that there is "no adapter change to the request shape" was wrong.

**Amendment 1 (2026-09-04) — the artifact route moves to `/sync`, because `/internal` is unreachable by a store.**

The decision above says the artifact route "requires the store's scoped key" and "joins the store-facing
family". [ADR-0054](0054-edge-cloud-http-client.md) pinned that route as `POST /internal/ota/artifact`,
describing `/internal/` as "the store-facing surface, not the public `/v1`".

**That description stopped being true, and nobody reconciled the two.** The proxy now denies the whole
prefix:

```
handle /internal/* {
	respond 404
}
```

`deploy/Caddyfile.d/site.caddy`, mirrored in `k8s/pos-cloud.yaml`. It is the enforcement of a real hole
— `/internal/ingest`, `/internal/reconcile` and `/internal/ota/report` were reachable and
unauthenticated from the internet — and it is deliberate: `/internal/*` is the cloud's own
**trusted-network** surface, private-network-only by design, answering `404` rather than `403` so an
unauthenticated caller learns nothing.

A store dials its cloud at `cloud_url`, which is the public hostname, through that proxy. So the pinned
path is **unreachable by the one caller it exists for**. Building the handler there would have produced
a route that 404s exactly as it does today — written, tested, and unreachable, which is the failure
`docs/roadmap-v3.md` indicts this program for seven times.

The deny's own comment already names the alternative, and [ADR-0097](0097-internal-route-authentication.md)
reached the same conclusion independently for the sibling report route, recording that it "moves to
`/sync/stores/{store_id}/…` when it gains a real caller". Neither was carried across to the artifact
route. This amendment carries it.

- **The route is `POST /sync/stores/{store_id}/artifact`.** The store-facing family, where the cloud
  resolves the tenant from the scoped key rather than trusting a body field, and where the config-pull,
  heartbeat, device and relay routes already live. `/internal` keeps what it is for: the cloud's own
  trusted-network tooling.

- **It requires the `read_config` scope** — the owner's call, taken 2026-09-04, on the question this ADR
  deferred as "an operational question, not an architectural one". Every provisioned box already carries
  `read_config`, so the OTA path works with **no re-provisioning**: no revisiting each live store to
  rewrite its mode-0600 env file, which at a hundred stores is a hundred site visits before a single
  update can ship. The cost accepted is that `read_config` now also authorises downloading a release
  artifact — a scope slightly broader than its name. That is bounded by what the scope is *for*: not
  secrecy (the artifact is signed, and the edge verifies it against a build-baked anchor,
  [ADR-0047](0047-minisign-verification.md)/[ADR-0092](0092-artifact-trust-chain.md)), but not running an
  open binary-distribution host on the VPS. A dedicated `fetch_update` scope stays the cleaner shape and
  the thing to reach for if a fork ever hosts stores it does not trust.

- **The request body keeps `release` and gains `arch`**, exactly as Correction 2 said. Moving the path
  does not change the body.

- **Still no `PROTOCOL_VERSION` bump.** `/sync` is unversioned like `/internal`, the field is additive,
  and the route has no existing server to break. The only client is `cloud-sync-http`, shipped in the
  same binary as the updater that calls it.

**What this costs.** ADR-0054's pinned path was wrong and its contract suite pins the wrong wire; both
change in the implementation slice. That is the price of a pin written before the surface it names was
given a meaning — and the reason this amendment exists rather than a quiet edit is that the *next* route
someone adds under `/internal` for a store to call will fail the same way. The rule, stated once: **a
route a store calls belongs on `/sync`; `/internal` is for callers on the box's own network.**

---

**Amendment 2 (2026-09-04) — what gets signed, and what the release is called.**

Building the `/admin` upload half turned up two mismatches between what this ADR assumed and what the
rest of the tree actually does. Both would have produced a fleet that fails at the last step, and
neither is visible from either side alone — which is why they are recorded here rather than fixed
quietly.

- **The signature must cover exactly the bytes the edge writes.** The release workflow signs
  `pos-edge-<tag>-<target>.tar.gz`; `UpdateInstaller::apply` takes `&[u8]` and, by its own contract,
  "writes the verified artifact as the next binary" — a bare executable. So the two halves of the
  trust chain were describing different files. Unpacking the tarball on the upload path (the tempting
  fix) is the one option that must not be taken: the bytes that reach `apply` would then be bytes
  nobody signed, and [ADR-0047](0047-minisign-verification.md)'s guarantee would end at the `tar`
  call. Instead the workflow now stages and signs the **bare binary** as a third asset per target
  (`pos-edge-<tag>-<target>.bin`), and that is the pair the upload carries. The tarball keeps its own
  signature and its own consumer — a human, and R3's installer — and is not what OTA reads.

- **A release has one name, not three.** This ADR called the registry key "the release tag, as the
  workflow cut it (e.g. `v1.2.3`)". But R1b makes the binary report `1.2.3` (the workflow strips the
  `v`, so that a running store's version is comparable with a rollout's), and a rollout's
  `target_version` is bare for the same reason. Three spellings of one release, with a `404` as the
  only symptom when they disagree — and a `404` on this route means "install nothing", so the fleet
  would sit at the old version with nothing in any log saying why. **The registry is keyed by the
  same bare string that appears in `target_version` and in the binary's own `version`.**
  `validate_release_tag` already accepts it (`1.2.3` is alphanumerics and dots); no mapping function
  exists anywhere, deliberately, because a mapping is a fourth place for the spelling to drift. The
  uploader passes the string it will promote.

**And the promote step gets a guard.** `PUT /admin/config/ota` publishes a rollout naming a
`target_version`. Nothing checked that the cloud actually hosts that version, so the console's
happy path was: promote a typo, watch every store in the ring fetch, `404`, and stay put. The route
now refuses a `target_version` with no recorded artifact, naming the field and listing what is
hosted. This is the whole of "promote" that did not already exist — the publish, the audit and the
kill switch shipped with O3 ([ADR-0078](0078-sync-and-ota-closure.md)); what was missing was the
refusal.

The guard reads the registry, not the object store, so it works on a deployment with no `[artifacts]`
block configured — such a deployment simply cannot upload, and therefore cannot promote, which is the
correct posture for a cloud that ships no edge releases.
