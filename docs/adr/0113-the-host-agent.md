# ADR-0113 — The host agent pulls jobs and starts one container per store, and it never holds a store's credential

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-06
· Builds [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)'s `hosted-by-platform` mode
· Reuses [ADR-0061](0061-order-relay.md)'s outbound pull and keeps
[ADR-0062](0062-the-relay-wake.md)'s refusal of a cloud-to-store channel
· Hands in [ADR-0050](0050-activation-code-exchange.md)'s activation code and nothing else
· Amends [ADR-0002](0002-one-binary-per-tier.md) a second time, after
[ADR-0112](0112-print-agents.md)
· Closes the [`server.rs`](../../crates/pos-edge/src/server.rs) drain gap ADR-0110 made load-bearing
· Does per host what [ADR-0055](0055-edge-ota-updater.md) Amendment 1 does per box
· Produces the per-store hostname
[ADR-0111](0111-a-second-origin-may-address-the-edge.md) decides who may use
· Carries on every job the region
[ADR-0114](0114-region-is-required-recorded-visible.md) requires
· Relates to [ADR-0090](0090-tls-postures.md), [ADR-0088](0088-ota-artifact-hosting.md),
[ADR-0048](0048-ota-rollout-model.md), [ADR-0068](0068-fleet-liveness.md),
[ADR-0049](0049-single-active-lease.md), [ADR-0108](0108-the-lease-generation-is-authority.md),
[ADR-0003](0003-cattle-not-pets.md), [ADR-0044](0044-fork-and-deploy.md),
[ADR-0037](0037-api-keys.md), [ADR-0046](0046-backups-and-restore.md)

Fourth of the five records on the **Edge Anywhere** programme.

## The problem

### Nothing in this repository can start a process

[ADR-0110](0110-edge-placement-is-a-deployment-axis.md) declared `hosted-by-platform` real: an admin
picks a region in the console, presses Start, and the platform stands the edge up. Behind that button
there is nothing.

[`deploy/Dockerfile`](../../deploy/Dockerfile) builds one binary and says so —
`cargo build --release --locked --target "$triple" -p pos-cloud` — so there is no edge image.
[`deploy/edge/`](../../deploy/edge/) holds a systemd unit and a PowerShell installer for a machine
somebody carries into a shop. [`installer.rs`](../../crates/pos-edge/src/installer.rs) is the *update*
seam, not a provisioner: `SystemdInstaller` writes a slot and retargets a symlink, and the thing that
turns its exit into a running process is `Restart=always` in
[`pos-edge.service`](../../deploy/edge/pos-edge.service). No file in the tree creates a machine, a
user, a volume, a port or a process.

### The obvious supervisor is the one thing forbidden from reaching in

`pos_cloud` knows every store, holds every credential and runs the console the button is on. It is
also the process that may never open a connection to a store. [`AGENTS.md`](../../AGENTS.md) §1 states
it as a rule of the system — *"Stores only make outbound connections. The cloud never dials into a
store"* — and [ADR-0062](0062-the-relay-wake.md) refused a live cloud-to-store channel **on merit**,
not for lack of effort, listing three independent grounds and settling that `MessageLink` stays
one-directional permanently.

A supervisor is worse than a delivery channel, not better. Delivering an order to a store that dialled
out is a message. Reaching a shell on a machine to start a process is administrative control of the
thing that holds the store's database, and it needs an inbound port on every host, a credential that
opens it, and a NAT traversal story for regions where there is none.

### A hosted edge placement that is torn down loses the last minutes of trading

Graceful shutdown in [`server.rs`](../../crates/pos-edge/src/server.rs) is
`axum::serve(listener, …).with_graceful_shutdown(wait_for_shutdown(shutdown_rx))`, and
`shutdown_signal` logs exactly what that means: *"shutdown signal received; draining in-flight
requests"*. The publish loop is one of the background tasks that takes the same
`tokio::sync::watch` flag and stops when it flips. Nothing flushes the outbox.

In-store that costs little: the machine boots again in the same shop, and
[`event_publish.rs`](../../crates/pos-edge/src/event_publish.rs) drains from where it left off, because
*"the outbox holds, and the counter keeps trading"*. A hosted edge placement being stopped for a move,
a host being decommissioned, a container being replaced — none of those necessarily come back.
ADR-0110 made this gap load-bearing and named this record as the one that closes it.

### A container has no service manager, and the binary has no way to ask for one

`ServeOutcome` exists precisely to distinguish *stopped* from *the binary on disk changed, start me
again*, and [`server.rs`](../../crates/pos-edge/src/server.rs) says why it is a return value rather than
a log line: *"On `systemd` the distinction is invisible, because `Restart=always` starts the binary
again whatever the exit code was."* And that is where it ends —
[`main.rs`](../../crates/pos-edge/src/main.rs) logs the outcome and returns `Ok(())` either way:

```rust
let outcome = runtime()?.block_on(run(path, shutdown_signal()))?;
if outcome == ServeOutcome::RestartWanted {
```

A supervisor that decides whether a store comes back cannot read a log line. Today there is no exit
code to read.

### Identity has exactly one door, and a supervisor is one commit away from a second

[ADR-0050](0050-activation-code-exchange.md) and
[ADR-0051](0051-device-credential-provisioning.md) built one way for a machine to become a store: a
single-use code, redeemed and minted in one transaction, yielding a `posdev_` credential that lands in
the `KeyVault` under `SecretName::DeviceCredential`.
[`activation.rs`](../../crates/pos-edge/src/activation.rs) states the rule the boot gate reads —
*"'Activated' means 'a device credential is in the vault'"* — and refuses a second activation on a box
that holds one.

A process that spawns stores is the most natural place in the system to put a second, quieter door:
a supervisor key that can mint a credential for any store it starts. That door would be opened for
convenience and closed by nobody.

## The decision

**A regional pool of `pos_host` agents long-polls the cloud for jobs over the same outbound rail the
order relay already runs on, and each job starts, drains or re-points exactly one store's `pos_edge`
in its own container, with its own volume, its own user and its own hostname — and the only identity
the job carries is an ADR-0050 activation code.**

