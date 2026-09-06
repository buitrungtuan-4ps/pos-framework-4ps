# ADR-0112 — A paired device may own a printer's transport, and the edge still renders every byte

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-06
· Moves the last hop of [ADR-0103](0103-directly-attached-printers.md) and
[ADR-0100](0100-receipt-and-ticket-printing.md) without moving anything above it
· Required by [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)'s hosted edge placements
· Rests on [ADR-0030](0030-pairing-and-offline-auth.md) for who may ask,
[ADR-0084](0084-device-authentication.md) for the two gates, and
[ADR-0018](0018-http-websocket-stack.md) for what the fan-out is and is not
· Takes its delivery shape from [ADR-0062](0062-the-relay-wake.md)'s wake over a durable queue
· Serves the four `/api/*` routes [ADR-0111](0111-a-second-origin-may-address-the-edge.md) covers
· Amended by [ADR-0113](0113-the-host-agent.md), which reopens ADR-0002 a second time
· Relates to [ADR-0102](0102-printing-any-script.md), [ADR-0104](0104-multi-component-and-inclusive-tax.md),
[ADR-0106](0106-the-store-is-a-legal-person.md), [ADR-0107](0107-the-buyer-is-a-subject.md),
[ADR-0041](0041-device-onboarding.md), [ADR-0072](0072-floor-and-kitchen.md),
[ADR-0073](0073-alerting.md), [ADR-0068](0068-fleet-liveness.md),
[ADR-0114](0114-region-is-required-recorded-visible.md)

Third of the five records on the **Edge Anywhere** programme, after
[ADR-0110](0110-edge-placement-is-a-deployment-axis.md) and
[ADR-0111](0111-a-second-origin-may-address-the-edge.md), before
[ADR-0113](0113-the-host-agent.md) and
[ADR-0114](0114-region-is-required-recorded-visible.md). All five are on disk, so all five are
linked.

## Delivery — 2026-09-06, the queue

The **durable queue** shipped: migration
[`0009_print_jobs.sql`](../../crates/adapters/store-sqlite/migrations/0009_print_jobs.sql) and the
four operations over it in
[`store-sqlite`](../../crates/adapters/store-sqlite/src/writer.rs) — enqueue, claim, acknowledge,
expire — proven against real SQLite in
[`tests/print_queue.rs`](../../crates/adapters/store-sqlite/tests/print_queue.rs).

Two things this record specified turned out to be load-bearing in a way worth recording, because the
first wiring got one of them wrong:

- **"One job per printer at a time" is not "exclude the claimed row."** Filtering only the leased row
  leaves the printer's *next* job claimable, so an agent holding one job is handed a second — two
  writers to one print head, which is the interleaved-bytes failure this record cites as the reason
  for the rule in the first place. The claim skips any printer holding an unexpired live claim,
  whole. A test asserting that a second claim returns nothing is what caught it.
- **The cap must ignore expired rows.** Counting them would leave a printer that jammed for longer
  than the TTL permanently refusing work, on the strength of tickets nobody will ever collect.

**The five constants are not here.** The TTL, the claim lease and the allowance are the caller's:
this adapter holds no clock and no policy, and takes computed instants and a cap. That is what keeps
them in one edge module, where changing one is a release rather than a schema change — and it is why
the tests exercise the boundary with a cap of two rather than writing two hundred rows.

**`AgentUnavailable` is deliberately not an outcome of the enqueue.** It is decided *before* the
table is touched, from the agent binding and when it was last heard from — facts the queue does not
hold — and the ordering is part of the decision: a queue must not start building behind a box that is
not there.

**Not in this slice.** `agent_device_id` and `DeviceKind::Terminal`; `PrintWake` and the two claim
routes; the two gates on binding a terminal; the agent binary, the console entry and the silence
alert. Nothing enqueues yet — `dispatch` still opens every transport directly, so every store prints
exactly as it did.

## Delivery — 2026-09-06, a printer names its agent

The **binding** shipped, cloud-side and on the wire. `PublishedDevice` gained
`agent_device_id: Option<DeviceId>` and `DeviceKind` gained `TERMINAL`
([`devices.rs`](../../crates/pos-proto/src/devices.rs)); `pos_cloud::devices::DeviceKind` learned
`Terminal` / `"terminal"`; migration
[`0056_device_agent.sql`](../../crates/adapters/store-postgres/migrations/0056_device_agent.sql)
added the nullable column; `compile_devices` carries the pointer and publishes a terminal with an
empty address and `DEVICE_CONNECTION_UNSPECIFIED`; and `POST /admin/devices/publish` refuses a node
whose `agent_device_id` does not resolve to a `TERMINAL` in that node.

**Both console writes shipped with it, because the field has no other author.** `POST
/admin/devices/terminals` writes the approved `TERMINAL` row directly, and `POST
/admin/devices/proposals/{id}/agent` is the picker — both behind `ConsolePermission::ManageDevices`
with an audit entry, and the picker with an `If-Match` ([ADR-0094](0094-console-optimistic-concurrency.md))
as this record specified. Landing the node field without them would have shipped a wire field nobody
could fill.

Three things are worth recording because scoping settled them:

- **The picker does not check that its target is a terminal, and that is not a gap.** *Resolvable* is
  a property of the set being published, not of either row: the terminal may be created after the
  pick, or archived before the publish. Only the compile sees the whole node at once, so that is
  where the check lives — and refusing the *publish* is what makes "a paired phone can never be a
  print agent" true at the store, because an unresolvable reference never leaves the cloud.
- **The version comes from `xmin`, so the `If-Match` cost no schema.** The registry's conditional
  writes already read `xmin::text` as an opaque token; `device_proposals` needed only to select it.
  `resolve` (approve/reject) stays unconditional — it acts on `pending` rows and the picker on
  `approved` ones, so the two cannot race for the same row, and making an existing route conditional
  would be a behaviour change to the console this slice did not ask for.
- **The connectionless-row skip gained one exception and kept its scope.** That rule exists to stop a
  USB printer's cash drawer being published as a network device's; a terminal has neither a drawer
  nor an address, and `DEVICE_CONNECTION_UNSPECIFIED` is the accurate answer rather than a guess. If
  the skip had taken terminals, no `agent_device_id` could ever have resolved.

**Not in this slice.** `PrintWake` and the two claim routes; `POST /api/print/agent` and its revoke;
the agent binary, the console screen and the silence alert. `dispatch` still opens every transport
directly, and no store's node carries an agent until an operator picks one — so every store prints
exactly as it did.

## Delivery — 2026-09-06, the two gates on binding a terminal

**The slices swapped, and the reason is in the record.** This was written as the fourth slice and is
shipped as the third, because the enqueue's *first* question is the agent binding — "unclaimed, or
not heard from within `AGENT_SILENCE`, refuse before the table is touched" — and every agent route
has to turn the paired device that called it into the agent it speaks for. Shipping the claim and
acknowledge routes first would have shipped a surface that could only ever refuse. Nothing about
either slice's content changed; the order did.

`POST /api/print/agent` and `POST /api/print/agent/revoke`
([`http/print_agent.rs`](../../crates/pos-edge/src/http/print_agent.rs)) sit behind the paired-device
gate **and** an employee signed in holding `Permission::ManageDevices`, checked in the handler
against the roster the cloud published. The binding is migration
[`0010_print_agents.sql`](../../crates/adapters/store-sqlite/migrations/0010_print_agents.sql) and
the [`PrintAgents`](../../crates/pos-edge/src/print_agent.rs) seam over it — the shape
`QueueNumberAuthority` and `LeaseAuthority` already take, implemented for `SqliteStore` and again in
memory for the example and the tests.

Four things settled during the work:

- **Exclusive in both directions, and both enforced in the schema.** A primary key on the terminal
  and a unique index on the paired device. One terminal, one box, for the reason this record gives;
  and one box, one terminal, because a terminal *is* a machine and so is a paired device, so a box
  answering for two would invent a machine that is not in the shop.
- **Re-claiming from the holder is a refresh, not a conflict.** An agent that restarts and re-claims
  the identity it already holds must not need a manager at the box a second time. The refusals are
  for a *different* device.
