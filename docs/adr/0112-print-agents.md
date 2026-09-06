# ADR-0112 — A paired device may own a printer's transport, and the edge still renders every byte

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-06
· Moves the last hop of [ADR-0103](0103-directly-attached-printers.md) and
[ADR-0100](0100-receipt-and-ticket-printing.md) without moving anything above it
· Required by [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)'s hosted placements
· Rests on [ADR-0030](0030-pairing-and-offline-auth.md) for who may ask and
[ADR-0018](0018-http-websocket-stack.md) for what the fan-out is and is not
· Relates to [ADR-0102](0102-printing-any-script.md), [ADR-0104](0104-multi-component-and-inclusive-tax.md),
[ADR-0106](0106-the-store-is-a-legal-person.md), [ADR-0107](0107-the-buyer-is-a-subject.md),
[ADR-0072](0072-floor-and-kitchen.md), [ADR-0073](0073-alerting.md), [ADR-0068](0068-fleet-liveness.md)

Third of the five records on the **Edge Anywhere** programme. **ADR-0111** (a second origin may
address the edge), **ADR-0113** (the host tier) and **ADR-0114** (region) are reserved numbers with
no files, so they are named in plain text: `xtask links` fails a build on a link that does not
resolve.

## The problem

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
bitmap blocks with a `job_id`. Every one of those steps is placement-blind Rust over rows the edge
already holds.

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
the edge, unchanged, in every placement.**

### The edge renders every byte, and a device that rendered any of them would be wrong

The agent receives a finished `PrintJob`: text blocks the printer's code page covers, bitmap blocks
for everything it does not, a cut, and an id. It decides nothing. Its whole contract is *open this
address, write these bytes, report this id*.

Putting any rendering on the device fails on four independent counts.

**Fonts.** [ADR-0102](0102-printing-any-script.md) rasterises with `rustybuzz` and `ttf-parser` in
[`pos-render`](../../crates/pos-render/), against fonts installed on the machine, at a width taken
from `PrinterCapabilities::dots_per_line`. A device that drew its own glyphs would draw them from
whatever fonts that device happens to have, so a store with three terminals would print three
different tickets from one order — and `printing.rs` already warns when *no installed font covers
these characters*, a diagnosis that is worthless if the answer differs per tablet. The raster width
is worse: `ASSUMED_DOTS_PER_LINE` is 576, and a device guessing a different figure renders a raster
wider than the print head, which shears.

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
document as the output. ADR-0110's constraint on this whole programme settles it: placement *"may
change where a byte is written and may never change what the byte says."* This record moves the
write. It moves nothing else.

### A printer names its agent, and absent means the edge

`PublishedDevice` gains one optional field:

```rust
/// The device whose transport reaches this printer. Absent: the edge opens the address itself.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub agent_device_id: Option<DeviceId>,
```

and `DeviceKind` gains one variant, `Terminal = "TERMINAL"`, for the entry an `agent_device_id`
points at. Both are additive, which is the [`AGENTS.md`](../../AGENTS.md) §2 rule and also the point:
**absent means the edge is the agent**, so an in-store store configures nothing, changes nothing, and
runs exactly [ADR-0103](0103-directly-attached-printers.md). A fleet of five hundred stores takes
this release and prints tomorrow the way it printed today, because for every one of them the field is
not there.

A terminal entry's `address` is empty, because nothing dials it. The edge never opens a connection to
an agent; the agent opens one to the edge. That is `AGENTS.md` §1's outbound rule read one layer
further in, and it buys the same thing at the store scale that it bought at the fleet scale: no
inbound port on a till, no NAT to traverse, no discovery, and nothing to configure when the
till's DHCP lease changes.

The alternative — a separate `print_agents` node, or an `agent_id` on the station plan — puts the
binding somewhere other than on the thing being bound. A printer's agent is a fact about that
printer, in the same category as its connection and its address, and those already live on the device
entry. Splitting them means two nodes that can disagree about a device, and the config tree's
never-blank rule ([ADR-0108](0108-the-lease-generation-is-authority.md)) makes disagreement durable:
one node updates, the other keeps its previous value, and the store prints somewhere nobody chose.

