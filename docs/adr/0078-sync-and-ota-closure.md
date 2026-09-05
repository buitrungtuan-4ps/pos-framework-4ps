# ADR-0078 — Sync & OTA closure: the cloud learns what each store is running, and gets first-class levers instead of hand-edited JSON

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-02
**Relates to** [ADR-0053](0053-cloud-sync-port.md) (the `CloudSync` port this extends with `report()`) · [ADR-0048](0048-ota-rollout-model.md) (the rollout model whose progress the cloud now observes) · [ADR-0052](0052-ota-rollout-config.md) (the `fleet_update`/`device_ota` config nodes the OTA levers publish) · [ADR-0055](0055-edge-ota-updater.md) (the edge updater that reports its outcome; its binary composition stays the flagged hardware gate) · [ADR-0040](0040-reconciliation.md) (the reconcile endpoint whose runs this records) · [ADR-0068](0068-fleet-liveness.md) (the fleet read model the report feeds) · `docs/cloud-admin-ux-plan.md` (Track O3)

**Context.** The fleet subsystems are built but the loop is open — the cloud publishes and the edge acts, and the cloud never learns the result.

1. **The cloud cannot see rollout progress.** OTA is published as config (`fleet_update`/`device_ota`,
   ADR-0052) and each store pulls it; the edge updater (ADR-0055) decides, verifies, installs, self-tests,
   and rolls back — and then tells no one. `CloudSync` (ADR-0053) carries `activate` and `fetch_update`
   only. The config-pull already records *which config version* a store holds (ADR-0068), but not which
   *binary* it is running or whether that binary passed its self-test. So a rollout is a publish into
   silence: an operator cannot answer "how many stores are on the new version, and did any fail?".
2. **OTA is published by hand-editing JSON.** There is no OTA admin lever. A rollout — target version,
   minimum ring, ramp percent, kill switch, revoked keys — is authored by PUTting a raw `fleet_update`
   node through the generic config-publish route. Halting a bad rollout means hand-editing `halted: true`.
   This is exactly the kind of raw-JSON operation the console overhaul set out to remove.
3. **Reconciliation leaves no trail.** The `POST /internal/reconcile` diff endpoint (ADR-0040) answers
   "which of these ids am I missing?" statelessly. Nothing records that a reconciliation ran, for which
   store, or how much it found — so a gap that reconciliation closed is invisible after the fact.
4. **Levers have no buttons.** Rollup-reset and similar operational levers exist only as routes; an
   operator reaches them with `curl`, not the console.

**Decision.** Close the reporting half of the loop and put the levers behind buttons, without depending
on the parts that need a real box.

1. **Extend `CloudSync` with `report()`** (this ADR, slice 1). One new method, in the port's existing
   style — a port-local `UpdateReport { tenant, store, installed, self_test_passed }`, the same kind of
   small owned struct as `ActivationGrant`, carrying only ids already named by the port plus the two
   facts the cloud needs. The edge tells the cloud the version now running and whether the post-install
   self-test passed; a report never changes what the edge runs. The `HttpCloudSync` adapter posts it to
   `POST /internal/ota/report` (a trusted-network `/internal` route carrying `tenant_id`/`store_id` in the
   body, exactly like `/internal/reconcile`); the fake accepts it; the shared contract suite gains a case.
2. **The cloud ingests reports into an OTA-progress read model** (slice 2). A new endpoint persists each
   report; the fleet read model gains the installed version and last self-test outcome, so `GET /admin/ota`
   can show, per store: the version published for its ring, the version it is actually running, and
   whether its self-test passed — the ring progress that was invisible.
3. **First-class OTA publish + kill-switch** (slice 3). Admin routes compose the `fleet_update` node
   through the config tree (reusing the `ota_violations` validation that already guards it) — a publish
   route that authors the rollout from typed fields, and a halt/resume route that flips the kill switch —
   so no one hand-edits JSON. Behind a new `console.ota.publish` permission, audited. Rollup-reset and the
   other existing levers get buttons in the same pass.
