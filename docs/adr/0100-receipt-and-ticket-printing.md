# ADR-0100 — The store learns its printers from configuration, not from a second sync loop

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Implements production-readiness **C2**
· Extends [ADR-0041](0041-device-onboarding.md), [ADR-0004](0004-cloud-owned-configuration.md)
· Relates to [ADR-0026](0026-port-shapes.md) §5, [ADR-0072](0072-floor-and-kitchen.md)

## The problem

Nothing prints. Every settled bill sets `BillView::print_receipt` from the domain's
`Effect::PrintReceipt`, the till renders "Printing receipt…", and the `printer-escpos` adapter —
which encodes ESC/POS, renders a bitmap line when a character falls outside the printer's code page,
and passes its contract suite — is a workspace member **no binary depends on**. `PrintJob` is
constructed nowhere outside the port and its fakes. A store cannot hand a customer a receipt, and a
kitchen cannot be handed a ticket.

Wiring the adapter is mechanical. The decision that is not: **where does a printer's address come
from?** Two paths already exist and neither has been chosen.

## The decision

**The cloud publishes approved devices as a `devices` node in the store's configuration tree.** The
edge reads it in `session_from_config` alongside `menu`, `permissions`, `floor` and `stations`, and
nothing new is fetched.

The rejected alternative is `GET /sync/stores/{id}/devices`, which is already built, already scoped
(`manage_devices`), and already returns each approved proposal with its `address`. It is a good route
and it keeps its job — it is what the console and an operator's `curl` read. It is the wrong source
for a *trading* store, for three reasons:

1. **A printer address must survive a WAN outage, and a config node does.** The config tree is
   persisted locally and restored at boot (**C1**), so a box that reboots with its broadband down
   comes back knowing where its printers are. A second sync loop would come back knowing nothing and
   would print nothing until the cloud answered — the same failure C1 just closed for the menu, and
   worse here, because a receipt is a legal artefact in Vietnam, not a convenience.
2. **One delivery mechanism, one set of failure modes.** The edge already runs a config-pull loop
   with a backoff, a held-version handshake and a hot swap into the live session. A second loop is a
   second thing to get wrong and a second thing an operator has to reason about when a store looks
   half-connected — the exact shape of the `relay_orders`-missing bug (roadmap-v3 E6).
3. **The store's key would need a third scope.** `manage_devices` exists for the *propose* direction
   — a box telling the cloud what it found on the LAN. Granting every store key a read scope it only
   needs because of a delivery choice widens the credential for no capability gain, and S1 has just
   narrowed that credential deliberately.

The `devices` node carries only what the edge acts on: for each approved device, its id, kind
(`printer` / `kds`), address, connection (`usb` / `network` / `serial`), and the `station_id` it
serves. Discovery, proposal and approval stay exactly where ADR-0041 put them; this is the last hop
only.

## What follows from it

- **A drawer still only opens over USB.** `PrinterConnection::may_open_a_drawer` is false for
  `Network` and `Serial`, and publishing an address changes nothing about that: port 9100 has no
  authentication, and the drawer-kick rides the same unauthenticated channel as everything else
  (`docs/architecture.md` §5). A node that names a network printer with a drawer is accepted and the
  drawer command is not sent.
- **Failover is the plan's, not the adapter's.** `KitchenStation::backup_station_id` already declares
  where a ticket goes when a station's printer is down, and `pos_core::floor` already enforces that a
  backup names a different station in the same plan. The dispatcher consults it; the adapter reports
  reachability and nothing more.
- **A receipt is never logged.** `PrintDocument` may carry a buyer's name and tax code on a corporate
  invoice (`docs/roadmap.md` P10), so it is deliberately outside the no-personal-data marker.
  `tracing` records a job's identifier and outcome, never its content — the rule `pos-ports`'s printer
  module already states, restated here because this is the ADR that gives it a caller.
- **A store with no `devices` node prints nothing and says so.** Absent is not an error: a LAN-only
  box that has never synced, or a shop that genuinely has no printer, is an ordinary state. What must
  not happen is the current silence — the till claiming "Printing receipt…" while nothing is wired.
  The bill view reports whether a printer was actually reached.

## What approval has to start capturing

Scoping the publish turned up a constraint worth stating, because it decides the shape of the slice
that follows and would otherwise be rediscovered as a surprise: **a device proposal carries neither a
connection nor a station.** `PersistedDeviceProposal` is `{id, tenant, store, kind, name, address}` —
the facts a box can *discover* on its LAN. It cannot discover that the printer at the counter is on
USB with a drawer under it, or that the one at `192.168.1.50` is the oven's.

Compiling the node from what exists today would therefore publish every device as `Network` with no
station. That is safe — a network device never opens a drawer — and it is also wrong twice over: a
store with a USB receipt printer and a cash drawer could not use it, and no kitchen ticket could
route anywhere.

So **approval is where those two facts are added**, which is what ADR-0041 already made approval for:
the moment a human looks at a discovered device and says what it is. The proposal record gains
`connection` and an optional `station_id`, the approve route accepts them, and the console's Devices
screen asks for them at the point of approval. Only then does the compile have something honest to
publish.

Sequenced that way: (2a) approval captures connection and station; (2b) the cloud compiles and
publishes the `devices` node; (3) the edge applies it; (4) the dispatcher prints. A receipt printer
needs 2a–4; a kitchen ticket needs the station from 2a as well.

## Delivered

All five slices are in the tree. (1) the `devices` node in `pos-proto`; (2a) approval captures a
device's connection and the station it serves; (2b) `POST /admin/devices/publish` compiles the
approved proposals onto the node; (3) `session_from_config` applies it, and C1 persists it; (4) the
dispatcher — `crates/pos-edge/src/printing.rs` — selects the printer, builds the document, and sends
it over `printer-escpos`'s new raw-TCP transport, after the commit and never before. The settle and
fire responses report `PRINTED`, `NO_PRINTER`, `PRINTER_UNAVAILABLE` or `UNPRINTABLE_TEXT`, and the
till renders the outcome instead of asserting one.

Two things this ADR assumed were only a hardware gate turned out to be **two** gates, and both are
now written down as such (`docs/gate-register.md` §6, P8 and P9):

- **USB and serial have no transport.** The `Transport` seam is the right shape and the TCP case
  needed no hardware to prove; the other two need a device on a desk. They are refused with a named
  reason rather than dialled at port 9100, because "the network printer did not answer" is the wrong
  thing to hand an operator holding a USB cable. A cash drawer therefore stays unreachable in this
  build — which changes nothing about the rule, only about when it first bites.
- **A Vietnamese item name cannot go on paper yet.** §5 of ADR-0026 puts the text-or-bitmap decision
  in the framework, and the framework has no CP1258 table and no rasteriser, so the dispatcher refuses
  the line. This was foreseeable from `CodePage::covers` — which answers `line.is_ascii()` for every
  page — and was not foreseen here. Receipts are unaffected; kitchen *tickets* in Vietnamese wait on
  P9. The KDS is unaffected throughout: it is a screen, and it has always had the order.

## Consequences

- No new port: `pos-ports`'s `Printer` is the seam, and `printer-escpos` implements it over its own
  `Transport`. A TCP transport (port 9100) is the network case; USB is the hardware-gated one.
- No new scope, no new loop, no new credential.
- **Hardware gate.** Everything above is verifiable in CI against a fake transport — the encoding,
  the routing, the failover choice, the drawer refusal. That a real Epson or Xprinter accepts the
  bytes is not, and stays in `docs/gate-register.md` §6 with the rest of the hardware soak.