- **The permission is checked at the route, not at a decide.** A binding produces no event: it is
  durable edge-local state, like the pairing it records against, not a fact about the business. That
  needed a by-employee-id lookup on the roster — sign-in is keyed by badge code, everything after it
  carries an `EmployeeId` — so `StaffRoster` gained one.
- **`last_seen_at` is written by the agent's own claims against the queue, not by a ping of its own.**
  A separate liveness ping is a second thing that can be true while printing is broken. The act that
  proves an agent is there is asking for work.

**Not in this slice.** `PrintWake`, `GET /api/print/jobs`, the acknowledge route, and the `dispatch`
branch that makes the queue live — the slice this one swapped with, now next. Nothing enqueues yet.
A store that binds an agent today changes nothing about where its tickets print.

## Delivery — 2026-09-06, the agent claims, acknowledges, and the edge enqueues

The queue is live. `GET /api/print/jobs` and `POST /api/print/jobs/{job_id}/ack`
([`http/print_jobs.rs`](../../crates/pos-edge/src/http/print_jobs.rs)) sit behind the paired-device
gate and no second one; the branch in
[`Printers::dispatch`](../../crates/pos-edge/src/printing.rs) sends a job whose printer names an
agent to that agent's queue and leaves every other printer on the path
[ADR-0103](0103-directly-attached-printers.md) shipped. `PrintOutcome` gained the three answers this
record specified — `QUEUED_TO_AGENT`, `PRINT_AGENT_UNAVAILABLE`, `PRINT_QUEUE_FULL` — and the till
says which.

Five things settled during the work, three of them departures from what this record sketched:

- **`PrintWake` is a `broadcast`, not a `Notify`.** This record sketches `tokio::sync::Notify` and
  also states the rule *"a waiter subscribes before it reads, never after"*. A bare `Notify` cannot
  honour that rule: `notify_waiters` stores no permit and wakes only waiters already parked, so a
  signal landing between the subscribe and the first poll is lost — which here is a ticket sitting
  in the queue for the whole park while a guest waits at a table. A `broadcast::Receiver` buffers
  from the moment it subscribes, which is exactly the property the rule needs. This is the same
  choice, for the same reason, that [`pos_cloud::wake`](../../crates/pos-cloud/src/wake.rs) made
  under [ADR-0062](0062-the-relay-wake.md). The sketch is superseded; the rule it was serving is
  not.
- **The acknowledgement is scoped to the holding agent in the statement, not by a check first.**
  An acknowledgement *deletes* a document that exists nowhere else. A check and a delete are two
  steps a race can get between, so the scope is in the `DELETE`'s own `WHERE`. Without it, any
  paired device answering for any terminal could delete another's ticket unprinted — a dish the
  kitchen never sees, on a machine nobody is looking at.
- **Liveness is stamped by an `UPDATE` that can never insert.** This record says `last_seen_at` is
  written by the act that proves liveness, and that act — asking for work — runs constantly. It can
  land in the same second a manager releases the terminal at the till. An upsert would put back what
  the revoke had just taken away, so `PrintAgents::heard_from` updates a binding that is there and
  reports `false` when it is not. A revoke a busy agent can undo is not a revoke.
- **A rendered `PrintJob` is `serde`-serialisable, in `pos-ports`, once.** The queue row holds the
  finished document, so something has to encode it. It carries the derives where the type already
  lives rather than in a second wire shape somewhere else: two definitions of a printed document is
  two renderings of a legal document that can disagree, which is the thing four paragraphs of this
  record are about. A raster goes as hex rather than as a JSON array of numbers — two characters a
  byte instead of three or four, and one field instead of a thousand.
- **`AgentDispatch` is dyn-compatible where the three seams under it are not.** `PrintAgents`,
  `PrintQueue` and `PrintWake` all return `impl Future`, and `Printers` is held as one
  `Arc<Printers>` in an axum extension by every composition and every route test. Erasing once, at
  the point where the three become a single question, keeps three generic parameters out of the
  signature of every caller that only ever wanted to print a receipt.

Two properties are pinned by tests that were verified red first: a job enqueued while an agent is
parked reaches it without waiting out `AGENT_PARK` (remove the signal and the test times out), and
a second concurrent request on one binding is answered rather than parked (remove the guard and it
takes the full park).

**Not in this slice.** The `pos_print_agent` binary itself, the console's `TERMINAL` entry and agent
picker, the drawer following the printer, and silence reported to the console before the night ends.
A store can bind an agent and the edge will queue for it; nothing ships yet that claims from that
queue in the field.

### Correction — 2026-09-06, the leased job had no address

The slice above shipped `GET /api/print/jobs` handing over the job id and the bytes and **not the
address**, against this record's own sentence three sections up: *"Its whole contract is open this
address, write these bytes, report this id."* No agent could have been written against that
response. A leased job now carries the printer's `address`, its `connection`, and the
`PrinterCapabilities` the edge assumed when it prepared the document.

All three are resolved at claim time from the live published `devices` node rather than stored on
the queue row, because a printer re-addressed since the job was queued should be dialled where it is
now. And the capabilities are the **edge's**, which is the same argument this record makes four
paragraphs above about rendering, read one step further: an agent computing its own would be a
second opinion about a printer's column width, and one whose `prints_bitmaps` disagreed would refuse
a raster the edge had just drawn for it.

A job whose printer the store no longer publishes is skipped rather than handed over with an address
invented at the route. There is nowhere to send it; it expires at its TTL like any other
undeliverable job, and the agent's other printers still get their work.

## Delivery — 2026-09-06, the agent binary

`pos_print_agent` exists: [`crates/pos-print-agent/`](../../crates/pos-print-agent/), a third binary
this record already argued for and [ADR-0113](0113-the-host-agent.md) named as one of ADR-0002's two
exceptions. It claims, writes, and acknowledges, and it decides nothing.

- **The three-crate rule is a gate, not a comment.** This record said the list *"is a rule a
  `cargo-deny`-style check can hold"*, so it is one:
  [`xtask print-agent-deps`](../../xtask/src/checks/print_agent_deps.rs) refuses any workspace
  dependency outside `printer-escpos`, `pos-ports`, `pos-proto` — dev-dependencies included, because
  a test build that can reach the domain is a test build that invites the domain in. It runs in the
  `rules` CI job and in `just preflight`.
- **The order of the three writes is the whole design, and it is tested as such.** Write the bytes,
  **record the id, then acknowledge**. A crash between the record and the acknowledgement is the
  case that ordering exists for: the job returns at the claim lease, the record answers *already
  written*, and the agent acknowledges without printing a second ticket. A write that *fails* is
  never acknowledged, so the lease returns it — which is what a printer out of paper needs. Both are
  pinned by tests over a stubbed edge and print head, each verified red first.
- **A corrupt record starts empty rather than refusing to run.** A kitchen with no tickets is worse
  than one duplicate ticket, and this record already accepts one duplicate as the cost of losing the
  file entirely.
- **One printer's jam does not hold up another printer's ticket.** The claim returns at most one job
  per printer but an agent may own several, and a cycle that gave up on the first refusal would let
  a jammed kitchen printer stall the counter's receipt.
- **`http` and `https` both, and nothing else.** [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)
  made where the edge runs a deployment axis: in-store it is a box on the shop LAN behind no proxy,
  hosted it is behind the same TLS terminator as everything else. Which one a store uses is a fact
  about that store, not a posture this binary holds — but a third scheme is refused at construction,
  where an operator can still read the message. The client is ADR-0054's stack, so the agent adds no
  third-party subtree the tree does not already build.
- **`--self-test` deliberately opens no printer and does not dial the edge.** It answers *can these
  bytes run on this box, and can they read this machine's configuration* — the same question
  [ADR-0055](0055-edge-ota-updater.md) Amendment 1 has the edge's self-test answer. A printer that is
  off is an ordinary state of the world, not a fact about a staged binary.

