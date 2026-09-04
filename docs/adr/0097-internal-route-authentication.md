# ADR-0097 — The `/internal` routes get a key of their own, and now is the only cheap time to do it

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-03
**Amends** three records that state the current no-authentication posture as deliberate:
[ADR-0040](0040-reconciliation.md) §53 (rejecting authentication on `/internal/reconcile`),
[ADR-0087](0087-edge-relay-and-event-publish.md) §40 (`/internal/ingest` "remains unauthenticated"),
and [ADR-0078](0078-sync-and-ota-closure.md) §32 (`/internal/ota/report` as "a trusted-network
`/internal` route"). Each was right about the network and silent about what happens inside it.
**Relates to** [ADR-0044](0044-fork-and-deploy.md) (the server-generated secrets file this reuses) ·
[ADR-0050](0050-activation-code-exchange.md) (the no-oracle refusal this copies) ·
[ADR-0085](0085-edge-cloud-sync-transport.md) (the never-in-`config.toml` rule, and what it is
actually about) · [ADR-0024](0024-protocol-version-negotiation.md) (why a fleet cannot be assumed
upgraded) · [ADR-0090](0090-tls-postures.md) · `docs/roadmap-v3.md` **O5**

**Context.** `pos_cloud` serves three routes under `/internal/*` that authenticate nothing:

| Route | Does | Worst case if reached |
|---|---|---|
| `POST /internal/ingest` | appends a batch of event envelopes | forged events for any tenant |
| `POST /internal/reconcile` | answers which of N event ids the cloud lacks | an existence oracle over a store's event log |
| `POST /internal/ota/report` | records a store's installed version + self-test | a falsified fleet report for any store |

They were designed for a private network and documented as such. #144 closed the internet-facing
half in both deploy lanes — a `location ^~ /internal/` deny in the k8s Ingress, the equivalent in
`deploy/Caddyfile.d/site.caddy` — with an xtask gate that fails if either is removed. What remains
open is everything *inside* the trust boundary: another container on the compose network, a pod
reached directly by service IP, a fork that terminates TLS somewhere the deny does not cover, or an
operator's `curl` from the wrong host.

**The finding that decides the shape of this: nothing calls these routes yet.** That was measured,
not assumed, across the whole workspace:

- `/internal/ingest` has **no** HTTP caller anywhere. The production feed is in-process — the durable
  NATS cursor calls `cloud.ingest(...)` directly (`crates/pos-cloud/src/cursor.rs:94`), and the
  module's own doc says the HTTP route "is only the reconciliation re-push". The sole caller in the
  tree is one test.
- `/internal/reconcile` has **no** non-test caller. The edge-side loop that would call it is recorded
  as deferred in ADR-0078 and was never built.
- `/internal/ota/report` has one *client* — `cloud-sync-http`'s `report()` — and that client has **no
  caller in `pos-edge`**. It is exercised only by the contract suite.

So the ordering problem that normally makes this kind of change expensive — the server starts
requiring a header, and a fleet of deployed edges that cannot be assumed upgraded starts failing —
**does not exist today**. There is no compatibility window to design, because there is nothing to be
compatible with. That stops being true the moment R5/#265 (the OTA artifact route) or #273 (a real
`report()` caller) lands, and then it is ADR-0024's problem instead — a fleet that cannot be assumed
upgraded, with stores that may be offline for days.

That is the whole argument for doing this now rather than with the work that will need it: **the
cost of this change is a step function, and we are still on the cheap side of the step.**

**Decision. A shared secret on the three routes, cloud-side only, enforced immediately and
fail-closed.** Five parts.

**1. `X-Pos-Internal-Key`, not `Authorization: Bearer`.** `bearer::authenticate` defines the
`Authorization` value space as `pos_<ULID>_<secret>` and returns a `Grant { tenant_id, scopes }` —
the tenant-isolation boundary every `/sync` and relay handler reads. A tenantless shared secret has
no `Grant` to be, so reusing the header would force either a second parse branch inside the one
function that answers *who is calling*, or a sentinel tenant. Both are worse than a second header.
The tree already has the convention: `X-Pos-Webhook-Signature`, `X-Pos-Artifact-Signature`.

**2. The refusal is `404`, with no oracle.** Same wording whether the header is absent, malformed, or
wrong; no `WWW-Authenticate`; no `details`. This is not novelty — it is what the two proxy denies
already answer, for the reason `site.caddy` states in place: *a 403 confirms the route exists*.
`/admin/setup` sets the same precedent for a route that is off. A caller who is supposed to be here
has the key; a caller who is not should not learn that there was something to find. Comparison is
`constant_time_eq`, already `pub` in `auth::enrol` and already imported by `http.rs` — not a fourth
copy.

