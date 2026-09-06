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
· Relates to [ADR-0090](0090-tls-postures.md), [ADR-0088](0088-ota-artifact-hosting.md),
[ADR-0048](0048-ota-rollout-model.md), [ADR-0068](0068-fleet-liveness.md),
[ADR-0049](0049-single-active-lease.md), [ADR-0108](0108-the-lease-generation-is-authority.md),
[ADR-0003](0003-cattle-not-pets.md), [ADR-0044](0044-fork-and-deploy.md)

Fourth of the five records on the **Edge Anywhere** programme. **ADR-0111** (a second origin may
address the edge) and **ADR-0114** (region) are reserved numbers with no files, so they are named
here in plain text and not linked: `xtask links` fails a build on a link that does not resolve.

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

### A hosted placement that is torn down loses the last minutes of trading

Graceful shutdown in [`server.rs`](../../crates/pos-edge/src/server.rs) is
`axum::serve(listener, …).with_graceful_shutdown(wait_for_shutdown(shutdown_rx))`, and
`shutdown_signal` logs exactly what that means: *"shutdown signal received; draining in-flight
requests"*. The publish loop is one of the background tasks that takes the same
`tokio::sync::watch` flag and stops when it flips. Nothing flushes the outbox.

In-store that costs little: the machine boots again in the same shop, and
[`event_publish.rs`](../../crates/pos-edge/src/event_publish.rs) drains from where it left off, because
*"the outbox holds, and the counter keeps trading"*. A hosted placement being stopped for a move, a
host being decommissioned, a container being replaced — none of those necessarily come back. ADR-0110
made this gap load-bearing and named this record as the one that closes it.

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

- `GET /sync/hosts/{host_id}/jobs` — a bounded long-poll returning the pending batch immediately if
  any, else holding open to a cap.
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

### A job carries a store id, a release, a hostname and a single-use code

Three kinds, and no others:

- **`spawn`** — store id, the release tag this store's ring says it should run, its `cloud_url`, its
  per-store hostname, and a **single-use activation code**.
- **`stop_and_drain`** — store id, and the drain budget below.
- **`update`** — store id and a release tag.

The code is minted by the cloud through the route that already mints codes,
`/admin/activation-codes`, with the same `Issued → Redeemed` single-use rule, the same `Revoked` state
for the leaked-sheet case, and the same audit entry ([ADR-0069](0069-audit-trail.md)). The agent puts
it in the container's environment as `POS_EDGE_ACTIVATION_CODE`; the boot gate reads it when the vault
holds no credential and runs the exchange
[`activation.rs`](../../crates/pos-edge/src/activation.rs) already runs. One new optional environment
variable and one branch. `POST /api/activate` is unchanged and is still how a human activates a box in
a shop.

**A fresh code rides every `spawn`, including a restart, and that is correct rather than wasteful.**
If the container's vault already holds a credential, the second activation is the conflict ADR-0050
specifies and the edge carries on with what it holds. If it holds none — a first spawn, a rebuilt
container, a vault that did not survive — it redeems and gets one. Either way the store comes up with
nobody typing anything, which is what the Start button promised. An unredeemed code is revoked by the
cloud when the store's next heartbeat proves it did not need one.

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

The host's site file is a fifth file in [`deploy/Caddyfile.d/`](../../deploy/Caddyfile.d/) in the same
shape as the four that exist, importing the shared part exactly as
[`site.caddy`](../../deploy/Caddyfile.d/site.caddy) is imported today, with the per-store
`reverse_proxy` pointing at that store's loopback port instead of `pos_cloud:8080`.

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
`ui/src/api/client.ts`'s same-origin `fetch` — is ADR-0111's subject. This record produces the name;
that one decides what may use it.

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

**When the budget runs out, the process says so, by name and by count**, and the agent acks
`not_drained` with the residual depth rather than `drained`. The log line carries the store id and the
number of undelivered events and nothing else — an outbox record is a domain event, and `AGENTS.md`
§2 keeps personally identifiable information out of logs. The cloud records the failed drain, and
ADR-0110's handover states do the rest: a `not_drained` placement is **not `settled`**, so nothing
about it may be retired and its volume is not deleted. Between the last successful publish and the
teardown, that volume holds the only copy of those events.

One coordinated edit follows and is easy to miss: `TimeoutStopSec=30` in
[`pos-edge.service`](../../deploy/edge/pos-edge.service) is shorter than a 60-second drain budget, so
an in-store box would be `SIGKILL`ed mid-flush by the very unit that was supposed to let it finish.
The unit's stop timeout rises with the budget, in the same change.

### Updates: the host re-points the binary, the edge still decides nothing about its own image