**Not in this slice.** The generated installer — this record's *"rides the same rail, and gets its
installer from the same generator"* is still a claim about where it will live rather than a file on
disk; the agent's shape (no update slots, no store key, no database) is different enough from the
edge's that it wants its own generator function and its own drift check. Also still to come: silence
reported to the console before the night ends, and the console's `TERMINAL` entry and agent picker.
Until the installer lands, an agent is installed the way any small service is: put the binary
somewhere, write `print-agent.toml`, and register it.

### Correction — 2026-09-06, the installer landed and moved the token

The installer above is now on the rail this record named: `printAgentInstallerTemplate()` in
[`installers.mjs`](../../dashboard/src/installers.mjs), the generated
[`install-pos-print-agent.ps1`](../../deploy/edge/install-pos-print-agent.ps1), and
[`pos-print-agent.service`](../../deploy/edge/pos-print-agent.service) for Linux. The drift check and
the PowerShell parse gate that already covered the edge's installer now cover this one too — a second
elevated script running on a machine in a shop earns the same gate for the same reason.

**Writing it moved the device token out of the configuration file**, which is the kind of correction
an installer forces. The agent's `Config` required `device_token` in `print-agent.toml`; the
installers put credentials in the service's own environment, and the two would not have worked
together. The token now comes from `POS_PRINT_AGENT_TOKEN` — a registry key readable only by accounts
that can read the service on Windows, a root-owned `EnvironmentFile` on Linux — and the file remains
a fallback so a technician bringing one terminal up by hand needs no second mechanism. That is the
split the edge already makes for `POS_EDGE_SYNC_KEY` under
[ADR-0086](0086-edge-keyvault-and-activation.md); the config file beside the state is read by whoever is diagnosing
a printer, and a credential does not belong in it.

The unit is not a copy of the edge's, and the differences are all the same difference: **no update
slots**, because the agent holds nothing that a reinstall would lose beyond one duplicated ticket;
**no store key and no database**; a short `TimeoutStopSec`, because a stop mid-job leaves that job
unacknowledged and the queue hands it back at the lease, so there is nothing to drain. The one thing
copied unchanged is the restart posture, on both platforms, because a crashed agent is a kitchen
printing nothing while the till keeps reporting `QUEUED_TO_AGENT` — the queue doing exactly its job,
which is what makes the failure quiet.

## The problem

### Delivery note — 2026-09-06, silence reaches the console

The second half of *"silence is reported twice"* is built. The heartbeat carries one standing per
bound agent — the terminal, the paired device answering for it, and the age of its oldest
unacknowledged job — and O2's evaluator gains `print_agent_stalled`, firing `Critical` at five
minutes exactly as this record specified.

Three decisions the section above did not settle, each because writing it raised the question:

- **The field is an `Option` around a list, not a list.** *Absent* and *empty* are different claims
  and only one of them is safe to treat as "nothing to do". Absent is a box that did not look — an
  older edge, or one whose record would not read — and leaves what the cloud knew alone. Empty is a
  box that looked and has no bound agent, which is what a manager releasing the last terminal
  produces, and it has to **replace** the stored list: a console still showing an agent nobody is
  bound to is precisely the stale answer this field exists to prevent. The same distinction
  `outbox_depth` draws between `None` and `0` ([ADR-0068](0068-fleet-liveness.md)), one level down.
- **The alert is judged on the age the store reported, not on the cloud's `now`.** That age was
  measured against the store's own clock at the moment of the beat; re-ageing it here would add the
  heartbeat interval and the cloud's skew to a number an operator reads as *how long the kitchen has
  been waiting*, and would keep growing after the store stopped reporting at all — turning a lost
  link into a fictional print backlog. A store that has gone silent is `store_offline`'s to raise,
  and [ADR-0110](0110-edge-placement-is-a-deployment-axis.md) already decides how loudly.
- **One alert per agent, keyed on the agent.** A shop with a jammed kitchen printer and a healthy
  counter has one problem. Rolling the standings up to the store would either hide the jam or
  condemn the counter with it.

The standings are a **JSON column on `store_liveness`**, not a table. The list is a snapshot the
store replaces wholesale on every beat; nothing joins to it, nothing queries by agent id, and the
console renders the whole list at once. A table would buy an index nothing reads and cost a
transactional delete-then-insert and a second RLS policy on every heartbeat.

The edge's read is one `LEFT JOIN`, so a bound agent with an empty queue is a row carrying nothing
rather than a missing row — *this terminal has an agent and it is keeping up* and *nobody ever bound
this terminal* are answers a console must be able to tell apart. Expired jobs are excluded on the
same clock the claim uses: a ticket the TTL has already given up on must not keep an alert alive
after the thing it was about stopped mattering.

Four properties are pinned by tests verified red first: the ms→seconds arithmetic and its
empty-queue case; the `LEFT JOIN` and the TTL exclusion, against a real database; the five-minute
threshold and the `Critical` severity; and the absent-versus-empty rule, which fails the moment the
route collapses the two.

**Not in this slice.** The console's `TERMINAL` entry and agent picker, over the `create_terminal`
and `set_print_agent` routes that shipped in slice 2 and still have no console surface — which is
why [`deploy/edge/README.md`](../../deploy/edge/README.md)'s first precondition is still met by hand.
The drawer following the printer also remains where the section above left it: this record settles
which machine the kick lands on, and ADR-0103's missing console field still means no drawer opens
anywhere.

### Delivery note — 2026-09-06, the console entry and the agent picker

The last piece of slice 5. A terminal can be created from the console, and an approved printer can
be pointed at one — the two routes that shipped in slices 2 and 4 with no surface to drive them.

**A blocker found by reading, not by a failing test: there was no `/admin` read the picker could be
built on.** Every `/admin/devices` route is a write except `GET /admin/devices/proposals`, and that
one was hard-coded to `DeviceProposalStatus::Pending`. Both halves of a binding are `approved` — the
printer already in service, the terminal already created — so the onboarding queue could show
neither, and `list_store_devices` sits on `/sync` behind the store's own API key, where a console
cannot reach it. The seam underneath already took both filters (`list(tenant, store, status)`), so
the fix is additive: optional `store_id` and `status` query parameters, defaulting to `pending` so
every existing caller keeps exactly the rows it had. An unknown status token is refused **by name**
rather than narrowed to the default — a caller who mistypes `aproved` and is handed the pending
queue would read the empty result as *this store has no printers*.

The console is two store-scoped cards below the onboarding queue, both fed by one read of the
store's approved devices, split by kind:

- **Terminals** — the create. A name and nothing else: nothing dials a terminal, so it has no
  address, and the admin who creates the entry is the human gate ADR-0041's approval step exists to
  be.
- **Print agents** — the pick. Each approved printer shows the terminal writing its bytes, or *the
  edge prints it*, and the picker offers that store's terminals plus the same "none". The write
  carries the version the row was read at ([ADR-0094](0094-console-optimistic-concurrency.md)):
  two managers picking different agents for one printer is the ordinary race, and last-write-wins
  would leave the loser believing a decision that is not in the database.

They follow the **store** in the top bar rather than the tenant, because an agent may only be a
terminal standing in the same shop — a tenant-wide picker would offer picks `admin_publish_devices`
will refuse. A pick that no longer resolves renders as the raw id rather than as *no agent*: "none"
is a different and much quieter state, and the two must not be confused on the screen that sets them.

Both new parameters are pinned by a route test verified red first — the status filter, and the
unchanged pending default — because the default is the whole reason this could be added to a
shipped read rather than a new one.

**Not in this slice.** The drawer following the printer, exactly where the section above left it:
this record settles which machine the kick lands on, and ADR-0103's missing console field still
means no drawer opens anywhere.

### The last hop assumes one building, and everything above it does not

[ADR-0103](0103-directly-attached-printers.md) decided that a USB or serial printer is reached by
opening its OS device path — `/dev/usb/lp0`, `/dev/ttyUSB0`, `\\.\COM3` — and writing bytes to it.
[`printer_escpos::device::DeviceTransport`](../../crates/adapters/printer-escpos/src/device.rs) does
exactly that, and [`printing.rs`](../../crates/pos-edge/src/printing.rs) picks it in `TcpTransports`:

```rust
PrinterConnection::Usb | PrinterConnection::Serial => {
    Ok(Box::new(DeviceTransport::new(&device.address)))
}
```