### The agent pulls region-scoped jobs, on the relay's shape, because that shape is already paid for

The store-facing surface grows one family, and it is a copy of the one
[`relay.rs`](../../crates/pos-cloud/src/relay.rs) already serves:

- `GET /sync/hosts/{host_id}/jobs` — a bounded long-poll returning the pending batch for this
  host's region immediately if any, else holding open to a cap.
- `POST /sync/hosts/{host_id}/jobs/{job_id}/ack` — the agent reports what it did.
- `POST /sync/hosts/{host_id}/heartbeat` — liveness and capacity, the shape
  [ADR-0068](0068-fleet-liveness.md) already defines.

They authenticate like the rest of that family, with a scoped key
([ADR-0037](0037-api-keys.md)) under one new deny-by-default variant in
[`apikey.rs`](../../crates/pos-cloud/src/auth/apikey.rs), `Scope::RunHostJobs` (`run_host_jobs`).
`Scope::from_wire` is already deny-by-default on read, so an older cloud simply does not know the
name and grants nothing.

**Reusing the relay's shape is the point, not a convenience.** [ADR-0062](0062-the-relay-wake.md) did
four hard things on that rail and each of them is exactly as necessary here: the `RelayWake` seam so an
idle waiter costs no queries; two waiter classes with separate signals, so waking the wrong one is
impossible; *"a waiter subscribes before it reads, never after"*, which is the ordering bug that
otherwise turns a lost signal into a job that sits for the whole long-poll cap; and jitter on the
reconnect backoff, so a cloud deploy does not bring every host back in lockstep. A second delivery
mechanism would need all four and would get three of them right.

It also means **no inbound port on a host and no new direction of trust**. A host in a region behind
NAT works with no port forward, exactly as a store on a 4G SIM does. `pos_cloud` gains no client that
dials anything, so ADR-0062's refusal is not weakened by a special case for machines the platform
happens to own — and machines the platform owns today are machines an operator owns tomorrow.

### A host key is bound to one host in one tenant, and a host is enrolled before it has one

**The scope says what a key may do; the binding says whose data it may do it to, and the binding is
the isolation.** In the family this copies, `StoredApiKey`
([`apikey.rs`](../../crates/pos-cloud/src/auth/apikey.rs)) carries a `tenant_id` — *"the one field
isolation rests on"* — and an optional `store_id`, and every `/sync/stores/{store_id}/…` handler calls
`require_store(&grant, store_id)` ([`bearer.rs`](../../crates/pos-cloud/src/auth/bearer.rs)) so a key
issued for store A cannot read store B. Without the equivalent here, any key carrying `run_host_jobs`
could long-poll `GET /sync/hosts/{any_host_id}/jobs` and collect another host's spawn jobs — each one
carrying a live single-use activation code for a store — which is fleet-wide credential harvesting in
the record whose title promises the opposite.

So **`StoredApiKey` gains an optional `host_id` beside `store_id`**, `bearer.rs` gains
`require_host(&grant, host_id)` as the exact mirror of `require_store`, and all four host routes —
the three above and the artifact fetch below — call it before they read anything. A key with no
`host_id` is refused on every one of them: absent is not a wildcard, for the same reason
[ADR-0114](0114-region-is-required-recorded-visible.md) gives about the region — a wildcard here means
"any agent may start this store", and that arrives as a race with no human in it.

**A host registration belongs to exactly one tenant, and that is a cost taken deliberately.** The
alternative is a platform-level host table outside the tenant RLS pattern, and a second isolation model
in a cloud where every isolation claim rests on one column is how the first cross-tenant read happens.
One physical machine may carry more than one registration — one per tenant it serves, each with its own
`host_id`, its own key, its own declared ceiling and its own job stream. The cost is real and is stated
rather than hidden: nothing sums two registrations' ceilings, so a machine serving two tenants is two
hosts to the console and to capacity planning. What would change that is a platform-scoped grant kind,
which is a change to the auth model and needs its own record, not a paragraph in this one.

**A host exists because a person enrolled it.** `POST /admin/hosts` writes the registry row — the
region, the declared ceiling, a human label — under `ConsolePermission::ManageStores`
([ADR-0067](0067-multi-admin-console-rbac.md)), the same permission that bumps the lease and sets
`edge_placement`, because enrolling a machine that may run stores is that class of act. It carries
`If-Match` ([ADR-0094](0094-console-optimistic-concurrency.md)) and writes an audit entry
([ADR-0069](0069-audit-trail.md)) under `host.register`, with `host.retire` at the other end. The key
is minted by the route that already mints keys, `/admin/api-keys`, bound to that `host_id`, carrying
`run_host_jobs` and nothing else, shown once and never stored — the shape a store's own key already
has. The agent holds one credential, and it is its own.

### A job carries a store id, a region, a release, a hostname and a single-use code

Three kinds, and no others:

- **`spawn`** — store id, the **region**, the release tag this store's ring says it should run, its
  `cloud_url`, its per-store hostname, and a **single-use activation code**.
- **`stop_and_drain`** — store id, the region, and the drain budget below.
- **`update`** — store id, the region, and a release tag.

The kind is a published enum on the job wire and takes the naming standard's shape
([ADR-0010](0010-naming-standard.md)): `JOB_KIND_UNSPECIFIED` is the mandatory zero value, and an agent
that reads it refuses the job rather than guessing which of the three a newer cloud meant, beside
`JOB_KIND_SPAWN`, `JOB_KIND_STOP_AND_DRAIN` and `JOB_KIND_UPDATE`. The ack's outcome is the same shape
— `ACK_OUTCOME_UNSPECIFIED`, `ACK_OUTCOME_STARTED`, `ACK_OUTCOME_DRAINED`, `ACK_OUTCOME_NOT_DRAINED`,
`ACK_OUTCOME_REFUSED` — so a cloud meeting an outcome it predates records an unspecified ack instead of
reading an unknown token as success.