**3. The secret lives in `deploy/secrets/cloud.toml`, and this does not break the
never-in-`config.toml` rule.** The rule comes from ADR-0085, and it comes with its reason attached:
the edge's `config.toml` is *"the world-readable bootstrap an operator may inspect"*. That is a
statement about **the edge's file**, not about the four characters `.toml`. In the same breath
ADR-0085 calls the cloud's server-generated secrets *"the same shape"* as the mode-0600
`EnvironmentFile` it prescribes. `deploy/secrets/cloud.toml` is mode 600, chowned to uid 10001,
inside a mode-700 directory, git-ignored, and it already holds the Postgres password
(`database_url`), `admin_setup_token` and `table_token_secret`.

Putting a fourth secret there is not an exception to the rule; it is where the cloud's secrets
already are. The alternative — a `POS_CLOUD_INTERNAL_KEY` environment variable — was considered and
rejected: it satisfies the letter of a rule written about a different file, and pays for it by
introducing the cloud binary's *first* secret-bearing env var (today it reads exactly one env var,
`POS_CLOUD_CONFIG`) and splitting the cloud's secrets across two mechanisms. A rule enforced against
its own reason is a rule that has stopped working.

The value is wrapped in a redacting newtype rather than held as a bare `String`, following
`webhook::sign::SigningSecret` — private field, hand-written `Debug`, an `expose` named to be
conspicuous in a diff — because `CloudConfig` derives `Debug` and already carries three plaintext
secrets. That is a pre-existing smell this does not add to.

**4. Fail closed at boot, not "absent means off".** `CloudConfig::validate()` refuses to start when
the key is missing or too short, naming the field. This is the part that is load-bearing, and the
reason is specific: `CloudConfig` is **not** `#[serde(deny_unknown_fields)]` (unlike `EdgeConfig`,
which is). So `internal_shared_secet = "..."` — one transposed letter — deserialises to `None`. If
absence meant "authentication off", that typo would leave a mode-0600 file that looks correct to the
operator in front of a wide-open surface. A boot refusal converts a silent hole into a loud one.

This means an existing deployment does not start until its `cloud.toml` gains the key. That is
intended, it is the framework behaving the way a framework should, and it is an upgrade note rather
than a surprise: `bootstrap.sh` mints it, and the fork checklist and both runbooks say so.

**5. Cloud-side only. The secret never reaches a store box.** No `SecretName` variant, no second env
var beside `POS_EDGE_SYNC_KEY`, no service-unit change, no wizard change, and nothing added to a
store's `config.toml`. Two reasons, and the second is the more important one.

The first is that there is nothing to give it to — see the finding above.

The second is that **giving it to stores would be actively worse.** `cloud-sync-http` serves
`/activate` and the `/internal/*` paths through one `TlsHttpTransport`, and that transport is
constructed on every box with a `cloud_url`, including boxes that have not activated yet. A header
attached unconditionally would ship a fleet-wide secret to an unauthenticated pre-activation
endpoint on every unprovisioned machine — strictly worse posture than today. Any future edge-side
sender must gate the header on the path prefix, and that guard is load-bearing, not a nicety.

**A fleet-wide secret cannot make `/internal/ota/report` honest, and this ADR does not pretend
otherwise.** That route takes `tenant_id` and `store_id` **in the request body** — ADR-0078 §32 says so in
as many words — so it trusts the caller's claim about which store it is. A shared secret changes who can reach the route; it does not
make the report attributable. Any holder of the key could still file a report for any store.

The fix for that is not a better secret — it is the surface. When store-originated reporting gains a
real caller, it moves to `/sync/stores/{store_id}/…`, where the cloud resolves the tenant from the
scoped key instead of reading it out of the body. `site.caddy` already names this as the intended
path. Recorded here so the shared secret is not later mistaken for having solved it.

**Scope.**

In: the `CloudConfig` field and its redacting newtype; the `validate()` arm; the guard and its
threading into all three routes (`/internal/ingest` on the main `CloudApp` router, `/internal/
reconcile` and `/internal/ota/report` on their own sub-routers); `bootstrap.sh`, `compose.yml` and
the k8s manifest; `docs/fork-checklist.md` and both runbooks; and — in the same commit — every
in-tree statement that these routes carry no authentication, which this makes false: four doc
comments in `http.rs`, one in `cloud-sync-http/src/client.rs`, the two proxy configs, `k8s/README.md`,
and **the `tls-modes` gate's hint text**. The gate keeps failing on a removed deny; its hint must
stop teaching the opposite of the new truth, or the first fork to read it concludes the proxy deny
is now redundant and deletes it.

The guard goes on `/internal/reconcile`'s **handler**, not its router: that router also carries
`/admin/reconcile`, which is gated by a console permission and must not require the internal header.

Out, with reasons:

- **Moving `/internal/ota/report` to `/sync`.** The right fix for attribution, and a different slice:
  it changes a wire surface, needs the store-side caller that does not exist yet, and would hide a
  security fix inside a protocol change.
- **`deny_unknown_fields` on `CloudConfig`.** It would reject every existing box whose `cloud.toml`
  carries a stale or commented key. The `validate()` arm closes the specific hole without that blast
  radius. Worth doing later, on its own.
- **Rate limiting the refusal.** The routes are private-network-only and now key-gated; a brute force
  needs network position first. The console's login limiter exists if this changes.
- **Any edge-side sender.** Nothing to send today, and the pre-activation leak above means the first
  one needs its own design.

**Alternatives.**

- **Rely on the proxy denies alone.** Rejected — that is the posture this amends. One control, at one
  layer, that a fork can misconfigure and that does not exist at all for direct in-cluster access.
- **Per-store credentials instead of one shared key.** Rejected *as framed*: a per-store credential
  for these routes is `/sync`, which already exists and already resolves the tenant from the key. The
  question is not which secret `/internal` should take but whether store traffic belongs on
  `/internal` at all — and it does not. Answered above rather than duplicated here.
- **A compatibility window (`Off` / `Warn` / `Required`).** Rejected, and this is the alternative
  that would have been correct in almost any other circumstance. A three-mode config plus a
  deprecation schedule is real machinery, and it buys nothing when the set of callers to be
  compatible with is empty. Shipping it would mean carrying a mode that no deployment ever needs
  through however many releases it takes to retire. If this were being done after R5 lands, the
  window would be mandatory and this record would say so.
- **mTLS between the edge and `/internal`.** Rejected for now, not on merit: ADR-0089 already records
  per-store mTLS as the direction for NATS, and doing it here first would fork that story. When mTLS
  arrives it supersedes this.

**Consequences.**

- An existing deployment will not boot after upgrade until `cloud.toml` gains the key. Loud, named in
  the error, and minted automatically by `bootstrap.sh` for new boxes.
- The three routes stop being reachable by anything on the private network that does not hold the
  key. Today that set is empty of legitimate callers; a fork running its own cloud-side tooling from
  another host must add the header.
- Two controls now guard the same surface, and neither is redundant. The proxy keeps the internet
  out; the key keeps the inside honest. The gate that enforces the first must keep failing.
- The next `/internal` route is born requiring the key, because the guard is threaded, not optional.

**Delivery.** This ADR, then one slice: the config field and newtype, the `validate()` arm, the guard
and its three call sites, the deploy and doc changes, the corrections to every "carries no
authentication" claim including the gate's hint, and tests that pin the 404-with-no-oracle refusal
and the boot refusal.

---

**Delivery note (2026-09-04) — the report move this ADR said was owed is done.**

This record predicted its own successor: *"When store-originated reporting gains a real caller, it
moves to `/sync/stores/{store_id}/…`, where the cloud resolves the tenant from the scoped key instead
of reading it out of the body."* R5 is that caller, so the move landed with it.

`POST /sync/stores/{store_id}/report` now takes the tenant from the key and the store from the path,
and the body carries only `installed` and `self_test_passed`. Dropping the two ids from the body was
not tidying: leaving them would mean the wire still *offered* a store id that the cloud has to be
careful to ignore, which is the kind of field somebody wires back up in two years.

**What the move actually buys, stated exactly.** A report is now **tenant-attributable**. It is not
store-attributable, because a [`Grant`](../../crates/pos-cloud/src/auth/apikey.rs) pins a tenant and a
scope, not a store — so a key issued to one store can still name a sibling store of the same tenant in
the path. Cross-tenant forgery is closed; intra-tenant is not. That residual is bounded by what a
report *is*: pure telemetry that never changes what any box runs ([ADR-0078](0078-sync-and-ota-closure.md)),
so the worst case is a distorted rollout picture inside one operator's own fleet. Closing it needs a
store-scoped grant, which is a key-issuance change and its own slice — recorded here rather than
implied, because "attributable" without the qualifier is exactly the overclaim this ADR was written to
avoid making about a shared secret.

**`/internal/ota/report` stays, and now has no intended caller.** It keeps the shared-secret guard and
remains legitimate for on-box tooling, which is what `/internal` is for. But the edge no longer posts
to it, so it is a deprecation candidate rather than a live path; it is not removed here because the
CHANGELOG's rule is two releases of deprecation first. Both routes share one write helper
(`record_ota_report`), so the difference between them is *where identity comes from* and nothing else —
which makes that the real difference rather than merely the intended one.

The route gate is `read_config`, for the reason [ADR-0088](0088-ota-artifact-hosting.md) Amendment 1
gives for the artifact route: every provisioned box already carries it, so the OTA path needs no
re-provisioning of live stores, and a report is not a secret.