An edge in a container **must not install its own release**, and it already does not try:
`SystemdInstaller::is_ready` looks for `<state>/bin/current` and a container has none, so the box
*"logs that it found no layout and starts no updater"* and keeps trading. That existing behaviour is
the correct one and needs no change.

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
in at compile time ([`trusted_keys.rs`](../../crates/pos-edge/src/trusted_keys.rs),
[ADR-0047](0047-minisign-verification.md), [ADR-0092](0092-artifact-trust-chain.md)) — and adding a
second, image signatures, would mean two answers to "is this release trustworthy" and a day when they
disagree. So `deploy/edge/Dockerfile` builds a *runtime* image only: CA roots, a fixed uid, and an
entrypoint that execs `/opt/pos-edge/current`. It is versionless and the whole platform runs one of
them. The releases are files on the host, fetched over `GET /sync/hosts/{host_id}/artifact` — the same
blob [ADR-0088](0088-ota-artifact-hosting.md) already serves to stores at
`/sync/stores/{store_id}/artifact`, with the host's key instead of the store's — and verified with the
`updater-minisign` verifier **before** the bytes are ever mounted into a container. The cloud stays the
dumb host ADR-0088 made it; a swapped blob makes an update fail and can never make a store run
unsigned code.

This is also the honest answer to ADR-0110's *"there is no edge image"*: there is now an edge runtime
image, and it is empty of edge.

### Capacity and liveness land in the fleet read model that exists

A host heartbeats on the shape [ADR-0068](0068-fleet-liveness.md) already defines, into the same
liveness capture (`store_liveness`, migration
[`0020_store_liveness.sql`](../../crates/adapters/store-postgres/migrations/0020_store_liveness.sql)),
and surfaces on the routes that already exist: `/admin/fleet` and `/admin/fleet/{store_id}`.
`FleetStoreView` gains the host that runs the store; the fleet list gains host rows beside store rows.
Online/offline stays derived at read time against `FLEET_ONLINE_THRESHOLD_MS`, never stored, exactly as
that record settled.

Capacity is two numbers on a host's row: how many edges it is running, and the ceiling the operator
**declared** for it. Declared, never inferred — an inferred ceiling is a bin-packer that nobody named
and nobody can predict.

A second console would be wrong for one reason that outranks the others. During an incident the
question is "is this store trading?", and for a hosted store the answer is two facts stacked: the
store's own liveness and the liveness of the host under it. Two screens means the second one is not
open at 19:30 on a Friday, which is the hour ADR-0110 and
[ADR-0001](0001-offline-first-store-autonomy.md) both keep pointing at. O2
([ADR-0073](0073-alerting.md), [`alerts/model.rs`](../../crates/pos-cloud/src/alerts/model.rs)) gains
one kind beside `StoreOffline` and `RelayBacklog`: `host_unreachable`, `Critical`, because every store
on a silent host is a hosted store, and ADR-0110 established that a hosted store nobody can reach is a
store that is not selling.

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
`pos_host` may depend on `pos-proto` for the job wire and on the signature verifier, and on nothing
else in the workspace — no `pos-core`, no `pos-edge`, no `store-sqlite`. That is a dependency list a
`cargo-deny`-style check can hold, exactly as ADR-0112 said of the print agent.

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

So a dead host is a **placement move**, run by a person, through the choreography ADR-0110 already
fixed and already made boring: stand the new placement up, activate it, bump the lease, wait for
`settled`, retire the old. Pressing Start on a host in another region is the "stand up" step. Nothing
else about the procedure changes, and it is the same procedure whether the store is moving between
hosts, from a host back into a shop, or from a shop onto a host.

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
  around five countries the arithmetic does not ask for one either: a placement is a long-lived fact
  about a shop, not a workload with a duration.
- **It makes no promise about a host pool the operator has not provisioned.** The Start button is
  offered only where a host with declared capacity exists in the chosen region; where none does, the
  console says so and offers `in-store` and `hosted-by-operator`. ADR-0110 kept the framework neutral
  on where an operator hosts, and this record does not smuggle a platform dependency back in through
  a greyed-out button. A fork that provisions no hosts loses one of three modes and nothing else.
- **It does not give the host a way into a running store.** No exec, no debug endpoint, no log tail
  into a container from the job wire. A host that can exec into a store's container can read that
  store's database, and then every isolation claim above is a claim about intent rather than about
  capability.
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
- **It does not fix Windows.** A hosted placement is a Linux placement, as ADR-0110 said; issue #182 is
  neither blocked by this record nor closed by it.
- **It does not put `pos_host` in the OTA rings.** The host ships on the cloud's cadence, because it is
  a platform machine. Putting a supervisor in the same rollout as the thing it supervises is how a
  canary takes out its own observer.