An older edge that does not know the field ignores it and opens the address itself, which is the safe
direction: in-store that is the behaviour it already had, and in a hosted placement it opens a device
path that is not there and reports `PRINTER_UNAVAILABLE` — a named refusal, not silence.

### The console decides which entries may be claimed, and the store decides which box claimed one

**The designated agent is a fixed, mains-powered terminal, and never a waiter's phone.** That is a
rule, not advice, and it is enforced in two places because one is not enough.

The console enforces the *entry*. An `agent_device_id` may only name a `TERMINAL` device in the same
published node, approved through [ADR-0041](0041-device-onboarding.md)'s discover → propose →
admin-approves flow by a named admin holding `ConsolePermission::ManageDevices`. `POST
/admin/devices/publish` refuses a node whose reference does not resolve. A phone that paired with the
store is a paired device; it is not an approved terminal, so it cannot be named, and naming it is not
a thing an operator can do quickly by mistake — it is an approval with an audit entry
([ADR-0069](0069-audit-trail.md)) and an `If-Match` ([ADR-0094](0094-console-optimistic-concurrency.md)).

The store enforces the *machine*. A paired device claims an agent identity once, at the box, over
`POST /api/print/agent`, and the edge records the binding durably — the same shape
[`pairing.rs`](../../crates/pos-edge/src/pairing.rs) already uses, which records a device *before*
returning its token so a crash between the two leaves the operator pairing again rather than holding
a credential the box forgot. The binding is exclusive: a second paired device claiming a held agent
is refused, not silently promoted. `POST /api/print/agent/revoke` releases it, mirroring
`/api/pair/revoke`.

Take-over-by-latest is the tempting simplification and it is wrong for the reason the whole framework
keeps rediscovering. Two devices holding one agent identity both claim from the same queue, so each
ticket prints exactly once — on whichever box grabbed it. If both are at the counter, nothing is
visibly wrong. If one is a phone in an apron, half the kitchen's tickets are in a pocket, and nobody
finds out until service. Refusing is visible; splitting is not.

### The queue is a table on the store's SQLite, capped and time-limited

A tablet sleeps, a terminal is rebooted mid-service, a USB cable is knocked out. A queue in the
agent's memory loses jobs to all three, and a queue in the edge's memory loses them to a restart. So
the queue is a new table in a new additive migration, `0009_print_jobs.sql`, in the store's own SQLite
— the durable, edge-local category `store_lease`, `intake_ledger` and the queue-number counter
already occupy, written through the single writer thread that serialises every other allocation.

It is a **side record, not an event**. `AGENTS.md` §1 forbids PII in an event payload, a rendered
receipt may carry a buyer's name and tax code, and the row holds the rendered document. It is the
same category as [`0004_intake_ledger.sql`](../../crates/adapters/store-sqlite/migrations/0004_intake_ledger.sql)
— a durable side table the event log does not know about — and the same rules apply: never logged,
never published, deleted on expiry.

`AGENTS.md` §2 forbids an unbounded queue, so:

- **`MAX_QUEUED_PER_PRINTER = 200`.** Counted per printer, not per agent, so one jammed kitchen
  printer cannot consume the receipt printer's budget on the same terminal.
- **`JOB_TTL = 600` seconds.** A job older than ten minutes is expired and deleted, never delivered.
- **`CLAIM_LEASE = 30` seconds.** A claimed job that is not acknowledged within it returns to the
  queue. An agent that dies holding a job does not hold it forever.

**A ticket printed an hour late is worse than a ticket that visibly failed.** The late one is cooked
against a bill that settled, walks out to a table that left, and costs the food twice. The failed one
is a cashier reading a refusal while the guest is still standing there. That is the entire argument
for the TTL, and it is why the TTL is ten minutes rather than a value chosen to make the numbers look
generous.