That line is correct and it is the only line in the print path that cares which building the process
is in. A network printer is no better off: `TcpTransport` dials `192.168.1.50:9100`, an address that
means something on the shop LAN and nothing from a data centre.

Above that line, nothing cares. The dispatcher selects a device, `prepare` turns every line the
printer's own character set cannot carry into a raster ([ADR-0102](0102-printing-any-script.md)),
`receipt_document` composes the seller block, the per-rate tax section and the buyer block
([ADR-0104](0104-multi-component-and-inclusive-tax.md), [ADR-0106](0106-the-store-is-a-legal-person.md),
[ADR-0107](0107-the-buyer-is-a-subject.md)), and the result is a `PrintJob` — a list of text and
bitmap blocks with a `job_id`. Every one of those steps is edge-placement-blind Rust over rows the
edge already holds.

So the problem is exactly one function call wide. [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)
named it and refused to solve it: *"It does not change the printer's last hop … Dispatching print
from a designated POS terminal, and what happens to the drawer, is ADR-0112."*

### A kitchen ticket is not a notification

A hosted store that cannot print is not a degraded store, it is a closed kitchen. A ticket that does
not print is a dish that is not cooked, and unlike a receipt there is no customer standing there to
notice. The KDS is not the answer either — [ADR-0100](0100-receipt-and-ticket-printing.md) says *"the
KDS is unaffected throughout: it is a screen, and it has always had the order"*, which is true and is
why a kitchen that works off paper cannot be told to look at a screen it does not have.

The receipt is worse in a different direction. In Vietnam it is a legal artefact, not a convenience
— ADR-0100 says so in the sentence that justified putting printer addresses in the config tree — and
[ADR-0106](0106-the-store-is-a-legal-person.md) put the seller's registration on it.

### The pipe that already reaches every device is the wrong pipe

The edge already pushes committed changes to every device in the store over one
[`tokio::sync::broadcast`](../../crates/pos-edge/src/fanout.rs) channel. It is the obvious carrier and
it is wrong three times over, and each reason is written into the code that implements it.

It is a **broadcast**. Every subscriber gets every frame. A receipt document may carry a buyer's name
and tax code — `printing.rs` says so where it refuses to log one — and putting that on a channel
every paired tablet reads is the [`AGENTS.md`](../../AGENTS.md) §2 rule against PII in payloads,
routed around by choosing a different envelope.

It is **lossy on purpose**. `FANOUT_CAPACITY` is 256 frames and a subscriber that falls behind gets
`RecvError::Lagged` and is told to reload:

```rust
Err(RecvError::Lagged(_)) => {
```

[`ws.rs`](../../crates/pos-edge/src/http/ws.rs) answers that with `ServerMessage::Resync`, and
`fanout.rs` names the subscriber it is designed for: *"a tablet asleep in a drawer"*. Resync works
because every other frame describes state the device can refetch. A print job describes state that
exists nowhere else. A resynchronising till reloads the floor; a resynchronising printer prints
nothing, and there is no snapshot of a ticket to come back to.

It is **in-process**. The channel dies with the process. A restart — an OTA install deliberately
restarts the edge ([ADR-0055](0055-edge-ota-updater.md)) — takes every undelivered job with it.

### Nothing in the tree says which machine owns a printer

`PublishedDevice` carries `device_id`, `kind`, `connection`, `address`, `name` and an optional
`station_id`, and [`devices.rs`](../../crates/pos-proto/src/devices.rs) documents `address` as
*"`host:port` for a network printer, an OS device path for USB or serial"*. Whose OS is not asked,
because until now there was only one answer.

## The decision

**A printer may name the device that owns its transport. That device claims rendered bytes from a
durable queue on the edge, writes them, and says the job id back. Everything above the write stays on
the edge, unchanged, in every edge placement.**

### The edge renders every byte, and a device that rendered any of them would be wrong

The agent receives a finished `PrintJob`: text blocks the printer's code page covers, bitmap blocks
for everything it does not, a cut, and an id. It decides nothing. Its whole contract is *open this
address, write these bytes, report this id*.

Putting any rendering on the device fails on four independent counts.

**Fonts.** [ADR-0102](0102-printing-any-script.md) rasterises in
[`pos-render`](../../crates/pos-render/) — today with `harfrust` and `skrifa`, the maintained
successors that replaced ADR-0102's `rustybuzz` and `ttf-parser` when RUSTSEC-2026-0206 and
RUSTSEC-2026-0192 declared both unmaintained
([`docs/dependency-review-2026-09.md`](../dependency-review-2026-09.md)) — against fonts installed on
the machine, at a width taken from `PrinterCapabilities::dots_per_line`. A device that drew its own
glyphs would draw them from whatever fonts that device happens to have, so a store with three
terminals would print three different tickets from one order — and `printing.rs` already warns when
*no installed font covers these characters*, a diagnosis that is worthless if the answer differs per
tablet. The raster width is worse: `ASSUMED_DOTS_PER_LINE` is 576, and a device guessing a different
figure renders a raster wider than the print head, which shears.

**Tax law.** The receipt is a legal document. ADR-0104's components, ADR-0106's registration line and
ADR-0107's buyer block are country law expressed in Rust in one place. Rendering them twice is
holding two legal opinions and discovering the difference in an audit.

**Shaping.** ADR-0102 is explicit that Indic scripts *"cannot be done by a code page **in
principle**"* — the vowel sign is written before the consonant it follows and consonants form
conjuncts that are not any of their parts. That is the HarfBuzz algorithm, and a second
implementation of it in a second language would disagree with the first in exactly the cases nobody
tests.

**`ui/` is a view.** [`client.ts`](../../ui/src/api/client.ts) opens with the whole posture: *"Every
call is one `fetch` to the same origin that served the app."* The screens render what the edge
returns. ADR-0110 declined a device-local write buffer partly because it would need *"a second
definition of the domain, in a language with no `pos-core`, tested by nobody, and free to disagree
with the first"*, and a device-side renderer is that same mistake at a smaller scale with a legal
document as the output. ADR-0110's constraint on this whole programme settles it: the axis *"may
change where a byte is written — the printer's last hop, the origin a browser talks to — and may
never change what the byte says."* This record moves the write. It moves nothing else.

### A printer names its agent, and absent means the edge

`PublishedDevice` gains one optional field:

```rust
/// The device whose transport reaches this printer. Absent: the edge opens the address itself.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub agent_device_id: Option<DeviceId>,
```

and `DeviceKind` gains one variant, `Terminal = "TERMINAL"`, for the entry an `agent_device_id`
points at. Both are additive, which is the [`AGENTS.md`](../../AGENTS.md) §2 rule and also the point:
**absent means the edge is the agent**, so a store whose `edge_placement` is `in-store` configures
nothing, changes nothing, and runs exactly [ADR-0103](0103-directly-attached-printers.md). A fleet of
five hundred stores takes this release and prints tomorrow the way it printed today, because for
every one of them the field is not there.

A `TERMINAL` entry's `address` is empty, because nothing dials it, and its `connection` is
`DEVICE_CONNECTION_UNSPECIFIED`, which is what that zero value is for. The edge never opens a
connection to an agent; the agent opens one to the edge. That is `AGENTS.md` §1's outbound rule read
one layer further in, and it buys the same thing at the store scale that it bought at the fleet
scale: no inbound port on a till, no NAT to traverse, no discovery, and nothing to configure when the
till's DHCP lease changes.

The alternative — a separate `print_agents` node, or an `agent_id` on the station plan — puts the
binding somewhere other than on the thing being bound. A printer's agent is a fact about that
printer, in the same category as its connection and its address, and those already live on the device
entry. Splitting them means two nodes that can disagree about a device, and the config tree's
never-blank rule ([ADR-0033](0033-config-tree.md), applied node by node in
[ADR-0072](0072-floor-and-kitchen.md) and [ADR-0074](0074-localization-and-tax.md)) makes
disagreement durable: one node updates, the other keeps its previous value, and the store prints
somewhere nobody chose.

