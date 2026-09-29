# ADR-0155 — A QR payment is confirmed by its gateway through the cloud, and by hand when it cannot be

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-29
· Relates to [ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md),
[ADR-0001](0001-offline-first-store-autonomy.md), [ADR-0028](0028-settlement-and-payment-invariant.md)

## The problem

A QR transfer is confirmed today by a cashier looking at a phone. That is slow at a queue. It is
also the weakest control on the till, because a screenshot of an old transfer looks like a new one.
A gateway that issues the QR can say for certain that the money arrived, but it says so with a
webhook to the merchant's server, and a store takes no inbound connections (architecture §1.3).

## Options considered

| | Option | Store takes inbound traffic | Credentials on the store | Offline |
|---|---|---|---|---|
| A | Keep confirmation by hand | No | None | Unchanged |
| B | The store polls the gateway itself | No | **Yes** | Needs the internet |
| C | **The cloud holds the connection, receives the webhook and relays the confirmation** | No | None | Falls back to A |

B puts a tenant's gateway key on every till. **C is chosen**, with A as its offline path.

## Decision (proposed)

1. **A `QrGateway` port** in `pos-ports`, with its contract suite and fake:
   - `create_intent(payment_id, amount)` returns the QR payload to show and the gateway's reference.
   - `look_up(reference)` says whether the money arrived.
   - `verify_webhook(headers, body)` authenticates a notification and reads it.

   Each vendor is a `qr.<vendor>` connector in the cloud. `qr.sandbox` ships in every build, can be
   told to confirm, stay silent or send a duplicate, and draws a well-formed EMVCo payload.
2. **The till asks the cloud for the QR.** When the store's `integrations` node names a QR
   connection, Pay with QR asks the cloud over the outbound channel the store already uses. The cloud
   creates the intent for the exact amount due, keyed by a `payment_id` the edge minted, and the till
   draws the payload.
3. **The gateway's webhook lands on the cloud.** The cloud verifies it with the connection's sealed
   secret, matches it to the intent, and relays the confirmation down the store's existing relay
   stream. The till settles without a tap. A notification for a different amount, or a replayed one,
   settles nothing and raises an alert.
4. **Offline, or a gateway gone quiet, falls back to hand.** After a bounded wait, or with no
   connection at all, the cashier confirms by hand as today. A gateway confirmation that arrives
   after a hand confirmation is recorded, never applied twice, and a hand confirmation that the
   gateway never matches goes onto the day's reconciliation list.

## Consequences accepted

- The QR path gains a dependency on the cloud being reachable, and only for automatic confirmation.
  Selling by QR never stops.
- `billing.payment.captured` gains an optional `confirmed_by` (`GATEWAY` or `STAFF`) and the gateway
  reference. These are additive `pos-proto` changes, and the port is new, so both need the owner's
  review.
- The webhook route is the cloud's first unauthenticated inbound route for a vendor. It is
  rate-limited, and it trusts only what `verify_webhook` authenticates.

## Not decided here

Refunds to a wallet; dynamic QR on a customer-facing display; which gateways to build first.