4. **Reconciliation gains a run history** (slice 4–5). Every `/internal/reconcile` call records a run
   (store, candidates offered, missing found, when); `GET /admin/reconcile` lists them, with a manual
   "run now" trigger. The console finally shows that reconciliation happened and what it caught.

**Deliberately deferred (flagged, not silently dropped).** These need a real box or are large enough to be
their own track, and the repo already treats them as hardware/composition gates:

- **Running the edge OTA updater and the reconcile caller inside the shipped `pos_edge` binary.** The
  updater (ADR-0055) and a nightly reconcile manifest-sender are store-side fleet wiring that ADR-0055
  §Consequences and `docs/roadmap.md` P9 already hold behind the real `UpdateInstaller` (writing a binary,
  rebooting) and a real box. This track delivers and tests everything up to that seam — the `report()`
  port, its adapter, the cloud ingest, the read model, the levers, and the run history — so composing the
  binary is a wiring step against surfaces that already exist and are green, not new design.
- **The `/internal/ota/artifact` *server*.** The client and its pinned contract exist (ADR-0054); the
  server that streams a signed artifact needs a real artifact store and is out of this track's scope.
- **The remote last-30-minutes log tail over NATS.** `MessageLink` is deliberately outbound-only
  (ADR-0031/0053) and request/response over `link-nats` was rejected once already (ADR-0054 §Rejected);
  a log tail is a net-new subject + request-reply mechanism plus an edge-side log ring buffer, and lands
  as a flagged follow-up rather than being rushed into this track.

**Consequences.**

- `CloudSync` grows from two methods to three; its fake, the `HttpCloudSync` adapter, the shared contract
  suite (a fifth case), and the test stubs move together, as the port contract requires. No `Dyn` mirror
  (compile-time selected, ADR-0013), unchanged.
- The report path is additive: a store that never reports simply has no installed-version fact, and the
  fleet read model shows it as unknown — exactly as before this ADR. No protocol-version bump (the
  `/internal/ota/report` route and the read-model columns are additive).
- The OTA levers do not introduce a new delivery mechanism — they author the same `fleet_update` node the
  edge already reads, through the same config tree and the same `ota_violations` validation, so the cloud
  and edge cannot disagree about what a legal rollout is.
- A report is device/store telemetry, not personal data: it carries a version string and a boolean, never
  a customer identifier. Reports and reconciliation runs are operational records, kept out of the T1/T2
  reproduction rules.