An older edge that does not know the field ignores it and opens the address itself, which is the safe
direction: in-store that is the behaviour it already had, and in a hosted edge placement it opens a
device path that is not there and reports `PRINTER_UNAVAILABLE` — a named refusal, not silence.

### A terminal is created in the console, because nothing discovers one

`agent_device_id` holds a **cloud-approved device id** — the same identity space every other
`PublishedDevice.device_id` lives in, which
[`devices.rs`](../../crates/pos-proto/src/devices.rs) documents as *"the device's identity, as the
cloud's approval assigned it"*. It is not the locally minted id a pairing produces, and
[`pairing.rs`](../../crates/pos-edge/src/pairing.rs) says so in as many words: a paired device's id is
*"unique per pairing; the cloud's approved-device registry (propose→approve) is a separate identity
this local id does not claim to be."* Those are two identity spaces, this record uses both, and the
next section is entirely about the seam between them.

**A `TERMINAL` entry is created by an operator in the console, not proposed by a store.**
[ADR-0041](0041-device-onboarding.md)'s discover → propose → admin-approves flow exists because a
port-9100 printer answers an mDNS query and must not become live on that basis; the human gate is
what turns a discovery into a device. A terminal is the opposite case. Nothing on the LAN announces
itself as a POS terminal, `pos-edge` proposes nothing at all in this build —
[`discovery.rs`](../../crates/pos-edge/src/discovery.rs) ships the no-op advertiser and says
*"discovery is the raw-IP URL in this build"* — and the operator knows the machine exists before it
is plugged in, because somebody carried it into the shop. So the console writes the row directly, as
an approved device, and the named admin who created it **is** the human gate ADR-0041 was protecting.
The proposal path keeps its two kinds and its meaning; it is not stretched to cover a device no store
can discover.

That costs three additive cloud-side changes, named in Consequences: a `terminal` token on
`pos_cloud::devices::DeviceKind` and in the `device_proposals.kind` column (a value, not a schema
change — the column is `text NOT NULL` with no check constraint), a create route on the onboarding
router, and a `compile_devices` that carries the row into the published node. The publish handler's
existing rule that a connectionless row is *skipped rather than guessed* keeps its scope: it protects
a USB printer's cash drawer from being silently published as `network`, and a terminal has no drawer
and no address to guess.

### Two gates, and what each one proves

**The designated agent is a fixed, mains-powered terminal, and never a waiter's phone.** That is a
rule, not advice, and it is enforced in two places because one is not enough. Be precise about what
each place proves, because the two identity spaces above mean a loose claim here is a hole.

**The console proves the entry.** An `agent_device_id` may only name a `TERMINAL` device in the same
published node. Creating that entry is a console write by a named admin holding
`ConsolePermission::ManageDevices`, with an `If-Match`
([ADR-0094](0094-console-optimistic-concurrency.md)) and an audit entry
([ADR-0069](0069-audit-trail.md)); naming it on a printer is the same. `POST /admin/devices/publish`
refuses a node whose `agent_device_id` does not resolve to a `TERMINAL` in that node, so an
unresolvable reference never reaches a store. A phone that paired with the store is a paired device
and nothing else: there is no approved entry for it, so no printer can name it.

**The store proves the act.** A paired device claims an agent identity once, at the box, over `POST
/api/print/agent`, naming an `agent_device_id` from the published node — and that route requires a
paired device **and** an employee signed in on it holding `Permission::ManageDevices`
(`admin.device.manage`, Manager and Owner by default, declared `High` risk). Binding a terminal is a
managerial act performed in front of the machine, not something a process does on its own behalf. The
edge records the binding durably — cloud-approved id to locally minted paired id, with who claimed it
and when — before it answers, the same ordering
[`pairing.rs`](../../crates/pos-edge/src/pairing.rs) already uses, which records a device *before*
returning its token so a crash between the two leaves the operator claiming again rather than holding
a credential the box forgot. The binding is exclusive: a second paired device claiming a held agent
is refused, not silently promoted. `POST /api/print/agent/revoke` releases it behind the same two
gates, and a release is how a dead terminal is replaced.

Take-over-by-latest is the tempting simplification and it is wrong for the reason the whole framework
keeps rediscovering. Two devices holding one agent identity both claim from the same queue, so each
ticket prints exactly once — on whichever box grabbed it. If both are at the counter, nothing is
visibly wrong. If one is a phone in an apron, half the kitchen's tickets are in a pocket, and nobody
finds out until service. Refusing is visible; splitting is not.

**What neither place proves is which physical machine is on the other end**, and pretending otherwise
would be the more comfortable sentence to write. The framework has no device attestation: a paired
device is whatever holds a token, and a manager who signs in on a phone and claims a terminal entry
gets a phone as the agent. What the two gates buy is that this cannot happen casually or by a waiter
— it takes an approved entry a console admin created, a manager's PIN at the box, and a deliberate
exclusive claim — and that it cannot happen invisibly, because the heartbeat reports which paired
device holds each terminal entry and the console shows it. That is a policy enforced by permission
and visible in the console, not a proof carried by hardware, and the difference is stated here rather
than assumed by a reader.

### The queue is a table on the store's SQLite, capped and time-limited

A tablet sleeps, a terminal is rebooted mid-service, a USB cable is knocked out. A queue in the
agent's memory loses jobs to all three, and a queue in the edge's memory loses them to a restart. So
the queue is a new table in a new additive migration, `0009_print_jobs.sql`, in the store's own SQLite
— the durable, edge-local category `store_lease`, `intake_ledger` and the queue-number counter
already occupy, written through the single writer thread that serialises every other allocation.

It is a **side record, not an event**. `AGENTS.md` §2 forbids PII in an event payload, a rendered
receipt may carry a buyer's name and tax code, and the row holds the rendered document. It is the
same category as [`0004_intake_ledger.sql`](../../crates/adapters/store-sqlite/migrations/0004_intake_ledger.sql)
— a durable side table the event log does not know about — and the same rules apply: never logged,
never published, deleted on expiry.

`AGENTS.md` §2 forbids an unbounded queue, so:

- **`MAX_QUEUED_PER_PRINTER = 200`.** Counted per printer, not per agent, so one jammed kitchen
  printer cannot consume the receipt printer's budget on the same terminal. The table's total bound
  follows from it and is finite by construction: 200 rows times the printers the published `devices`
  node lists, a list only the console can grow.
- **`JOB_TTL = 600` seconds.** A job older than ten minutes is expired and deleted, never delivered.
- **`CLAIM_LEASE = 30` seconds.** A claimed job that is not acknowledged within it returns to the
  queue. An agent that dies holding a job does not hold it forever.

**A ticket printed an hour late is worse than a ticket that visibly failed.** The late one is cooked
against a bill that settled, walks out to a table that left, and costs the food twice. The failed one
is a cashier reading a refusal while the guest is still standing there. That is the entire argument
for the TTL, and it is why the TTL is ten minutes rather than a value chosen to make the numbers look
generous.

**The three enqueue outcomes are ordered, and the order is part of the decision.** An enqueue checks
the agent first, the cap second, and accepts third:

1. The named agent is unclaimed, or has not been heard from within `AGENT_SILENCE` — refuse
   `PRINT_AGENT_UNAVAILABLE`. Nothing is written. A queue does not start building behind a box that
   is not there.
2. That printer already holds `MAX_QUEUED_PER_PRINTER` unexpired jobs — refuse `PRINT_QUEUE_FULL`.
3. Otherwise the row is written and the outcome is `QUEUED_TO_AGENT`.

Ordering them this way makes the cap's scenario a specific and reachable one, which the reverse order
would not. A full queue is **not** a dead agent — step 1 already refused that case sixty seconds in.
It is a **live agent whose printer is not consuming**: paper out, cover open, a write that errors and
a job that returns to the queue at `CLAIM_LEASE`, while the till keeps firing. The agent claims,
fails, and claims again, so it is heard from every few seconds and never looks silent; the queue
grows at the rate the kitchen is firing until it hits 200. That is the state the cap exists for.

**When the cap is reached the enqueue refuses.** It does not drop the oldest job and it does not drop
the newest one silently. Adding a 201st job to a printer that has not consumed 200 is promising paper
that is not coming. The refusal is reported on the settle or fire response, so the operator learns it
at the till rather than from the absence of a ticket.