- **It does not choose a region, and does not say what may rest in one.** That is ADR-0114. This record
  hands it a pool to choose from.
- **It does not re-provision an existing store's scoped key.** The `ActivationGrant` field is filled on
  a *new* activation; every box already holding a key keeps it and keeps ADR-0086's env-override path.

## Consequences

- A third workspace binary, `pos_host`, permitted to depend on `pos-proto` and the signature verifier
  and nothing else in the workspace. [ADR-0002](0002-one-binary-per-tier.md) is amended to three tiers
  plus two named device-level artifacts, and gains the "decides nothing" rule that bounds the
  exception.
- The cloud serves four new store-family routes — `GET /sync/hosts/{host_id}/jobs`,
  `POST /sync/hosts/{host_id}/jobs/{job_id}/ack`, `POST /sync/hosts/{host_id}/heartbeat` and
  `GET /sync/hosts/{host_id}/artifact` — all behind one new deny-by-default scope,
  `Scope::RunHostJobs` (`run_host_jobs`). Internal to the fleet, so they stay out of
  [`docs/openapi.json`](../openapi.json) exactly as the other `/sync` routes do.
- `crates/adapters/store-postgres/migrations/0052_host_agents.sql`: the host registry, its declared
  capacity, and the job queue the long-poll reads — the `order_queue` shape one table over, RLS-isolated
  like every other cloud table, additive per [ADR-0017](0017-migrations.md).
- `RelayWake` gains a third and fourth waiter class for the job legs, and inherits the clamp that keeps
  the fallback re-read from computing to zero.
- `/admin/fleet` and `/admin/fleet/{store_id}` render hosts beside stores off one read model.
  `FleetStoreView` gains the host that runs the store. Online/offline stays derived at read time.
- O2 gains `AlertKind::HostUnreachable`, `Critical`, and one more row in the alert kind's `ALL`.
- Standing a store up in mode 3 is an `/admin` write and inherits what that implies:
  `ConsolePermission::ManageStores` ([ADR-0067](0067-multi-admin-console-rbac.md)) — the same
  permission that bumps the lease and sets placement, because they are one class of act — `If-Match`
  ([ADR-0094](0094-console-optimistic-concurrency.md)), and an audit entry naming the admin and the
  host ([ADR-0069](0069-audit-trail.md)).
- `ActivationGrant` gains an optional scoped key.
  [`activation.rs`](../../crates/pos-edge/src/activation.rs) stores it under `SecretName::SyncKey`
  before it announces success, in the order the vault-is-the-truth rule already requires. Additive,
  `Option`, no `PROTOCOL_VERSION` move.
- `EdgeConfig`'s environment grows `POS_EDGE_ACTIVATION_CODE`, read once by the boot gate when the
  vault is empty. It is not a config field and not a secret worth protecting: a spent code grants
  nothing.
- [`server.rs`](../../crates/pos-edge/src/server.rs) gains a bounded outbox flush in the shutdown path
  and a named log line when it does not finish; `TimeoutStopSec` in
  [`pos-edge.service`](../../deploy/edge/pos-edge.service) rises with the budget in the same change.
  ADR-0110's `settled` state stops depending on reading `outbox_depth` off a heartbeat and hoping.
- [`main.rs`](../../crates/pos-edge/src/main.rs) turns `ServeOutcome` into an exit code. `systemd` does
  not care, the Windows wrapper already reads the outcome, and a host agent finally can.
- `deploy/edge/Dockerfile`: a versionless runtime image with no release in it, and a fifth file under
  [`deploy/Caddyfile.d/`](../../deploy/Caddyfile.d/) for the per-store host site, importing the same
  shared part so the proxy configuration still exists once.
- **The word `placement` now has two meanings in the tree and one of them is older.**
  [`ota_state.rs`](../../crates/pos-edge/src/ota_state.rs) calls the `device_ota` ring-and-canary
  assignment a placement — *"no placement means the device cannot be weighed"* — and ADR-0110 gave the
  word to the store attribute. This record reads both in the same paragraph, so the OTA one is renamed
  to `assignment` (its type is already `DeviceOtaAssignment`) in the same change. A word that means two
  things is a bug report waiting to be misfiled.
- [`docs/glossary.md`](../glossary.md) gains **Host agent** — *the supervisor that starts and stops
  platform-hosted edges on one machine* — beside ADR-0110's **Placement** and ADR-0112's **Print
  agent**.
- Nothing published is removed or renamed. One scope, four routes, one migration, one alert kind, one
  optional grant field, one environment variable, one exit code and one runtime image are all
  additions, and no `PROTOCOL_VERSION` moves — the edge is not told it is hosted, because it does not
  need to know.