**When the cap is reached the enqueue refuses.** It does not drop the oldest job and it does not drop
the newest one silently. A queue at 200 for one printer means that printer's agent has been down long
enough that everything already queued will expire; adding a 201st job is promising paper that is not
coming. The refusal is reported on the settle or fire response, so the operator learns it at the till
rather than from the absence of a ticket.

### The job never rides `/ws`; only a wake does

Delivery is a claim the agent makes: `GET /api/print/jobs` long-polls and returns at most one job per
printer the agent owns, so a jammed device does not stall its neighbours. `POST
/api/print/jobs/{job_id}/ack` reports the outcome. Both sit behind the paired-device token every
`/api/*` route already requires ([ADR-0030](0030-pairing-and-offline-auth.md)).

The fan-out carries a wake and nothing else: one new `ServerMessage` variant naming an
`agent_device_id`, with no document, no buyer and no station. That is safe on a broadcast — it is one
identifier every device in the store could read off the console anyway — and it is safe against
`Lagged`, because a dropped wake costs latency and not a ticket. The agent also polls on a timer, and
the timer is not optional. [ADR-0062](0062-the-relay-wake.md) settled this exact shape for the order
relay and the sentence transfers without amendment: *"A wake is an optimisation, never the
correctness argument."* It also settled the ordering bug that comes free with it — *"A waiter
subscribes before it reads, never after"* — and the agent follows it: subscribe, then claim, then
wait.

Carrying the job itself on `/ws` fails on the three properties `fanout.rs` and `ws.rs` are built to
have, listed in the problem above. It is worth being precise about the resync case, because it is the
one that looks survivable. A device that lags is told to reload a fresh snapshot. For a floor screen
that is complete: the snapshot *is* the state. For a print job the snapshot is the queue — so a
resynchronising agent has to go and read the queue anyway. Once the queue is the thing the agent
reads on resync, the queue is the delivery mechanism, and the frame carrying the job is a second copy
of a document that can disagree with the first.

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
- `PRINT_AGENT_UNAVAILABLE` — the named agent is unclaimed, or has been silent past
  `AGENT_SILENCE = 60` seconds. Refused at enqueue, so the cashier knows before the guest walks away.
- `PRINT_QUEUE_FULL` — the cap above.

The till maps an unfamiliar token to *not printed*, which is what it must already do and is why
adding tokens is additive rather than breaking.

**In the console, before it becomes an incident.** The heartbeat body already carries `outbox_depth`
and the held lease generation as optional fields an older edge may omit
([`heartbeat_client.rs`](../../crates/pos-edge/src/heartbeat_client.rs),
[ADR-0068](0068-fleet-liveness.md)); the age in seconds of the oldest unacknowledged job joins them,
`Option<u64>`, absent on a store with no agent. O2's evaluator
([`alerts/model.rs`](../../crates/pos-cloud/src/alerts/model.rs), [ADR-0073](0073-alerting.md)) gains
one kind, `print_agent_stalled`, firing at **five minutes** — half the TTL, so the alert arrives while
the tickets it is about can still be saved.

It fires `Critical`, not `Warning`. ADR-0110 established that a stale heartbeat means different things
in different placements and therefore carries different severities. A stalled print agent does not:
in every placement it means paper that was promised is not coming, and the kitchen has not been told.

### The drawer follows the printer

A cash drawer is wired to a printer and opened by a command on the same channel, which is why
[`architecture.md`](../architecture.md) requires that printers which open one be USB-attached — port
9100 has no authentication and the kick rides the same unauthenticated channel as everything else.

So **the drawer is now attached to the agent's terminal**, physically and in the model. The kick
travels inside the job, to the agent that owns the printer, and opens a drawer under that counter.
`PrinterConnection::may_open_a_drawer` keeps its USB-only rule unchanged; what "USB" names is now the
agent's bus rather than the edge's, which is a change of machine and not of rule, because
`connection` was always a fact an operator asserted at approval rather than one the code discovered.

This record does not make a drawer open. ADR-0103 left that flagged — `PublishedDevice` carries no
field saying a drawer is wired to a given printer, so `assumed_capabilities` reports
`kicks_drawer: false` and no drawer opens anywhere. That console field is still not built. This record
settles only which machine the kick lands on when it is.