**Every job carries the region, and the long-poll filters on it.**
`GET /sync/hosts/{host_id}/jobs` returns only jobs whose region equals the region on that host's
registry row, and **a job with no region is returned to nobody**. That is the rule ADR-0114 depends on
and it is not a second vocabulary: the job carries that record's `region_country` and `region_label`
verbatim, and the match is exact equality on both, with no parsing — `region_label` is opaque there and
stays opaque here. Equality is safe by construction rather than by luck, because in
`hosted-by-platform` the console offers the regions enrolled hosts actually declared, so the value on
the store is one a host already wrote. A job whose region matches no enrolled host stays pending in the
queue and shows as pending in the console. It is never widened to a host that happens to be free.

The code is minted by the cloud through the route that already mints codes,
`/admin/activation-codes`, with the same `Issued → Redeemed` single-use rule, the same `Revoked` state
for the leaked-sheet case, and the same audit entry ([ADR-0069](0069-audit-trail.md)). The agent puts
it in the container's environment as `POS_EDGE_ACTIVATION_CODE`; the boot gate reads it when the vault
holds no credential and runs the exchange
[`activation.rs`](../../crates/pos-edge/src/activation.rs) already runs. One new optional environment
variable and one branch. `POST /api/activate` is unchanged and is still how a human activates a box in
a shop.

**A fresh code rides every `spawn`, including a restart, and that is correct rather than wasteful.**
If the container's vault already holds a credential the boot gate never reads the variable, so the code
is never presented and no second exchange is attempted: it is simply left unredeemed, and the cloud
revokes it when the store's next heartbeat proves it did not need one. If the vault holds none — a
first spawn, a rebuilt container, a vault that did not survive — it redeems and gets one. Either way
the store comes up with nobody typing anything, which is what the Start button promised.

**Minting a second kind of credential would be wrong three times over.**

It puts a fleet-wide capability in a supervisor. An activation code is scoped to one store and is
spent on first use; a minting key is scoped to everything the host has ever run and is spent never. A
host's compromise would stop being "the stores on this box" and become "every store this box has ever
started", which is a different incident with a different disclosure.

It creates a second revocation story. Today a compromised store is one credential to revoke and one
`SecretName::ALL` wipe. A supervisor-minted credential would need its own list, its own route, its own
audit shape and its own answer to "what does revoking the host do to the stores it started".

And it would make mode 3 different from modes 1 and 2 at the layer ADR-0110 insisted stays identical.
A platform-hosted store must be indistinguishable from an operator-hosted one to the cloud, because
the whole affordability argument is that the cloud learns about every store the same way: activation,
heartbeat, config pull, events.

**The job carries no scoped sync key, and the host is never a courier for one.** A supervisor holding
five hundred stores' `read_config` keys is the single object whose compromise is the fleet. So the
store's key arrives through the door that already exists: `ActivationGrant` in
[`cloud_sync.rs`](../../crates/pos-ports/src/cloud_sync.rs) gains one optional field beside
`device_id` and `credential`, and the edge stores it under
[ADR-0086](0086-edge-keyvault-and-activation.md)'s `SecretName::SyncKey`. Additive, `Option`, and a
cloud that sends none leaves every existing box exactly where ADR-0086 left it — vault first, falling
back to `POS_EDGE_SYNC_KEY`.

### One container, one volume, one user, one loopback port

Each store gets its own container running the same `pos_edge` release its ring names; its own volume
holding its SQLite database, its `.pre-update` copy and its state directory; its own OS user, a
distinct uid rather than a shared unprivileged one; and its own port, published on `127.0.0.1` only.
The reverse proxy below is the sole thing that reaches it.

**A compromised edge must not be able to read**: another store's SQLite file or outbox; another
store's device credential, scoped key or vault; another store's activation code, spent or not; the
host agent's own `run_host_jobs` key; the certificates and private keys in `secrets/tls/`
([ADR-0090](0090-tls-postures.md)); and the job stream itself. The enforcement is the four nouns above
— separate volume, separate uid, no shared mount, no path from the container to the agent's own state
— plus one refusal that is easy to give away and impossible to take back: **the container gets no
handle on the runtime that started it.** A container that can talk to its own daemon is a host, not a
guest, and every isolation sentence above becomes decoration.

The store binary needs none of this. It opens a SQLite file, binds a port and dials out. That is why
the isolation can be this blunt.

### Caddy in front, one hostname per store, and ADR-0090's four values unchanged

Each hosted store answers on its own hostname, and the host runs Caddy in front of its containers with
**exactly** [ADR-0090](0090-tls-postures.md)'s vocabulary: `TLS_MODE` with four values —
`acme-http01`, `acme-dns01`, `byo-cert`, `external` — nothing inferred from the hostname, each mode's
inputs checked and refused loudly when absent, the chosen mode recorded on the box, and `secrets/tls/`
as the one certificate location with a named populator per mode.

**The host's proxy configuration is its own, and it does not touch the cloud's.** The files in
[`deploy/Caddyfile.d/`](../../deploy/Caddyfile.d/) are not a library of site fragments: four of them
are `TLS_MODE` selectors, one of which [`bootstrap.sh`](../../deploy/bootstrap.sh) copies into
`secrets/Caddyfile` after validating the name against a closed list of exactly those four. Another file
there would be a fifth posture — the thing the paragraph above forbids — and nothing would ever install
it. Nor can the host import the shared part: `reverse_proxy pos_cloud:8080` lives *inside*
[`site.caddy`](../../deploy/Caddyfile.d/site.caddy), so importing it is precisely what pins the
upstream to the cloud. And that whole directory is mounted by
[`compose.yml`](../../deploy/compose.yml) into the cloud's Caddy, on a different machine entirely.

So a new `deploy/host/` holds the host's own compose file and one per-store site template the agent
renders per store: a site address that is the store's hostname, and a health-checked `reverse_proxy` at
that store's loopback port. What is reused is ADR-0090's **vocabulary and its refusals**, which is
where that record's content actually is — the same four `TLS_MODE` values, the same per-mode input
checks that stop the run rather than downgrading, the same `secrets/tls/` as the one certificate
location with a named populator per mode. Two proxy files that share no lines is the honest cost of two
machines; the duplication worth refusing is the other one.

