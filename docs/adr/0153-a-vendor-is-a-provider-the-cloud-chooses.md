# ADR-0153 — A vendor is a provider in a catalogue, and the cloud chooses it

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-29
· Relates to [ADR-0083](0083-integration-doctrine.md), [ADR-0027](0027-country-modules.md),
[ADR-0001](0001-offline-first-store-autonomy.md), [ADR-0112](0112-print-agents.md),
[ADR-0152](0152-a-receipt-number-belongs-to-a-series-and-each-country-sets-its-rules.md)

## The problem

ADR-0083 says every external system plugs in through a port and an adapter, the API, or the event
stream, and ADR-0027 says a vendor family is chosen at run time from configuration. Neither says
*how*: which configuration, who writes it, where the vendor's credentials live, or which process talks
to the vendor. The tree shows the gap. `PaymentTerminal` and `Fiscalization` have ports, contract
suites and object-safe mirrors (`pos_ports::dynamic`), and nothing in either binary calls them. The
ERP and courier adapters exist and read nothing a tenant can set.

The families that most need an answer are the ones with many vendors per country: e-invoicing
(Vietnam alone has several licensed providers), QR and wallet payments, card acquirers, delivery
marketplaces, couriers and ERPs. A chain switches between them for commercial reasons, a franchise may
use a different one per brand, and every new country brings its own. If each switch is a code change
in the edge, the console and a release to every store, "plug-and-play" is a claim, not a property.

## Options considered

| | Option | Add a vendor | Switch vendor | Where credentials live | Offline |
|---|---|---|---|---|---|
| A | Adapter chosen at build time per deployment | Release to every store | Release | Each box's config file | Unchanged |
| B | Every adapter on the edge, chosen by a config node | Release to every store | Config | **Every box** | Unchanged |
| C | **The adapter runs where the vendor is**; config chooses it | Cloud release, or edge release for a device | Config | **Cloud only**, for internet services | Each family keeps its own offline path |

B puts a tenant's e-invoice and gateway credentials on every till in the chain, so a stolen store PC
becomes the key to its tax filings, and it still needs an edge release per vendor. **C is chosen.**

## Decision (proposed)

1. **Where the adapter runs follows where the vendor is.**
   - A device on the store's network (card terminal, printer, scale, cash drawer) is driven by the
     **edge**, through a *driver*, because only the edge can reach it.
   - A service on the internet (e-invoice provider, QR or wallet gateway, marketplace, courier, ERP)
     is driven by the **cloud**, through a *connector*. The store asks the cloud over the outbound
     channel it already uses and never holds the vendor's credentials. Unlike a store, the cloud can
     also receive the vendor's webhook.
   - **Every family keeps an offline path at the store**, so ADR-0001 holds. An e-invoice is issued
     locally from a pre-allocated range and queued for submission (`docs/architecture.md` §6.1). A QR
     transfer falls back to the cashier confirming it by hand, as it does today. A card is not taken
     while its terminal cannot be reached, as today.

2. **A vendor is a catalogue entry, described by its own adapter.** Each adapter crate exports one
   `ProviderDescriptor`:
   - a `provider_id`, stable and shaped `<family>.<vendor>` (`einvoice.sandbox`, `qr.sandbox`). It is
     stored in configuration and never renamed.
   - the family, and the countries the provider serves;
   - a **settings schema**: a list of typed fields (text, number, choice, flag, secret), each with a
     translation key, whether it is required, and its validation;
   - its capabilities within the family, for example an e-invoice provider that can cancel an invoice
     against one that can only issue an adjustment.

   The cloud and the edge each build a registry from the descriptors compiled into them, one line per
   adapter, the way `country_registry!` lists countries. The console reads the cloud's registry
   through the admin API.

3. **A tenant configures a *connection*, not code.** A connection is a provider id, a scope (tenant,
   brand or store, the config tree's own layers), settings validated against the descriptor's schema,
   and the secret fields. The console lists a family's providers, renders the form **from the
   schema**, with no screen per vendor, and tests the connection before saving it. Switching vendor
   means editing a connection. A store picks the change up at its next config pull, and nothing is
   released.

4. **Secrets are write-only, and they never leave the cloud.** Secret fields are encrypted at rest
   with XChaCha20-Poly1305, already a `pos-cloud` dependency (`archive.rs`), under a key held in
   `secrets/cloud.toml`. The API accepts a new value and never returns one. A secret appears in no
   config node, event, log or backup archive in the clear. A driver that genuinely needs a device
   secret, such as a terminal's pairing key, is a later record.

5. **The store sees only what it needs**, in a published `integrations` node. Per family, the node
   says whether the online path is on and which connection serves it. For each edge driver it carries
   the provider id and the non-secret settings, such as a terminal's address. Adding the node changes
   `pos-proto`, so it waits on an owner review.

6. **Every family can be tested end to end without a vendor.** Each has its port, its contract suite
   in `pos-contract-tests`, a fake, and a **sandbox provider** compiled into every build and selectable
   like any other. The sandbox is deterministic and can be switched into the failure cases its
   contract names: a timeout read as `Unknown`, a refusal, a duplicate. CI drives the whole path:
   console, configuration, store, cloud, sandbox. A real vendor adapter passes the same suite, and a
   gated lane runs it against the vendor's own sandbox with credentials from CI secrets. Production
   credentials are never needed.

7. **Adding a vendor takes one crate and one line.** The crate starts from `templates/adapter-template`,
   implements the family's port and a descriptor, and passes the contract suite. The line registers
   it. `pos-core`, `pos-proto`, the edge and the console do not change. Adding a *family* is larger (a
   port, a node section, a contract suite) and gets a record of its own.

## Consequences accepted

- The online half of an internet-service family depends on the cloud being reachable. The family's
  offline path is what keeps selling unaffected. Each connection's health (its last success and last
  failure) is shown against the connection and alerts through the existing `AlertChannel`.
- The cloud becomes the place legal submissions are made, so its submission queue must be durable and
  replayable from the store's events. That is decided with the e-invoice lifecycle, not here.
- A new connector ships in a cloud release. That is the intent: vendors change on the cloud's cadence
  and stores on their own.
- The schema is deliberately small: typed fields, with no conditional logic and no scripting. A vendor
  that needs more gets a new field type, added once for every vendor.

## Not decided here

Each of these gets its own record: the e-invoice lifecycle and its events (issue offline, submit,
correct, cancel, reconcile); the QR gateway port; wiring `PaymentTerminal` into settlement; device
secrets for edge drivers.

## Before this is accepted

- The owner reviews the `pos-proto` change that adds the `integrations` node, which is the first
  backbone step this record needs.