### The agent is a third artifact, and ADR-0002 has to be told

The agent is a small native binary, `pos_print_agent`, installed on the designated terminal. Not the
browser tab: a browser cannot open `/dev/usb/lp0` or `\\.\COM3`, and the APIs that come closest are
per-device, gesture-gated and unspiked. It links `printer-escpos` and `pos-proto` and nothing else —
no `pos-core`, no `pos-edge`, no `store-sqlite` — so there is one ESC/POS encoder in the tree and the
agent is a build of the transport that already exists rather than a second implementation of it. That
dependency list is a rule a `cargo-deny`-style check can hold.

[ADR-0002](0002-one-binary-per-tier.md) says *"Exactly two binaries"*, and this is a third. Confront
it rather than route around it. ADR-0002's reason is that *"every additional process is something to
install, monitor, upgrade and debug remotely"*, and all three of those costs are real here. Three
things bound them:

- **It exists only where it is needed.** An `in-store` placement installs nothing. The blast radius is
  exactly the stores that chose to be hosted.
- **It holds nothing that matters.** One id per printer. Reinstalling it from scratch costs at most
  one duplicated ticket, which is the bounded failure the acknowledgement section already accepts.
- **It decides nothing.** No domain code, no configuration of its own, no state a person has to
  reason about. `deploy/edge/` already ships a PowerShell installer and a systemd unit for a machine
  somebody carries into a shop; the agent is the same class of thing and rides the same rail.

ADR-0110 recorded that mode 3 opens a question ADR-0002 does not answer, and named ADR-0113 as the
record that closes it. This is the second such question, and it closes here: the tiers are still store
and cloud, and a print agent is a device driver that happens to be a process.

## What this deliberately does not do

- **It does not put a single byte of rendering on the device.** Not the raster, not the tax section,
  not the invoice block, not the column width. The agent receives blocks and writes them. If a future
  device could plausibly render better — a terminal with a better font set, say — that is still
  declined, because the value of one renderer is that two stores print the same document and one
  auditor reads one implementation.
- **It does not promise an Android agent.** Android as an ESC/POS print agent over Bluetooth or
  USB-host **has not been spiked**, and neither has Tauri v2 against the real `ui/dist` on Android.
  Nothing in this record depends on either. The floor is a Windows or Linux terminal opening a device
  path — the case `DeviceTransport` already covers and `\\.\COM3` already names — and **Windows-only
  is the safe floor**, because `deploy/edge/` ships a PowerShell installer and a Windows service
  wrapper today. If both spikes fail, every decision above stands unchanged: the queue, the claim, the
  acknowledgement and the alert are indifferent to what the agent runs on.
- **It does not change the in-store path.** No `agent_device_id`, no queue row, no claim, no ack, no
  new latency. `Printers::dispatch` opens the transport and writes, exactly as
  [ADR-0103](0103-directly-attached-printers.md) has it. This is not a migration with a compatibility
  mode; it is a field that is absent for every store that exists.
- **It does not give the agent a second delivery path.** No inbound connection to a till, no mDNS
  lookup for an agent, no direct cloud-to-agent route. [ADR-0062](0062-the-relay-wake.md) declined a
  cloud-to-store channel on merit and the same reasoning applies one layer down: a second path that
  can disagree with the queue about whether a job was handed over is worse than the latency it saves.
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
  outbox batch does. ADR-0110 made that gap load-bearing for placement teardown; the print queue joins
  it. The TTL bounds the damage — a job lost to a restart was going to expire in under ten minutes
  anyway — but the gap is real and it is named here rather than papered over.
- **It does not let an agent print without a lease-holding edge.** The agent has no domain code and no
  database, so a hosted store whose WAN is down has a terminal that can write bytes and nothing to
  write. That is [ADR-0110](0110-edge-placement-is-a-deployment-axis.md)'s accepted trade — a hosted
  placement cannot sell offline — and the agent does not soften it. Softening it would mean rendering
  on the device, which is the first bullet.

