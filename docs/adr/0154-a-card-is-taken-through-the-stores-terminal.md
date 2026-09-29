# ADR-0154 — A card is taken through the store's terminal, and an unknown answer parks the bill

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-29
· Relates to [ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md),
[ADR-0028](0028-settlement-and-payment-invariant.md), [ADR-0001](0001-offline-first-store-autonomy.md)

## The problem

`PaymentTerminal` has a port, a contract suite and an object-safe mirror, and nothing calls it. A
card is taken today on a standalone terminal: the cashier keys the amount into it, then records
`PAYMENT_METHOD_CARD` on the till. The two amounts are typed twice and agree only if nobody slips.
Nothing holds the terminal's reference either, so a disputed charge cannot be traced back to its
bill. The port's own rule — an unknown answer is a result, never a decline — has no caller to obey
it.

## Options considered

| | Option | Offline | Amount typed | Unknown answer |
|---|---|---|---|---|
| A | Keep the standalone terminal | Unchanged | Twice | Not recorded |
| B | **The edge drives the terminal**, through a driver the `integrations` node names | Unchanged: no card while the terminal is unreachable | Once | Parks the bill |
| C | The acquirer's cloud API drives a "cloud terminal" | Card stops when the internet does | Once | Parks the bill |

C puts the internet on the card path, against ADR-0001. **B is chosen**, and A stays what a store
with no card-terminal connection does.

## Decision (proposed)

1. **A driver per vendor, on the edge.** A `card.<vendor>` crate implements `PaymentTerminal` and
   exports a `ProviderDescriptor` (ADR-0153). The edge builds a registry of them. A store's
   `integrations` node names the connection and its non-secret settings, such as the address and the
   terminal number, and the edge opens the driver from them. `card.sandbox` ships in every build and
   can be switched into each outcome the contract names.
2. **Pay with card calls the terminal.** The edge mints the `payment_id` before it touches the
   terminal (the port's idempotency rule) and asks the terminal for the amount due.
   - *Approved* settles the bill as a card payment carrying the terminal's reference.
   - *Declined* records nothing. The cashier tries again or takes another method.
   - *Unknown* **parks the bill.** The payment is recorded with outcome `UNKNOWN` and its
     reference, applied to nothing, and the bill stays open in the amber state `docs/ui-ux.md` §4
     describes.
3. **A parked payment is resolved by asking, never by assuming.** The till offers "check again",
   which calls `look_up`, and "cancel", which calls `void` and then lets the cashier take another
   method. A manager can also confirm by hand against the terminal's own slip. Every resolution is
   an event, and the parked payment appears in the day's reconciliation list until one is recorded.
4. **No card data, ever.** The terminal holds the card. The framework holds a reference and an
   outcome, as the port already requires.

## Consequences accepted

- `billing.payment.captured` gains an optional `terminal_reference`, and a new
  `billing.payment.resolved` records how a parked payment ended. Both are additive `pos-proto`
  changes and need the owner's review.
- A store's first card-terminal connection changes how its cashiers work. The console says so when
  one is enabled.
- A driver that cannot tell a timeout from a decline must answer `Unknown`. The contract suite
  already fails an adapter that answers `Declined`.

## Not decided here

Tips entered on the terminal; pre-authorisation and incremental auth for tabs; the terminal's own
pairing secret (a device secret, per ADR-0153).