Inventing a second posture vocabulary would be wrong for a reason that is not tidiness. ADR-0090's
real content is its **refusals** — `acme-dns01` with no `CF_DNS_API_TOKEN` stops the run rather than
silently downgrading to a method that cannot work, `byo-cert` with no files stops before Compose
starts a proxy that will fail to load them. A second implementation would re-derive those refusals
from scratch and get them wrong in the same way `bootstrap.sh` got them wrong before that record: by
falling through to a default that looks like a decision. Two vocabularies would also mean an operator
running mode 2 and the platform running mode 3 have two words for one posture, and the runbook has to
translate.

What the hostname is *for* — a browser on a counter reaching an edge that is not on its LAN, and what
that does to [ADR-0030](0030-pairing-and-offline-auth.md)'s raw-IP pairing URL and to
`ui/src/api/client.ts`'s same-origin `fetch` — is
[ADR-0111](0111-a-second-origin-may-address-the-edge.md)'s subject. This record produces the name; that
one decides what may use it.

### Drain before stop, bounded, and loud when it could not finish

**A `stop_and_drain` job may not report success until the outbox is empty or the budget is spent.**
That is a hard requirement, and it is the [`server.rs`](../../crates/pos-edge/src/server.rs) gap
closing: the shutdown path gains a final drain in which the publish loop is asked to run to
`outbox_depth == 0` before the process exits, rather than being one more background task that stops
when the shared flag flips.

The wait is bounded at **60 seconds**, and both ends of that matter.

It is bounded because [`AGENTS.md`](../../AGENTS.md) §2 forbids an unbounded structure and the same
reasoning covers an unbounded wait — a store whose cloud is unreachable would otherwise never stop,
and a host would hang on a container it was told to remove. It is 60 seconds rather than 5 because
`event_publish.rs`'s own `RETRY_BACKOFF` is 15 seconds, so a budget under that gives a link that
hiccuped once no second attempt at all.

**When the budget runs out, the process says so in two places the agent can actually read**, and a log
line is neither of them: this record refused a supervisor that reads log lines two sections above, and
that refusal stands here. The channel is the pair the agent already owns.

- **An exit code.** `ServeOutcome` gains a third value, `DrainIncomplete`, and
  [`main.rs`](../../crates/pos-edge/src/main.rs) maps the three onto exit codes — `0` stopped and
  drained, `10` restart wanted, `11` drain budget spent. An exit code is what a supervisor gets from the
  runtime it started the process in, with no channel into the container at all.
- **A status file on the store's own volume.** Before exiting, the edge overwrites one bounded JSON
  object at `<state>/drain-status.json`: the store id, the outbox depth it stopped at, the budget it
  spent, and the finish time. The agent reads it because it **owns the volume** — not by exec'ing into a
  container, not over a debug port, and not by parsing prose. It holds no personally identifiable
  information: a count is a count, and the outbox records themselves stay where they are, which is what
  `AGENTS.md` §2 requires.

The agent then acks `ACK_OUTCOME_NOT_DRAINED` carrying that residual count rather than
`ACK_OUTCOME_DRAINED`. If the file is missing — a `SIGKILL`, a machine that lost power mid-flush — the
ack still says `not_drained` and carries no count, and the cloud treats an unknown residual exactly as
it treats a positive one, because both mean *this outbox was not proven empty*. The cloud records the
failed drain, and ADR-0110's handover states do the rest: a `not_drained` edge placement is **not
`settled`**, so nothing about it may be retired and its volume is not deleted. Between the last
successful publish and the teardown, that volume holds the only copy of those events.

One coordinated edit follows and is easy to miss: `TimeoutStopSec=30` in
[`pos-edge.service`](../../deploy/edge/pos-edge.service) is shorter than a 60-second drain budget, so
an in-store box would be `SIGKILL`ed mid-flush by the very unit that was supposed to let it finish.
**It becomes 90 seconds**: the 60-second drain budget, plus the in-flight HTTP drain that runs before
it, plus margin, so systemd is never the thing that ends a flush still inside its own budget. The
number is the budget's and moves with it; the unit may not be the tighter of the two.

### Updates: the host re-points the binary, the edge still decides nothing about its own image

An edge in a container **must not install its own release**, and it already does not try:
`SystemdInstaller::is_ready` looks for `<state>/bin/current` and a container has none, so the box
*"logs that it found no layout and starts no updater"* and keeps trading. That existing behaviour is
the correct one and needs no change.

It is also why this section exists, and ADR-0110's Consequences say it in the same words: an operator's
Linux VPS running [`pos-edge.service`](../../deploy/edge/pos-edge.service) gets the `StateDirectory`,
the two slots and the atomic symlink rename exactly as an in-shop box does, **so mode 2 gets OTA for
free and mode 3 does not**. A container is not going to grow a service manager to fix that. The swap
moves up one level instead, to the agent.

So the host performs the swap, per store, honouring that store's ring. The ring is not the host's to
choose: it lives in the `device_ota` node the cloud publishes per store
([ADR-0052](0052-ota-rollout-config.md)), and the verdict is
[`decide_rollout`](0048-ota-rollout-model.md)'s — the same pure function, the same fixed precedence,
run by the cloud when it composes the job over the same `fleet_update` and `device_ota` facts the edge
would have read. The kill switch still halts, a revoked key still refuses, and a store below `min_ring`
still gets no job.

**The host does per host what [`installer.rs`](../../crates/pos-edge/src/installer.rs) does per box**,
and the mapping is one to one:

| Per box (ADR-0055 Amendment 1) | Per host (here) |
|---|---|
| `<state>/bin/slot-a` / `slot-b` | two verified release binaries in the host's own release store |
| `current` symlink, retargeted by one `rename` | which release the store's container mounts read-only at `/opt/pos-edge/current` |
| `previous` | the last release that reached a healthy boot for this store |
| `unconfirmed`, `MAX_UNCONFIRMED_BOOTS = 3` | the same marker and the same count, held by the agent per store |
| `pos-edge --self-test` on the staged file | the same flag, run against the new binary on the store's own volume before the swap |
| `<store db>.pre-update` | unchanged; it is on the store's volume |