## What this deliberately leaves uncertain

- **The three constants are first numbers, not measured ones.** `MAX_QUEUED_PER_PRINTER = 200`,
  `JOB_TTL = 600` seconds and `CLAIM_LEASE = 30` seconds are reasoned from service — a peak hour of
  firing at one station, the life of a useful ticket, a plausible write plus round trip — and none of
  them has been observed in a real kitchen. They are constants in one module rather than published
  configuration, so changing one is a release and not a schema change, and that is where they stay
  until a store has produced numbers.
- **Whether one agent per store is enough** is not settled. The model permits several — the binding is
  per terminal and printers name their agent individually — but no store has run more than one, and a
  second agent's failure modes (two terminals, one drawer, an operator who moved a printer's cable)
  are not designed for. They are permitted, not supported.

## Consequences

- `PublishedDevice` gains `agent_device_id: Option<DeviceId>` and `DeviceKind` gains `TERMINAL`. Both
  additive; `Open<DeviceKind>` already retains a token an older edge does not know, and an absent
  field already means what it has always meant.
- `crates/adapters/store-sqlite/migrations/0009_print_jobs.sql` is the queue: one row per job, keyed on
  `job_id`, holding the target printer, the owning agent, the rendered document, an enqueue time and a
  claim expiry. Additive-only, per [ADR-0017](0017-migrations.md), and never logged.
- The edge serves four new routes: `POST /api/print/agent`, `POST /api/print/agent/revoke`,
  `GET /api/print/jobs` and `POST /api/print/jobs/{job_id}/ack`, all behind the paired-device token.
- `printing.rs` gains one branch and loses none. `dispatch` reads `agent_device_id`: absent, it opens
  the transport as it does today; present, it enqueues. `prepare`, `receipt_document`,
  `ticket_document`, `assumed_capabilities` and `station_printer` are untouched.
- `PrintOutcome` gains `QueuedToAgent`, `AgentUnavailable` and `QueueFull`, wiring to
  `QUEUED_TO_AGENT`, `PRINT_AGENT_UNAVAILABLE` and `PRINT_QUEUE_FULL`. `printed()` is false for all
  three. The till renders each distinctly, and renders an unknown token as *not printed*.
- The heartbeat body gains the age of the oldest unacknowledged job, `Option<u64>`. O2 gains
  `AlertKind::PrintAgentStalled`, `Critical`, store-scoped, firing at five minutes.
- The console's Devices screen gains an agent picker on a printer, listing only approved `TERMINAL`
  devices in the same store, behind `ConsolePermission::ManageDevices`, with `If-Match`
  ([ADR-0094](0094-console-optimistic-concurrency.md)) and an audit entry
  ([ADR-0069](0069-audit-trail.md)) like every other `/admin` write. `POST /admin/devices/publish`
  refuses a node whose `agent_device_id` does not resolve to a terminal in the same node.
- A new workspace member, `pos_print_agent`, and a third thing to install — bounded to hosted stores,
  and permitted to depend on `printer-escpos` and `pos-proto` and nothing else. ADR-0002's "exactly
  two binaries" now has a stated exception and the reason for it.
- **The print path is tested twice**, which is ADR-0110's "every feature is now tested twice" arriving
  where it was predicted to arrive. The direct path keeps its fake `TransportFactory`; the agent path
  needs a fake agent that claims, acknowledges, loses an acknowledgement, sleeps past `CLAIM_LEASE`
  and comes back with a wiped id — all of which run in CI without hardware, exactly as ADR-0100's
  dispatch tests do. What a real Epson does with the bytes stays in `docs/gate-register.md` §6.
- [`docs/glossary.md`](../glossary.md) gains **Print agent**: *the device that owns a printer's
  transport and writes the bytes the edge rendered*. Five records and a console screen need one
  meaning for the word.
- Nothing published is removed or renamed. One optional field, one enum variant, one migration, one
  heartbeat field, one alert kind, three outcome tokens, four routes — all additions. No
  `PROTOCOL_VERSION` moves.