### The job never rides `/ws`, and neither does the wake

**Delivery is a claim the agent makes against the queue, and the queue is the only path.** `GET
/api/print/jobs` returns at most one job per printer the agent owns, so a jammed device does not stall
its neighbours; `POST /api/print/jobs/{job_id}/ack` reports the outcome. The agent never opens `/ws`
and there is no fan-out frame about printing at all — no new `ServerMessage` variant, and nothing for
`ui/` to learn to ignore.

The latency comes from a wake, and the wake is in-process. [ADR-0062](0062-the-relay-wake.md) settled
this exact shape for the order relay, on the same reasoning: the process that writes the row is the
process the waiter is parked in, so nothing needs to poll to discover a write it performed itself.
`PrintWake` is that seam, and it is `RelayWake` with one signal instead of two, because an
acknowledgement is a direct call and needs no notification:

```rust
/// Wakes an agent parked on `GET /api/print/jobs` the moment a job it may claim exists. The
/// dispatcher writes the row, commits, and then signals; nothing polls to discover that write.
pub trait PrintWake: Send + Sync {
    /// Signals that `agent` has at least one newly queued job.
    fn queued(&self, agent: DeviceId);
    /// Waits for the next `queued` for this agent, or `timeout`, whichever comes first.
    async fn await_queued(&self, agent: DeviceId, timeout: Duration) -> Woke;
}
```

It lives in `pos-edge` beside the fan-out rather than becoming a twenty-first port, for ADR-0062's
reason read one tier down: the writer and the waiter are the same process by construction here — a
store runs one edge, and [ADR-0049](0049-single-active-lease.md) is the mechanism that guarantees it —
so a `tokio::sync::Notify` is not a corner cut, it is exactly right.

ADR-0062's two sentences transfer without amendment. *"A wake is an optimisation, never the
correctness argument"*: the row in SQLite is the only source of truth, the parked request also returns
on a timer, and the re-read interval is derived as `AGENT_PARK / 2` rather than declared, so it cannot
be configured to a value that computes to zero re-reads and silently removes the safety net. And *"a
waiter subscribes before it reads, never after"*: the handler takes its wake subscription, then reads
the queue, then waits, so a job enqueued in between is already accounted for.

The park is bounded on both axes. `AGENT_PARK = 20` seconds is the deadline, after which the request
answers empty and the agent asks again — a held socket that is never answered is indistinguishable
from a dead one, and 4G NAT bindings are reaped. And one agent parks once: a second concurrent `GET
/api/print/jobs` on the same binding is answered immediately rather than parked, so an agent cannot
accumulate connections against the edge.

**ADR-0018 says a device does not poll, and this record is a stated exception to it, not an oversight.**
Its sentence is *"a device opens one socket and receives a typed event stream; it does not poll"*, and
[`ws.rs`](../../crates/pos-edge/src/http/ws.rs) repeats it. Two things make the agent a different
animal, and ADR-0018 names the first itself: it *"governs the edge's internal, UI-facing transport
only"*, and an agent is not a view — it holds no screen, renders nothing, and has no state to
resynchronise. The second is the failure mode. Every `/ws` subscriber can recover from a dropped frame
by refetching, which is what makes `Resync` a complete answer for a floor screen; an agent that missed
a frame would have missed a document that exists nowhere else. So the agent takes the shape [ADR-0061](0061-order-relay.md)
and [ADR-0062](0062-the-relay-wake.md) built for exactly this — a durable queue, a parked request, an in-process wake, a bounded
fallback re-read — and `/ws` stays what ADR-0018 made it.

It is worth being precise about the resync case, because it is the one that looks survivable. A device
that lags is told to reload a fresh snapshot. For a floor screen that is complete: the snapshot *is*
the state. For a print job the snapshot is the queue — so a resynchronising agent has to go and read
the queue anyway. Once the queue is the thing the agent reads on resync, the queue is the delivery
mechanism, and a frame carrying the job is a second copy of a document that can disagree with the
first.

Both routes sit behind the paired-device gate and no further: an agent is an unattended process, there
is no operator signed in on it, and requiring one would mean a manager's PIN before every kitchen
ticket. That is a weaker gate than the domain routes carry —
[`http/mod.rs`](../../crates/pos-edge/src/http/mod.rs) says the guarded router *"needs a paired device
**and** an employee signed in on it, so every read and command runs under a real Actor"* — and it is a
deliberate choice. It is not, however, a departure from a uniform norm, because there is no uniform
norm: of the edge's twenty-seven `/api/*` routes, nineteen carry both gates, **five carry the
paired-device gate only** — the three session routes, because signing in is how a device passes the
second gate, plus `/api/pair/devices` and `/api/pair/revoke` — and two carry neither, `POST /api/pair`
and `GET /api/activation`. So a paired-device-only route is an established shape here, and these two
join the five rather than inventing a sixth kind of gate.
The two human acts in this record, the claim and the revoke, carry both as well. All four routes
remain paired-device routes for [ADR-0111](0111-a-second-origin-may-address-the-edge.md)'s purposes,
which is what that record checked; two of them are gated more strongly than it needs, never less.

### The acknowledgement is the job id, and the agent persists one id per printer

`job_id` is already the idempotency key. `Printers::print_receipt` takes it and the doc comment states
the contract — *"pass the settle's event id and a retried settle reprints nothing"* — and
`printer-escpos` holds an in-process set keyed on it. That set is on the wrong machine now, because
the machine that writes the bytes is the one that can duplicate them.

So the agent persists **one value per printer: the id of the last job it wrote successfully.** Not
the document, not a queue, not a history. When the edge redelivers a job after a lost acknowledgement,
the agent compares the id against that value and, on a match, acknowledges without writing. One value
is sufficient because the queue leases one job per printer at a time — ESC/POS is a byte stream and
two concurrent writers interleave garbage — so at any moment there is exactly one job whose outcome
is in doubt.

An agent that loses that value — a reinstall, a wiped profile — reprints one job. That is accepted,
and it is the correct direction: a duplicate ticket costs a strip of paper, and a missing one costs a
dish. A duplicate *receipt* is the sharper case, and it is bounded by
[ADR-0025](0025-receipt-number-authority.md): the receipt number was allocated once, by the edge, at
settle, so the reprint carries the same number. It is a second copy of one receipt, not a second sale.

### Failover is one hop, at selection, and never after the job exists

`station_printer()` already resolves a station's printer and falls back exactly once, to the station
plan's `backup_station_id`, and the reason is in the function's own doc comment: *"One hop only — a
backup chain that looped would print a ticket somewhere nobody expected, and a ticket printed in the
wrong kitchen is worse than one not printed at all, because nobody goes looking for it."*
`pos_core::floor` already validates that a backup names a different station in the same plan
([ADR-0072](0072-floor-and-kitchen.md)).

That rule is unchanged, and agents add no second hop. The backup station's printer may be owned by a
different agent, and that is fine: resolve the printer, then resolve its agent. There is no
agent-to-agent fallback, because two failover dimensions multiply — a ticket that travelled
station → backup station → another agent's terminal lands two rooms from where anyone will look for
it, and the operator has no model that predicts where it went.

The one thing agents change is *when* the hop may consider liveness. Selection may now read one fact
it never had: whether the target printer's agent has been silent past `AGENT_SILENCE`. Reading it at
selection is safe — the job does not exist yet, so nothing can be printed twice.

**A queued job is never re-routed.** Once a job is on an agent's queue it prints there or it expires
there. Moving it to the backup station's agent is the two-writer shape in miniature: the original
agent wakes, claims the job it was already sent, and prints a ticket the backup already printed — in
a different room, where the duplicate is invisible. The queue's TTL and the alert below are the answer
to a stalled agent. Re-routing is not.

### Silence is reported twice: to the till now, and to the console before the night ends

Two thresholds, because they are for two different people.