**Rollback here means the same two moves**: point the store back at `previous` and restore the
`.pre-update` database. The verdict that triggers it is still the edge's own, not the host's —
[ADR-0055](0055-edge-ota-updater.md) Amendment 1 split the self-test in two on purpose, and the boot
confirmation can only be recorded by the version that is *running*, so `confirm_boot` keeps its job and
keeps reporting through the route it already reports on.

**The image contains no release, which is what keeps the trust chain single.** There is one signed
artifact in this system — the binary, with its detached minisign signature, verified against keys baked
in at compile time ([ADR-0047](0047-minisign-verification.md),
[ADR-0092](0092-artifact-trust-chain.md)) — and adding a second, image signatures, would mean two
answers to "is this release trustworthy" and a day when they disagree.

So `deploy/edge/Dockerfile` builds a *runtime* image only: CA roots, a fixed uid, and an
entrypoint that execs `/opt/pos-edge/current`. It is versionless and the whole platform runs one of
them. The releases are files on the host, fetched over `GET /sync/hosts/{host_id}/artifact` — the same
blob [ADR-0088](0088-ota-artifact-hosting.md) already serves to stores at
`/sync/stores/{store_id}/artifact`, with the host's key instead of the store's — and verified with the
`updater-minisign` verifier **before** the bytes are ever mounted into a container. The cloud stays the
dumb host ADR-0088 made it; a swapped blob makes an update fail and can never make a store run
unsigned code.

**There is also one key list, which is the same decision one level down, and it has to move for the
host to exist at all.** The mechanism that bakes the anchor lives today in
[`trusted_keys.rs`](../../crates/pos-edge/src/trusted_keys.rs), *inside* `pos-edge`, reading
`option_env!` at that crate's compile time behind a deliberately private parser — *"there is no public
function anywhere in `pos-edge` that turns a runtime string into a `PublicKey`"*, so a key can never
arrive from the channel it protects. `pos_host` may not depend on `pos-edge`, and the same danger
applies to it: the cloud composes the job, so a host that could take keys from the wire would be
verifying the composer's artifact against the composer's key. **The module moves to the verifier's own
crate, [`updater-minisign`](../../crates/adapters/updater-minisign/), unchanged in shape** — the same
`option_env!`, the same private parser, the same `NotBakedIn` refusal for a build with no anchor — and
`pos-edge` re-exports it, so no caller of `trusted_keys()` changes. It reads `POS_TRUSTED_KEYS` and
falls back to `POS_EDGE_TRUSTED_KEYS`, which is the additive rule applied to a build-time input: an
existing fork's build keeps working untouched.

Sharing the list is not a convenience either. The host verifies *the edge's* release. A second key list
for one artifact is exactly the "two answers to is this release trustworthy" this section opened by
refusing, one layer down, and the day they disagree is the day a signed release installs on five hundred
in-store boxes and is refused by the host that was supposed to check it first.

This is also the honest answer to ADR-0110's *"there is no edge image"*: there is now an edge runtime
image, and it is empty of edge.

### Capacity and liveness land in the fleet read model that exists

A host heartbeats in the **shape** [ADR-0068](0068-fleet-liveness.md) defines — one `last_seen_at`,
upserted on contact, with online/offline derived at read time against `FLEET_ONLINE_THRESHOLD_MS` and
never stored, exactly as that record settled — but **not into `store_liveness`**. That table is
[`0020_store_liveness.sql`](../../crates/adapters/store-postgres/migrations/0020_store_liveness.sql),
`PRIMARY KEY (tenant_id, store_id)` with both columns `NOT NULL`, and its header says why: *"the
store's liveness is its tenant's data"*. A host is not a store and has no store id, so it has no row to
occupy there, and widening the key of the table the fleet console reads so it can admit a different
kind of thing is how a read model stops meaning one thing.

So the host's `last_seen_at` sits on its own registry row in `0052_host_agents.sql`, keyed
`(tenant_id, host_id)` and RLS-isolated by the same policy shape — which it can be, because the section
above bound a host registration to exactly one tenant. `/admin/fleet` and `/admin/fleet/{store_id}` are
still the only fleet routes: the list renders host rows beside store rows by reading both captures, and
the store detail shows the store's own liveness and, under it, the liveness of the host running it.
`FleetStoreView` gains the host that runs the store.

Capacity is two more numbers on that same row: how many edges it is running, and the ceiling the
operator **declared** for it. Declared, never inferred — an inferred ceiling is a bin-packer that
nobody named and nobody can predict.

A second console would be wrong for one reason that outranks the others. During an incident the
question is "is this store trading?", and for a hosted store the answer is two facts stacked: the
store's own liveness and the liveness of the host under it. Two screens means the second one is not
open at 19:30 on a Friday, which is the hour ADR-0110 and
[ADR-0001](0001-offline-first-store-autonomy.md) both keep pointing at. O2
([ADR-0073](0073-alerting.md), [`alerts/model.rs`](../../crates/pos-cloud/src/alerts/model.rs)) gains a
sixth kind beside the five it has: `host_unreachable`. Its severity is `Critical`, which is one more
arm in `default_severity`'s match on kind rather than a stored field, because every store on a silent
host is a hosted store, and ADR-0110 established that a hosted store nobody can reach is a store that
is not selling.

### ADR-0002 settled: a third binary, `pos_host`

[ADR-0002](0002-one-binary-per-tier.md) says *"Exactly two binaries"*, and its reason is that
*"every additional process is something to install, monitor, upgrade and debug remotely"*. That reason
is real and the question deserves both sides before an answer.

**The case for `pos_edge host` as a subcommand.** Version lock-step: the supervisor and the supervised
are one build, so "which edge does this host know how to run" is never a question. One artifact to
sign, one release to promote, one OTA ring, one thing in `deploy/`.
[`main.rs`](../../crates/pos-edge/src/main.rs) already branches on `--self-test` and on the Windows
Service Control Manager, so a mode flag is not a new shape for that binary. And ADR-0002's cost —
another thing to install and upgrade — is genuinely avoided.

