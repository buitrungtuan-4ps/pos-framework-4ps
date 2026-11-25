# ADR-0240 — The cloud allocates each issuing point's invoice symbol from a ledger

**Status** Accepted 2026-11-25 under the owner's instruction to complete phase 10 with the
recommended options · **Owner** @maintainers-architecture · **Date** 2026-11-25 · Phase 10
(roadmap-v4 row 10) · Builds
[ADR-0225](0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md) decisions 2
and 7 · Relates to [ADR-0108](0108-the-lease-generation-is-authority.md),
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0183](0183-a-country-pack-seeds-a-new-stores-settings.md),
[ADR-0229](0229-the-e-invoice-issue-moment-is-a-schedule.md)

## Context

A Vietnamese store that must issue an e-invoice refuses to settle until the cloud publishes its
symbol (`409 E_INVOICE_SYMBOL_MISSING`). Since #1360 HQ writes it by hand with
`PUT /admin/config/einvoice-symbol`; there is no console screen and nothing allocates one. ADR-0225
decision 2 asks that **each issuing point** (a store's lease generation, ADR-0108) number under a
symbol of its own, so a replacement box never repeats a number, even one issued offline and never
synced. Typed by hand, nothing stops two stores of one seller being given the same symbol, which
would make two gapless sequences collide. The pilot readiness record (item 34a) names this the pilot
blocker. ADR-0225 decision 7 also gives the tenant a **correction symbol** that only the cloud
issues from, and nothing yet says how it is chosen.

## Options considered

1. **Hand entry only, with a console screen.** Cheap, but a symbol per box replacement is a step a
   technician forgets at night, and nothing checks two stores against each other.
2. **Derive the symbol from what is published now** (scan every store's `einvoice.symbol`). A
   replaced box's old symbol is overwritten, so a scan forgets it and can hand it out again: the
   collision the rule exists to prevent.
3. **A ledger of every symbol the tenant ever used, allocated from by the cloud, with hand entry
   still possible.** One table, unique per tenant and symbol.

## Decision

**Option 3.**

1. **The ledger.** The cloud records every invoice symbol a tenant's stores were given, by hand or
   allocated, in `einvoice_symbols` (tenant, symbol, store, lease generation, how it was given, when),
   unique on tenant and symbol. It is never deleted from: a symbol once used is used for good. The
   correction symbol is a row with no store.
2. **Hand entry stays.** `PUT /admin/config/einvoice-symbol` now also writes the ledger, and refuses
   (`409 E_INVOICE_SYMBOL_IN_USE`) a symbol the ledger gives to **another** store of the tenant. The
   same store may be given its own symbol again.
3. **Allocation.** `POST /admin/config/einvoice-symbol/allocate` gives a store the next free symbol
   for the current year from its country's rule, writes the ledger and publishes it exactly as the
   hand route does, audited as `config.einvoice_symbol.allocate`. Vietnam's rule builds the symbol
   from a form digit, a code letter, the year's two digits, a kind letter and the two seller
   characters of the next free index (`AA` to `ZZ`, 676 a year per tenant). **The form digit `1`,
   the code letter `C` and the kind letter `M` (an invoice generated from a cash register) are the
   recommended reading, to be confirmed by legal or the accountant**, as is whether 676 issuing
   points a year suffice. A country with no rule answers `422 E_INVOICE_SYMBOL_NO_RULE`, and its
   symbols are typed by hand.
4. **Allocation is a published setting** (ADR-0160): `einvoice.symbol_allocation`, at the Store,
   Store-group, Brand or Tenant layer. `EINVOICE_SYMBOL_ALLOCATION_MANUAL`, the register's default,
   allocates nothing by itself. `EINVOICE_SYMBOL_ALLOCATION_AUTOMATIC`, which the Vietnamese pack
   seeds (ADR-0183), makes the cloud allocate when:
   - **a box is activated** for a store that has no symbol (the activation code is redeemed); and
   - **the store's lease is bumped** for a replacement box: the new symbol is published in the same
     write as the new lease, so the new box never sees the old symbol.
   An allocation that fails (the ledger is unreachable) never fails the activation or the bump: it is
   logged, and the store refuses to settle until a symbol is published, which is the safe failure.
5. **The console** gets an *E-invoice symbol* screen per store: the symbol now published, the
   ledger's history for the store, **Allocate the next symbol**, and hand entry checked against the
   country's shape before it is sent.
6. **The correction symbol** (ADR-0225 decision 7) is allocated from the same ledger, with no store,
   the first time a correction needs one.

## Configuration

- **Setting:** `einvoice.symbol_allocation` (decision 4). Default `MANUAL`; Vietnam seeds `AUTOMATIC`.
- **Fixed (law and money):** a symbol is never given to two stores of one tenant, and never reused;
  the symbol's year is the year it is allocated in.

## Consequences accepted

- A new table, by an additive migration. It holds no personal data.
- A new `409` on the hand route, which before accepted any well-formed symbol: a store already given
  another store's symbol by hand is not changed, only refused next time.
- A new year needs a new symbol (ADR-0225 decision 2). This record does not roll symbols over on
  1 January by itself; the store refuses to settle with `WrongYear` until one is allocated, and the
  console's screen says so. A scheduled roll-over is left for later.
- The claim path (ADR-0148) does not allocate on its own; a store first activated by a claim is
  given its symbol from the console.
- No legal fact is invented: every letter of the rule is marked to confirm.