**Delivery (2026-08-28).** Slices 1–5 shipped: `CloudSync::report()` with its adapter, fake, and a
fifth contract case; the OTA-progress read model (`/internal/ota/report`, migration `0035`, the fleet
read's installed-version/self-test columns); the first-class OTA publish/kill-switch levers behind
`console.ota.publish` (`/admin/config/ota` + `/halt`) and the console OTA-updates screen; and
reconciliation run history (migration `0036`, `/internal/reconcile` records a run, `GET /admin/reconcile`
lists them) with the console Reconciliation screen and the rollup-**rebuild** button that gives the
existing `rollups/reset` lever a home in the UI. Three items from *Deliberately deferred* remain
flagged and unshipped, exactly as scoped: composing the edge OTA updater and a scheduled reconcile
manifest-sender into the shipped `pos_edge` binary (the ADR-0055 hardware gate), the
`/internal/ota/artifact` **server**, and the remote last-30-minutes log tail over NATS (a net-new
request-reply over the outbound-only `MessageLink`, rejected once in ADR-0054 — a follow-up track, not
this one).

**Amendment 1 (2026-09-02) — a report says "no self-test yet" instead of guessing.**
`UpdateReport.self_test_passed` becomes `Option<bool>`, and the `/internal/ota/report` field becomes
optional (`#[serde(default)]`, so an omitted field reads as `None`).

Slice 1 above shaped `report()` around the case it was written for — a store that has *just installed
and self-tested* — and a `bool` says everything that case needs. Wiring the reporting loop exposed the
case it cannot express. The loop's primary value is telling the cloud **which binary each store is
running**, which is useful on every store from its first boot, long before any store has taken an
update. For those stores there is no verdict, and a `bool` forces the edge to invent one:

- `true` claims a self-test that never ran, and the console shows a green **Passed** badge earned by
  nothing.
- `false` claims a failure that never happened, and every healthy store in a fresh fleet shows red —
  which trains an operator to ignore the column that exists to warn them.

The alternative — report *only* once a self-test exists — keeps the wire honest and loses the point:
the installed-version column would stay empty for every store until it OTAs once, so the fleet view
would be blank in exactly the state an operator most wants it (before the first rollout, checking what
is out there).

The read model was already right and unreachable, which is what makes this an amendment rather than a
new decision. `pos_cloud::fleet::FleetStore.self_test_ok` is `Option<bool>`, the `store-postgres`
column is nullable, and the console already renders `null` as **Not reported**. But that `None` was
only ever reachable for a store that had never reported *at all* — the wire could not carry "reported,
and no self-test". So the three-state model existed end to end except in the one place that produces
the value, and the display had a state nothing could put it in.

**Additive, and no `PROTOCOL_VERSION` bump.** `/internal` is unversioned; the field becomes optional
rather than changing type on the wire, so an edge built before this amendment posts the same body and
its `true`/`false` is read unchanged. `record_report` takes `Option<bool>` and writes SQL `NULL` for
`None`, which the column already accepts. The console needs no change: it renders the `null` it was
already written to expect.

**What this does not settle.** The reporting loop itself is still the next slice, and the install arm
stays where *Deliberately deferred* left it — `UpdateInstaller` has no production implementation (the
ADR-0055 hardware gate) and `POST /internal/ota/artifact` does not exist. This amendment only makes it
possible for the loop to be truthful when it lands.

**Amendment 2 (2026-09-05) — a report no longer carries a tenant, because nothing ever read it.**
`UpdateReport.tenant` is **removed**. The struct now carries `store`, `installed` and
`self_test_passed`, and nothing else.

The field was correct when Slice 1 wrote it and was made dead by
[ADR-0097](0097-internal-route-authentication.md), which moved reporting from
`POST /internal/ota/report` — an unauthenticated route that read `tenant_id` out of the **body**, which
is precisely what made a report un-attributable — to `POST /sync/stores/{store_id}/report`, where the
cloud takes the tenant from the scoped API key and the store from the path. Since that move,
`cloud-sync-http`'s request body has not serialised the field at all.

What made this worth closing rather than tolerating is what the edge had to do to satisfy the type.
`pos_edge::ota_client` grew a helper, `unsent_tenant()`, returning a nil ULID with a comment
explaining that whatever it returns never leaves the box. That is a field a caller must fill in with a
value it knows to be meaningless — the shape that teaches the next reader the tenant is a real input
and invites somebody to start trusting it. A self-reported tenant is exactly the claim ADR-0097 exists
to refuse; leaving the field in the port kept a door propped open for it.

**Why remove rather than deprecate.** `docs/naming-and-api.md` §11's additive-only rule governs the
**wire**, and this field is not on the wire — it is a Rust struct member that every implementation
sets and none transmits. Removing it changes no bytes and no `PROTOCOL_VERSION`; it is a compile-time
change inside the workspace, and the compiler finds every site.

**The legacy route stays, for now.** `POST /internal/ota/report` still exists in `pos-cloud` behind
ADR-0097's shared secret, still parses `tenant_id` from its body, and is what `docs/openapi.json`
documents. No edge in this tree calls it — the shipped loop uses `/sync` — so it is a compatibility
surface for an older edge rather than a live path. Retiring it is a route removal with its own
deprecation window and is **not** in this amendment's scope; it is recorded here so the next reader
knows the two paths disagree about tenancy on purpose, and which one is authoritative.