**The case for a third binary.** A supervisor needs a container runtime client, a volume and user
provisioning path, a Caddy config writer and a release store. None of that belongs in the binary that
runs on a mini-PC under a counter. `AGENTS.md` §2 requires an ADR before a dependency is added
precisely to stop a tier acquiring capabilities it will never use, and every one of those dependencies
would ship to five hundred in-store boxes that will never call them. Worse than the bytes: a store
binary that *contains* a supervisor is a privilege-escalation surface on a machine in a shop, on a
network an attacker can walk onto. [ADR-0112](0112-print-agents.md) already broke the two-binary rule
once, deliberately, and its test for whether an extra artifact is acceptable — *"it decides
nothing"* — is met here.

**Decided: a third binary, `pos_host`.** The lean going in was the subcommand, for lock-step, and that
argument does not survive the runtime-image decision above. The host does not link the edge and does
not contain a release; the binary it runs is the artifact the OTA ring published, verified by
signature. There is nothing to keep in step. What the host is actually coupled to is the **job wire**,
and that is governed by the additive rule like every other wire in this system — the host writes what
the job tells it to write, and does not need to know what a `pos_edge` release expects. The version
relationship between `pos_host` and `pos_edge` is the relationship between `systemd` and `pos_edge`
today, and nobody ships those as one artifact.

**ADR-0002 is amended to read: three tiers — store, cloud, host — and one binary per tier, plus named
device-level artifacts that decide nothing.** `pos_edge` and `pos_cloud` are unchanged.
`pos_print_agent` (ADR-0112) and `pos_host` are the two exceptions, each named, each with the tier it
serves, and each bound by the rule that keeps the exception from becoming a habit: **no domain code.**
`pos_host` may depend on exactly three workspace crates and no others: `pos-proto` for the job wire,
`updater-minisign` for the signature check, and `pos-ports` for the `PublicKey` and signer types that
verifier speaks in. `pos-ports` is named rather than implied because a list that omits a transitive
requirement is a list no check can be written against. No `pos-core`, no `pos-edge`, no `store-sqlite`:
the host holds no domain code, no store schema, and nothing that can open a store's database. That is a
dependency list a `cargo-deny`-style check can hold, exactly as ADR-0112 said of the print agent.

### When a host dies, a person moves the store, and open orders are lost

**Nothing re-spawns a dead host's edges automatically.** Not the cloud, not a sibling host, not the
pool.

The reason is the invariant the whole framework is built on. Starting a store on a second machine
without bumping the lease is two writers — duplicate receipt numbers
([ADR-0025](0025-receipt-number-authority.md)), duplicate legal invoice numbers, a double-spent last
portion of stock, two partial Z reports — and
[ADR-0049](0049-single-active-lease.md)/[ADR-0108](0108-the-lease-generation-is-authority.md) are the
only thing that forbids it. An automatic failover would do exactly that, and it would do it at the
worst possible moment: a host is declared dead when it is *unreachable*, and an unreachable host is
most often a host that is still running. A supervisor that reasons "I cannot see it, therefore it has
stopped" is a supervisor that starts a second copy of a shop that is currently selling.

So a dead host is an **edge-placement move**, run by a person, through the choreography ADR-0110
already fixed and already made boring: stand the new edge placement up, activate it, bump the lease,
wait for `settled`, retire the old. Pressing Start on a host in another region is the "stand up"
step. Nothing else about the procedure changes, and it is the same procedure whether the store is
moving between hosts, from a host back into a shop, or from a shop onto a host.

What is lost, plainly:

- **The lease is not lost.** The authoritative generation is a Postgres row
  ([`0051_store_lease.sql`](../../crates/adapters/store-postgres/migrations/0051_store_lease.sql)),
  and it survives every version of this failure. That is why recovery is a bump and not a
  reconciliation.
- **Committed events are not lost while the volume survives.** They are rows in the outbox on the
  store's own volume, and they publish when a container comes up on it again — including on a
  different host, because the volume is the store and the host is not.
- **If the volume is gone with the machine, everything since the last successful publish is gone.**
  That is not new; it is the dead-mini-PC case [ADR-0003](0003-cattle-not-pets.md) already accepts. A
  hosted store's exposure is narrower in one specific way — it cannot trade without the WAN, so it
  rarely holds more than `IDLE_INTERVAL`'s worth of events plus whatever a cloud outage accumulated —
  and that is a consequence of ADR-0110's accepted trade, not a mitigation anyone designed.
- **Open orders are lost.** An order taken and not yet settled lives in that store's database. If the
  volume is gone, the tables are occupied in the room and empty in the system, and there is no
  reconstruction, because the events that would rebuild them were never published — they were never
  meant to leave until they were committed. Staff re-enter them from the paper dockets, or from the
  guests. There is no version of this record in which that is not true, and softening it in the
  console would only mean somebody discovers it during service.

**And there is no per-store backup tier, which is the answer to ADR-0110's assignment rather than a
silence about it.** That record hands this one "the per-store isolation and the backups that follow
from it", and what follows from it is none. Take the three things a store's volume holds. Committed and
published events are already in the cloud, whose own backups
[ADR-0046](0046-backups-and-restore.md) owns — WAL archiving, the tiers, the weekly restore drill —
and nothing here changes them. The local projection is derived and rebuilds from the log. What is left
is the unpublished tail, and a snapshot cannot bound *that*: the events that matter are by definition
the ones written since the last successful publish, and a nightly copy taken at 03:00 does not contain
the sale taken at 19:45. What actually bounds the tail is in this record already — the drain that must
empty the outbox before a stop may report success, and ADR-0110's rule that nothing is deleted before
`settled`.

The second reason is one a framework may not decide on an operator's behalf. A store's volume holds
employee records and order history, so a snapshot of it is a **second resting place for that data**,
with its own country, its own retention and its own lawful basis.
[ADR-0114](0114-region-is-required-recorded-visible.md) is explicit that the framework never sees where
[`backup.sh`](../../deploy/backup.sh) ships bytes, and
[ADR-0035](0035-retention-and-pii-masking.md) owns how long anything is kept. A fork that wants
per-store snapshots adds them at the host with those two answers written down first; it does not get
them by default from a framework that can answer neither.

## What this deliberately does not do