**At the till, immediately.** The settle and fire responses already report `PRINTED`, `NO_PRINTER`,
`PRINTER_UNAVAILABLE` or `UNPRINTABLE_TEXT`, and the till renders the outcome instead of asserting one
([`bills.rs`](../../crates/pos-edge/src/http/bills.rs),
[`lines.rs`](../../crates/pos-edge/src/http/lines.rs)). Three tokens join them:

- `QUEUED_TO_AGENT` — handed to a live agent's queue. **This is not success**, and the till must not
  render it as a tick. `PrintOutcome::printed()` stays false for it.
- `PRINT_AGENT_UNAVAILABLE` — the named agent is unclaimed, or has neither claimed nor acknowledged
  within `AGENT_SILENCE = 60` seconds. The first enqueue check above, so the cashier knows before the
  guest walks away.
- `PRINT_QUEUE_FULL` — the cap, checked second.

The till maps an unfamiliar token to *not printed*, which is what it must already do and is why
adding tokens is additive rather than breaking.

**In the console, before it becomes an incident.** The heartbeat body already carries `outbox_depth`
and the held lease generation as optional fields an older edge may omit
([`heartbeat_client.rs`](../../crates/pos-edge/src/heartbeat_client.rs),
[ADR-0068](0068-fleet-liveness.md)); one more optional field joins them, absent on a store with no
agent: a per-agent standing carrying the terminal's `agent_device_id`, the paired device holding it,
and the age in seconds of its oldest unacknowledged job. Three opaque identifiers and a number, no
PII. That field is what makes the previous section's visibility claim real — the console can show
which box holds each terminal — and it feeds the alert. O2's evaluator
([`alerts/model.rs`](../../crates/pos-cloud/src/alerts/model.rs), [ADR-0073](0073-alerting.md)) gains
one kind, `print_agent_stalled`, firing at **five minutes** — half the TTL, so the alert arrives while
the tickets it is about can still be saved.

It fires `Critical`, not `Warning`. ADR-0110 established that a stale heartbeat means different things
in different edge placements and therefore carries different severities. A stalled print agent does
not: in every edge placement it means paper that was promised is not coming, and the kitchen has not
been told.

### The drawer follows the printer

A cash drawer is wired to a printer and opened by a command on the same channel, which is why
[`architecture.md`](../architecture.md) requires that *"printers that open a cash drawer must be
USB-attached, **or** all POS devices must sit on a separate VLAN, because port 9100 has no
authentication"* — the kick rides the same unauthenticated channel as everything else.

The disjunct matters here, and it collapses. The VLAN half assumes the edge and the printer are on one
network an operator controls; from a hosted edge placement the edge is not on the shop's VLAN at all,
so segmenting the shop protects nothing the edge is inside. Only the USB half survives a hosted edge
placement, which is why **the drawer is now attached to the agent's terminal**, physically and in the
model. The kick travels inside the job, to the agent that owns the printer, and opens a drawer under
that counter. `PrinterConnection::may_open_a_drawer` keeps its USB-only rule unchanged; what "USB"
names is now the agent's bus rather than the edge's, which is a change of machine and not of rule,
because `connection` was always a fact an operator asserted at approval rather than one the code
discovered.

This record does not make a drawer open. ADR-0103 left that flagged — `PublishedDevice` carries no
field saying a drawer is wired to a given printer, so `assumed_capabilities` reports
`kicks_drawer: false` and no drawer opens anywhere. That console field is still not built. This record
settles only which machine the kick lands on when it is.

### The agent is a third artifact, and ADR-0002 has to be told

The agent is a small native binary, `pos_print_agent`, installed on the designated terminal. Not the
browser tab: a browser cannot open `/dev/usb/lp0` or `\\.\COM3`, and the APIs that come closest are
per-device, gesture-gated and unspiked. Of the workspace it links `printer-escpos`, `pos-ports` and
`pos-proto` and nothing else — no `pos-core`, no `pos-edge`, no `store-sqlite`. `pos-ports` is on that
list because it is where the contract lives: `PrintJob`, `PrintDocument`, `PrinterCapabilities` and
`Transport` are all defined there, and a binary that is handed a `PrintJob` cannot avoid naming its
type. It is trait and type definitions with no infrastructure dependency, which is the boundary
`AGENTS.md` §2 draws. Off the workspace it takes the HTTP client stack
[ADR-0054](0054-edge-cloud-http-client.md) already pins for
[`cloud-sync-http`](../../crates/adapters/cloud-sync-http/), so the agent adds no third-party subtree
the tree does not already build. That list — three workspace crates, no domain — is a rule a
`cargo-deny`-style check can hold, and it is the rule that keeps one ESC/POS encoder in the tree
rather than two.

[ADR-0002](0002-one-binary-per-tier.md) says *"Exactly two binaries"*, and this is a third. Confront
it rather than route around it. ADR-0002's reason is that *"every additional process is something to
install, monitor, upgrade and debug remotely"*, and all four of those costs are real here. Three
things bound them:

- **It exists only where it is needed.** A store whose `edge_placement` is `in-store` installs
  nothing. The blast radius is exactly the stores that chose to be hosted.
- **It holds nothing that matters.** One id per printer. Reinstalling it from scratch costs at most
  one duplicated ticket, which is the bounded failure the acknowledgement section already accepts.
- **It decides nothing.** No domain code, no configuration of its own, no state a person has to
  reason about. `deploy/edge/` already ships a **generated** PowerShell installer —
  [`install-pos-edge.ps1`](../../deploy/edge/install-pos-edge.ps1), emitted by
  `dashboard/src/installers.mjs`, regenerated and diff-checked by the `dashboard` CI job — and a
  systemd unit, for a machine somebody carries into a shop. The agent is the same class of thing,
  rides the same rail, and gets its installer from the same generator.

ADR-0110 recorded that `hosted-by-platform` opens a question ADR-0002 does not answer, and named
ADR-0113 as the record that closes it. This is the first of the two, and it does not close alone:
[ADR-0113](0113-the-host-agent.md) amends ADR-0002 to read **three tiers — store, cloud, host — and
one binary per tier, plus named device-level artifacts that decide nothing**, with `pos_print_agent`
and `pos_host` as the two named exceptions. Read against that amendment, this record's claim is the
narrow one: the store tier gains a named exception, not a tier of its own, and a print agent is a
device driver that happens to be a process.

## What this deliberately does not do

- **It does not put a single byte of rendering on the device.** Not the raster, not the tax section,
  not the invoice block, not the column width. The agent receives blocks and writes them. If a future
  device could plausibly render better — a terminal with a better font set, say — that is still
  declined, because the value of one renderer is that two stores print the same document and one
  auditor reads one implementation.
- **It does not attest the agent's hardware.** A claim proves a manager's permission at a box, not
  which box. The framework has no device attestation anywhere — no TPM, no per-device certificate,
  nothing a pairing carries beyond a token — and inventing one for printing alone would be the wrong
  place to start: it belongs to pairing ([ADR-0030](0030-pairing-and-offline-auth.md)) and
  [ADR-0084](0084-device-authentication.md), it would need an enrolment story for every device class,
  and every part of the system that trusts a paired device would want it before printing does. What
  would settle it is a device-identity record covering pairing as a whole; until there is one, this
  record relies on permission, exclusivity and the console's visibility, and says so out loud rather
  than implying a proof it does not have.
- **It does not promise an Android agent.** Android as an ESC/POS print agent over Bluetooth or
  USB-host **has not been spiked**, and neither has Tauri v2 against the real `ui/dist` on Android.
  Nothing in this record depends on either. The floor is a Windows or Linux terminal opening a device
  path — the case `DeviceTransport` already covers and `\\.\COM3` already names — and **Windows-only
  is the safe floor**, because `deploy/edge/` ships a generated PowerShell installer and a Windows
  service wrapper today. If both spikes fail, every decision above stands unchanged: the queue, the
  claim, the acknowledgement and the alert are indifferent to what the agent runs on.
- **It does not support more than one agent per store.** The model permits several — the binding is
  per terminal and printers name their agent individually — but no store has run more than one, and a
  second agent's failure modes (two terminals, one drawer, an operator who moved a printer's cable)
  are not designed for. Permitted, not supported, and the first fork that needs two should expect to
  write the record that supports them.
