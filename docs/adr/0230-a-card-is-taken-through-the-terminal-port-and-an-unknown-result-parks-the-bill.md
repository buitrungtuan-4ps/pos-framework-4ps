# ADR-0230 — A card is taken through the terminal port, and an unknown result parks the bill

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-24 · Wires the
`PaymentTerminal` port (`crates/pos-ports/src/payment.rs`, roadmap-v3 **B9.2**) · Extends
[ADR-0220](0220-a-bill-takes-its-payments-one-at-a-time.md) (a card is one of a bill's payments),
[ADR-0221](0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md) (a tab's bill) and
[ADR-0224](0224-self-order-is-a-guest-page-and-a-kiosk.md) decision 5 (a card at the kiosk) ·
Relates to [ADR-0201](0201-the-till-runs-on-every-device-class.md) decision 5,
[ADR-0083](0083-integration-doctrine.md)

Accepted 2026-11-24 under the owner's instruction to complete phases 7–9 with the recommended
options. The owner has not chosen a card-terminal vendor for Vietnam; that is an **open owner
decision** below, and nothing in this record depends on it.

## Context

The port exists and is well shaped: `Unknown` is a success value, an attempt always carries the
terminal's reference, `authorize` is idempotent by `payment_id`, and no card data crosses it. A fake
(`pos-fakes`) and a contract suite (`pos-contract-tests`) prove it. **The edge calls it zero
times**: a card is "marked paid" — the cashier runs a standalone terminal and the till records
`PAYMENT_METHOD_CARD` as captured. `docs/ui-ux.md` already promises that an unknown card result
parks the bill amber with two exits and lands on a reconciliation list; nothing implements it.

`billing.payment.captured` already carries `outcome`, and the cloud's tender roll-up already counts
only `PAYMENT_OUTCOME_CAPTURED`. The edge's fold counts every payment event, because until now every
one was captured. The event has nowhere to keep the terminal's reference, so an unknown attempt
could not be looked up after a restart.

## Options considered

1. **Keep marking cards paid.** Leaves the exit criterion of phase 8 unmet and the costliest bug in
   the port (a guessed outcome) one cashier's mistake away.
2. **Keep the reference in an edge-local table.** A new `EventStore`/port method and a migration for
   one string, and the cloud's nightly reconciliation could not see it.
3. **One additive field on the payment event, and the edge's fold reading `outcome`** (chosen).

## Decision

1. **The edge calls the port.** An edge is composed with at most one card terminal
   (`Edge::with_card_terminal`); one composed with none answers `CARD_TERMINAL_NONE`, and the till
   keeps offering the card recorded by hand, exactly as today. Which terminal sits beside which
   device becomes a published field on the `devices` node when the first real adapter arrives
   (decision 8); until then only the fake exists, composed in the demo example and in tests.
2. **`POST /api/bills/{id}/card`** takes `amount` (and optionally `tip` and `split_ways`, as a
   payment does). The edge mints the `payment_id` **before** it touches the terminal, then calls
   `authorize`:
   - **Captured** for the amount asked → one `billing.payment.captured` with outcome `CAPTURED`,
     exactly as a payment, settling the bill when it covers what remains.
   - **Declined** → nothing is recorded; the till hears `CARD_DECLINED`.
   - **Never reached** (`PortError::unavailable`) → nothing is recorded; `CARD_TERMINAL_UNAVAILABLE`.
   - **Unknown, an unrecognised outcome, or a captured amount other than the one asked** → one
     `billing.payment.captured` with outcome `UNKNOWN`. The bill **parks**: it is not paid by it,
     and every payment, settle and void on it is refused `CARD_RESULT_UNKNOWN` until it is resolved.
3. **An unknown attempt has three exits**, at
   `POST /api/bills/{id}/card/{payment_id}/resolve` with `action`:
   - `LOOK_UP` asks the terminal (`look_up`): captured → a second event for the same `payment_id`
     with outcome `CAPTURED`; declined, or a reference the acquirer never saw → outcome `DECLINED`;
     still unknown → nothing changes.
   - `CONFIRM` — staff read the terminal's own slip and it says approved → outcome `CAPTURED`.
   - `CANCEL` voids it on the terminal (`void`) → outcome `DECLINED`; a terminal that says it has
     already settled answers `CARD_ALREADY_CAPTURED`, and the person confirms instead.
   Each needs `billing.payment.take`, and each is recorded with the person who did it.
4. **One additive field:** `terminal_reference`, an optional string of at most 128 characters on
   `billing.payment.captured`, written for every attempt made through the port and absent for a card
   recorded by hand. It is the acquirer's opaque reference: not card data and not personal data.
5. **The edge's fold reads `outcome`.** A payment counts towards its bill, its tender and its drawer
   only when its outcome is `CAPTURED` (or absent, as every event written before this record is). An
   `UNKNOWN` event parks its bill; a later event for the same `payment_id` resolves it. The cloud's
   roll-up already counts only `CAPTURED`, so it needs no change and the reconciliation list is the
   events whose last outcome is `UNKNOWN`.
6. **A tab** is paid by card through the same route, since its bill is any bill (ADR-0221). Holding
   a card open on a tab (a pre-authorisation captured at close) is **not** built: the port has no
   hold-then-capture, and adding one is a port change to make with the first real vendor.
7. **A kiosk** (ADR-0224 decision 5) may call the card route, and only that payment route: for a bill
   on an order it opened, for the whole of what remains, with no tip. The fixed `KIOSK` set does not
   gain `billing.payment.take`; the route admits a kiosk record on exactly these terms. An unknown
   result at a kiosk tells the guest to see staff, and staff resolve it at a till.
8. **B10.3, a real terminal for Vietnam, stays open.** The vendor is an **open owner decision**;
   its adapter goes under `crates/adapters/`, passes the contract suite, and brings the per-device
   binding of decision 1.

## Configuration

- **Whether a store takes cards at all** stays its accepted tenders (`PAYMENT_METHOD_CARD`).
- **Which terminal** is composition today and a published device field with the first adapter.
- **Fixed rules,** money integrity: no guessed outcome, no payment over a parked bill, no card data.

## Consequences accepted

- One additive event field (`docs/snapshots/events.txt`), two routes, three refusal reasons and
  translations for them. No port, permission or dependency change.
- **An edge rolled back below this release folds an `UNKNOWN` payment as paid.** The upgrade note
  says to resolve parked cards before a rollback; the canary rings see it first.
- **Nothing here is verified on a real terminal.** It is built and tested against the fake and the
  contract suite; a vendor's adapter is the first proof on hardware.
- **Data:** the reference is not personal data, so no PDPD (Decree 13/2023) transfer question
  arises from it; card data never reaches the framework. Whether an acquirer's reference must be
  kept for a set period is **to be confirmed by legal or the accountant**; it lives as long as the
  event does.