- **The cloud still never dials a host.** Every job is pulled, every ack is pushed by the agent, and
  there is no callback URL, no agent-side listener and no SSH from `pos_cloud`. ADR-0062's three
  grounds carry over unchanged, and "we own this machine" is not a fourth position between them: the
  machines the platform owns today are machines an operator owns tomorrow, and a transport that
  depends on who owns the box is a transport with two behaviours.
- **It does not merge edge logic into `pos_cloud`, and isolation is the whole reason.** ADR-0110
  refused it once; at this layer the refusal is more concrete, not less. One store's process, one
  store's volume, one store's uid, one store's port, one store's restart. A multi-tenant seller inside
  `pos_cloud` puts every store behind one deploy, one connection pool and one migration, so a bad
  release stops a country rather than a shop — and it forfeits `in-store` mode outright, because the
  thing that sells with the cable unplugged has to be the thing in the shop.
- **No autoscaling, no bin-packing, no multi-tenant process sharing.** A host's ceiling is declared by
  a person and a job that would exceed it is refused at the ack, not queued and not "fitted
  somewhere". Any scheduler that moves stores between hosts is a scheduler that *starts* stores, and
  starting a store is a lease act with a named admin and an audit entry on it. At 500 stores over
  around five countries the arithmetic does not ask for one either: an edge placement is a long-lived
  fact about a shop, not a workload with a duration.
- **It makes no promise about a host pool the operator has not provisioned.** The Start button is
  offered only where a host with declared capacity exists in the chosen region; where none does, the
  console says so and offers `in-store` and `hosted-by-operator`. ADR-0110 kept the framework neutral
  on where an operator hosts, and this record does not smuggle a platform dependency back in through
  a greyed-out button. A fork that provisions no hosts loses one of three modes and nothing else.
- **It does not give the host a way into a running store.** No exec, no debug endpoint, no log tail
  into a container from the job wire. A host that can exec into a store's container can read that
  store's database, and then every isolation claim above is a claim about intent rather than about
  capability. The drain's status file is not an exception to this and is why it takes the shape it
  does: the agent reads a file on a volume it already owns, after the process has exited, and gains
  nothing it did not already have.
- **It does not solve the container's vault durability, and it is built so that it does not have to.**
  [ADR-0086](0086-edge-keyvault-and-activation.md) flagged headless Linux: the `linux-native` kernel
  keyring is sessionless but volatile, and the durable answer is a TPM-sealed credential that a
  container does not have. The fresh-code-per-spawn decision above is what keeps this record correct
  either way — a volatile vault costs one redemption and one audit entry per container start, not an
  outage. It is still noise at 500 stores, and a durable per-store vault on a host is wanted. It is not
  designed here, and this is where that is written down rather than assumed away.
- **The drain budget and the host ceiling are first numbers, not measured ones.** 60 seconds is
  reasoned from `RETRY_BACKOFF` and a plausible batch, and no hosted store has ever been torn down.
  Both live in one module as constants rather than published configuration, so changing one is a
  release and not a schema change, and that is where they stay until a real teardown produces a number.
- **It does not put a hosted edge on Windows.** A hosted edge placement is a Linux one, as ADR-0110
  said, and nothing here changes what an in-shop Windows box already has: a service wrapper
  ([`service.rs`](../../crates/pos-edge/src/service.rs)) and a generated installer
  ([`install-pos-edge.ps1`](../../deploy/edge/install-pos-edge.ps1), emitted by
  [`installers.mjs`](../../dashboard/src/installers.mjs), regenerated and diff-checked by the
  `dashboard` CI job and syntax-checked by
  [`installer-syntax.mjs`](../../dashboard/scripts/installer-syntax.mjs)). It keeps both. Mode 3 is
  Linux containers because that is what the isolation above is written in.
- **It does not put `pos_host` in the OTA rings.** The host ships on the cloud's cadence, because it is
  a platform machine. Putting a supervisor in the same rollout as the thing it supervises is how a
  canary takes out its own observer.
- **It does not choose *which* region a store goes in, and does not say what may rest in one.** Every
  job carries a region and the long-poll filters on it, as decided above — that much is this record's,
  because it is a property of the wire. What the region *means* — whether an edge placement is lawful,
  whether a transfer is covered, who was told — is
  [ADR-0114](0114-region-is-required-recorded-visible.md)'s, and this record hands it a pool to choose
  from.
- **It does not re-provision an existing store's scoped key.** The `ActivationGrant` field is filled on
  a *new* activation; every box already holding a key keeps it and keeps ADR-0086's env-override path.

## Consequences

- A third workspace binary, `pos_host`, permitted to depend on `pos-proto`, `updater-minisign` and
  `pos-ports` — and on nothing else in the workspace. [ADR-0002](0002-one-binary-per-tier.md) is
  amended to three tiers plus two named device-level artifacts, and gains the "decides nothing" rule
  that bounds the exception.
- The cloud serves four new store-family routes — `GET /sync/hosts/{host_id}/jobs`,
  `POST /sync/hosts/{host_id}/jobs/{job_id}/ack`, `POST /sync/hosts/{host_id}/heartbeat` and
  `GET /sync/hosts/{host_id}/artifact` — all behind one new deny-by-default scope,
  `Scope::RunHostJobs` (`run_host_jobs`), and all four calling `require_host(&grant, host_id)` before
  they read anything. `StoredApiKey` gains an optional `host_id` beside `store_id`, and
  [`bearer.rs`](../../crates/pos-cloud/src/auth/bearer.rs) gains `require_host` as the mirror of
  `require_store`. Internal to the fleet, so the routes stay out of
  [`docs/openapi.json`](../openapi.json) exactly as the other `/sync` routes do.
- Two new `/admin` routes enrol and retire a host — `POST /admin/hosts` and
  `POST /admin/hosts/{host_id}/retire` — under `ConsolePermission::ManageStores`, with `If-Match` and
  the audit actions `host.register` and `host.retire`. The agent's key is issued through the existing
  `/admin/api-keys`, bound to that `host_id` and carrying `run_host_jobs` alone.