- **It does not change the in-store path.** No `agent_device_id`, no queue row, no claim, no ack, no
  new latency. `Printers::dispatch` opens the transport and writes, exactly as
  [ADR-0103](0103-directly-attached-printers.md) has it. This is not a migration with a compatibility
  mode; it is a field that is absent for every store that exists.
- **It does not give the agent a second delivery path.** No inbound connection to a till, no mDNS
  lookup for an agent, no direct cloud-to-agent route, and no job or wake on `/ws`.
  [ADR-0062](0062-the-relay-wake.md) declined a cloud-to-store channel on merit and the same reasoning
  applies one layer down: a second path that can disagree with the queue about whether a job was
  handed over is worse than the latency it saves.
- **It does not give the agent wire its own version handshake.** `PROTOCOL_VERSION` negotiates the
  edge–cloud language ([ADR-0024](0024-protocol-version-negotiation.md)) and nothing here crosses that
  boundary. The agent wire is governed by the additive rule alone, so an older agent ignores fields it
  does not know. A second negotiation is a second thing to get wrong, and the day it is genuinely
  needed is the day a field stops being additive — which is already forbidden.
- **It does not re-route a queued job, and it does not chain failover.** Argued above. Both are
  refusals, not omissions: each would let one ticket print twice in two rooms.
- **It does not open a cash drawer.** ADR-0103's missing console field is still missing. This record
  says which machine the kick lands on, not that one is sent.
- **It does not fix the shutdown drain.** [`server.rs`](../../crates/pos-edge/src/server.rs) drains
  in-flight HTTP and nothing else, so a queued print job dies with the process the same way an unsent
  outbox batch does. ADR-0110 made that gap load-bearing for an edge-placement teardown; the print
  queue joins it, and [ADR-0113](0113-the-host-agent.md) is where it closes. The TTL bounds the damage
  — a job lost to a restart was going to expire in under ten minutes anyway — but the gap is real and
  it is named here rather than papered over.
- **It does not let an agent print without a lease-holding edge.** The agent has no domain code and no
  database, so a hosted store whose WAN is down has a terminal that can write bytes and nothing to
  write. That is [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)'s accepted trade — a hosted
  edge placement cannot sell offline — and the agent does not soften it. Softening it would mean
  rendering on the device, which is the first bullet.

## Consequences

- `PublishedDevice` gains `agent_device_id: Option<DeviceId>` and `DeviceKind` gains `TERMINAL`. Both
  additive; `Open<DeviceKind>` already retains a token an older edge does not know, and an absent
  field already means what it has always meant.
- The cloud learns the same word: `pos_cloud::devices::DeviceKind` gains `Terminal` / `"terminal"`,
  the `device_proposals.kind` column carries it without a schema change (`text NOT NULL`, no check
  constraint), and `compile_devices` publishes such a row with an empty address and
  `DEVICE_CONNECTION_UNSPECIFIED`. The publish handler's skip-a-connectionless-row rule keeps its
  scope: it exists to protect a USB printer's drawer, and a terminal has neither.
- A create route on the onboarding router writes an approved `TERMINAL` row directly, behind
  `ConsolePermission::ManageDevices` with an audit entry ([ADR-0069](0069-audit-trail.md)) — the
  console *is* the human gate for a device no store can discover.
- **The two console writes are different writes, and they carry different gates.** The agent picker
  edits an approved device row behind `ConsolePermission::ManageDevices`, with `If-Match`
  ([ADR-0094](0094-console-optimistic-concurrency.md)) and an audit entry, the shape
  `admin_update_device` already has for a registry device. `POST /admin/devices/publish` compiles and
  versions the node behind `ConsolePermission::PublishConfig`, reads no `If-Match` today, and gains
  exactly one thing here: it refuses a node whose `agent_device_id` does not resolve to a `TERMINAL`
  in the same node.
- `crates/adapters/store-sqlite/migrations/0009_print_jobs.sql` is the queue: one row per job, keyed on
  `job_id`, holding the target printer, the owning agent, the rendered document, an enqueue time and a
  claim expiry. Additive-only, per [ADR-0017](0017-migrations.md), and never logged.
- The edge serves four new routes: `POST /api/print/agent` and `POST /api/print/agent/revoke` behind
  the paired-device gate **and** an employee holding `Permission::ManageDevices`; `GET
  /api/print/jobs` and `POST /api/print/jobs/{job_id}/ack` behind the paired-device gate alone. Four
  routes is what [ADR-0111](0111-a-second-origin-may-address-the-edge.md) counted, and all four are
  covered by its rule.
- `PrintWake` is a new in-process seam in `pos-edge`, not a port and not a wire: a `tokio::sync::Notify`
  per agent, signalled after the enqueue commits. Nothing in `/ws`, `fanout.rs` or `ui/` changes, and
  no `ServerMessage` variant is added.
- `printing.rs` gains one branch and loses none. `dispatch` reads `agent_device_id`: absent, it opens
  the transport as it does today; present, it enqueues. `prepare`, `receipt_document`,
  `ticket_document`, `assumed_capabilities` and `station_printer` are untouched.
- `PrintOutcome` gains `QueuedToAgent`, `AgentUnavailable` and `QueueFull`, wiring to
  `QUEUED_TO_AGENT`, `PRINT_AGENT_UNAVAILABLE` and `PRINT_QUEUE_FULL`. `printed()` is false for all
  three. The till renders each distinctly, and renders an unknown token as *not printed*.
- The heartbeat body gains one optional field: a per-agent standing (terminal id, the paired device
  holding it, the age of its oldest unacknowledged job). O2 gains `AlertKind::PrintAgentStalled`,
  `Critical`, store-scoped, firing at five minutes.
- **Five constants live in one edge module, and a sixth in the cloud's evaluator.**
  `MAX_QUEUED_PER_PRINTER = 200`, `JOB_TTL = 600` seconds, `CLAIM_LEASE = 30` seconds,
  `AGENT_SILENCE = 60` seconds and `AGENT_PARK = 20` seconds are reasoned from service — a peak hour
  of firing at one station, the life of a useful ticket, a plausible write plus round trip, a NAT
  binding's patience — and none has been observed in a real kitchen. The alert's five minutes is half
  the TTL by construction, and the fallback re-read is `AGENT_PARK / 2` by derivation, so neither is a
  number anyone chose separately. They are constants rather than published configuration, so changing
  one is a release and not a schema change, and that is where they stay until a store has produced
  numbers. `AGENT_SILENCE` is the one to watch: it gates every enqueue, and a value too low refuses
  printing on a healthy store while a value too high queues jobs behind a dead one.
- A new workspace member, `pos_print_agent`, and a third thing to install — bounded to hosted stores,
  permitted to depend on `printer-escpos`, `pos-ports` and `pos-proto` and no other workspace crate,
  and on ADR-0054's existing HTTP client stack off it. ADR-0002's "exactly two binaries" now has a
  stated exception and the reason for it, and [ADR-0113](0113-the-host-agent.md) writes the amendment.
- **The print path is tested twice**, which is ADR-0110's "every feature is now tested twice" arriving
  where it was predicted to arrive. The direct path keeps its fake `TransportFactory`; the agent path
  needs a fake agent that claims, acknowledges, loses an acknowledgement, sleeps past `CLAIM_LEASE`
  and comes back with a wiped id — plus a test that a lost wake still resolves the park, which is
  ADR-0062's obligation inherited with its shape. All of it runs in CI without hardware, exactly as
  ADR-0100's dispatch tests do. What a real Epson does with the bytes stays in
  `docs/gate-register.md` §6.
- [`docs/glossary.md`](../glossary.md) gains **Print agent**: *the device that owns a printer's
  transport and writes the bytes the edge rendered*. Five records and a console screen need one
  meaning for the word.
- [`docs/adr/README.md`](README.md) gains this record's row, as the other four records of the
  programme gain theirs. A record the other four link to should be findable from the index.
- Nothing published is removed or renamed. One optional field, one proto enum variant, one cloud enum
  token, one migration, one heartbeat field, one alert kind, three outcome tokens, four edge routes and
  one console create route — all additions. No `PROTOCOL_VERSION` moves.