- `crates/adapters/store-postgres/migrations/0052_host_agents.sql`: the host registry — one row per
  registration, keyed `(tenant_id, host_id)`, carrying the region, the declared ceiling, the running
  count and `last_seen_at` — and the job queue the long-poll reads, the `order_queue` shape one table
  over, carrying the region every job is filtered on. RLS-isolated by the same policy shape as
  `store_liveness`, additive per [ADR-0017](0017-migrations.md). Host liveness lives here rather than in
  `store_liveness`, which is keyed on a store id a host does not have.
- `RelayWake` gains a third and fourth waiter class for the job legs, and the job long-poll takes the
  *store* long-poll's shape rather than the parked-submit one: `FALLBACK_INTERVAL` under the fixed
  `LONGPOLL_CAP` ([`relay.rs`](../../crates/pos-cloud/src/relay.rs)). It does not use `fallback_for`,
  whose `min(wait / 2)` clamp exists for the submit park's caller-supplied deadline and has nothing to
  clamp against a fixed interval under a fixed cap.
- `/admin/fleet` and `/admin/fleet/{store_id}` render hosts beside stores off one read model.
  `FleetStoreView` gains the host that runs the store. Online/offline stays derived at read time.
- O2 gains `AlertKind::HostUnreachable`: one more variant, one more row in `ALL`, one more token in
  `as_str`, and one more arm in `default_severity` returning `Critical` — severity is derived from the
  kind there, not stored on the alert.
- Standing a store up in mode 3 is an `/admin` write and inherits what that implies:
  `ConsolePermission::ManageStores` ([ADR-0067](0067-multi-admin-console-rbac.md)) — the same
  permission that bumps the lease and sets `edge_placement`, because they are one class of act —
  `If-Match` ([ADR-0094](0094-console-optimistic-concurrency.md)), and an audit entry naming the admin
  and the host ([ADR-0069](0069-audit-trail.md)).
- `ActivationGrant` gains an optional scoped key.
  [`activation.rs`](../../crates/pos-edge/src/activation.rs) stores it under `SecretName::SyncKey`
  before it announces success, in the order the vault-is-the-truth rule already requires. Additive,
  `Option`, no `PROTOCOL_VERSION` move.
- `EdgeConfig`'s environment grows `POS_EDGE_ACTIVATION_CODE`, read once by the boot gate when the
  vault is empty. It is not a config field and not a secret worth protecting: a spent code grants
  nothing.
- [`server.rs`](../../crates/pos-edge/src/server.rs) gains a bounded outbox flush in the shutdown path
  and writes `<state>/drain-status.json` when it does not finish; `TimeoutStopSec` in
  [`pos-edge.service`](../../deploy/edge/pos-edge.service) becomes 90 seconds in the same change.
  ADR-0110's `settled` gains a **second and stronger proof beside the one it has**, not a replacement:
  that record proves a handover settled by reading `outbox_depth` off the heartbeat *rather than by
  stopping the process and hoping*, and a process that exited having emptied its outbox proves the same
  fact without waiting for another heartbeat. The heartbeat reading stays, and `settled` still requires
  it.
- `ServeOutcome` gains `DrainIncomplete`, and [`main.rs`](../../crates/pos-edge/src/main.rs) turns the
  three outcomes into exit codes `0`, `10` and `11`. `systemd` does not care, the Windows wrapper
  already reads the outcome, and a host agent finally can.
- `trusted_keys.rs` moves from `pos-edge` to
  [`updater-minisign`](../../crates/adapters/updater-minisign/) with its private parser and its
  `option_env!` discipline intact, re-exported from `pos-edge` so no caller changes. It reads
  `POS_TRUSTED_KEYS` and falls back to `POS_EDGE_TRUSTED_KEYS`, so an existing fork's build keeps
  working.
- `deploy/edge/Dockerfile`: a versionless runtime image with no release in it. A new `deploy/host/`
  holds the host's compose file and the per-store site template the agent renders;
  [`deploy/Caddyfile.d/`](../../deploy/Caddyfile.d/) is untouched, because its four mode files are
  `TLS_MODE` selectors [`bootstrap.sh`](../../deploy/bootstrap.sh) validates against a closed list, and
  a fifth would be a fifth posture.
- **The vocabulary is `edge_placement`, and this record renames neither older `placement`.** ADR-0110
  settled it: `MenuPlacement`'s `/admin/catalog/menus/{menu_id}/placements` and the OTA rollout's
  `/admin/config/ota/placement` — whose refusal in
  [`ota_state.rs`](../../crates/pos-edge/src/ota_state.rs) reads *"no placement means the device cannot
  be weighed"* — keep their names, because renaming a published route to free up a word would break the
  additive rule for a problem a longer name already solves. The store attribute is `edge_placement` in
  the column, the JSON field and the configuration vocabulary, `EdgePlacement` in Rust, and
  `EDGE_PLACEMENT_UNSPECIFIED`, `EDGE_PLACEMENT_IN_STORE`, `EDGE_PLACEMENT_HOSTED_BY_OPERATOR`,
  `EDGE_PLACEMENT_HOSTED_BY_PLATFORM` on the wire per [ADR-0010](0010-naming-standard.md). This record
  uses that word and never the bare one for this concept.
- **No backup tier is added, and no `deploy/backup.sh` behaviour changes.** A store's volume is not
  snapshotted: the published events are in the cloud under [ADR-0046](0046-backups-and-restore.md), the
  projection is derived, and the unpublished tail is bounded by the drain and by ADR-0110's rule that
  nothing is deleted before `settled`. That is this record's answer to the backups ADR-0110 assigned
  it, and it is an answer rather than a deferral.
- [`docs/glossary.md`](../glossary.md) gains **Host agent** — *the supervisor that starts and stops
  platform-hosted edges on one machine* — beside ADR-0110's **Edge placement** and ADR-0112's **Print
  agent**.
- Nothing published is removed or renamed. One scope, four `/sync` routes, two `/admin` routes, one
  optional key binding, one migration, one alert kind, two audit actions, one optional grant field, one
  environment variable, two job-wire enums with their `*_UNSPECIFIED` zero values, one exit code and one
  runtime image are all additions, and no `PROTOCOL_VERSION` moves — the edge is not told it is hosted,
  because it does not need to know.
