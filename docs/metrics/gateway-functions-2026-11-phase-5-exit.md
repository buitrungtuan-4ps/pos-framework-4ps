# Gateway and functions at the end of phase 5, November 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-11-13

The exit record of roadmap-v4 phase 5 (gateway and functions): what the exit criterion asked, what
shows it is met, and what the phase's scope still leaves open. Like
[console-designers-2026-11-phase-4-exit.md](console-designers-2026-11-phase-4-exit.md), it records
one moment and is not regenerated.

**Measured at** `a3322a64` on `main`, with the two changes still in review named below.

## The criterion

[ADR-0216](../adr/0216-integrations-come-in-three-tiers-and-functions-wait.md) decision 4 replaced the
phase's original criterion, which asked for a signed WebAssembly function on a canary ring: **a
till's external lookup and a delivery order both go through the gateway, and a partner places an
order through the public API.** Functions ([ADR-0199](../adr/0199-a-function-is-signed-pure-webassembly-that-returns-a-decision.md))
are deferred until the owner names a real case; nothing of them was built.

## The criterion is met

### A till's external lookup goes through the gateway

The till never reaches the internet. A lookup on a package screen goes till → edge → cloud →
the tenant's declared endpoint ([ADR-0198](../adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md),
[ADR-0214](../adr/0214-a-tills-external-lookup-is-one-catalogue-act.md)).

| Step | Pull requests | Evidence |
|---|---|---|
| A tenant declares its endpoints; each URL vetted by the webhook guard, each secret header sealed | #1265, #1268 | `crates/pos-cloud/tests/cloud/gateway_endpoints.rs`, the console's endpoints screen |
| The cloud makes a store's call: the store's `call_gateway` credential, the tenant's enabled endpoint, limits, breaker, deadline; every call recorded | #1266 | `crates/pos-cloud/tests/cloud/gateway_calls.rs` |
| The till may connect only to its own edge (CSP `connect-src`) | #1269 | the till replay fails on any CSP report |
| The edge runs a till's lookup through the cloud (`POST /api/gateway/calls`), as the act `integrations.lookup.run` | #1270 | `crates/pos-edge/tests/gateway_calls.rs` |
| A flow step that needs the cloud says what happens offline; publish refuses one that does not | #1274 | `pos-core` package checks, `NO_OFFLINE_BRANCH` |
| A package screen draws a lookup through the gateway, and follows the step's offline branch | #1275, #1277 | `ui/unit/package-lookup.test.ts` |

### A delivery order goes through the gateway

A vendor's own webhook reaches only the cloud; the store pulls the order, and answers the vendor
only through the cloud ([ADR-0083](../adr/0083-integration-doctrine.md) decision 3,
[ADR-0215](../adr/0215-a-delivery-order-names-its-vendor.md)). No vendor is named anywhere a store
runs.

| Step | Pull requests | Evidence |
|---|---|---|
| A vendor's signed webhook becomes an order on the relay `/v1/orders` uses; the store pulls it | #1267 | `connectors::a_signed_vendor_order_reaches_the_store_through_the_cloud` (`crates/pos-cloud/tests/cloud/connectors.rs`) |
| The order names its vendor, an opaque `vendor_id`, on `sales.order.opened` | #1282 | `vendor::a_delivery_order_names_its_vendor_on_the_order_it_opens` (`crates/pos-edge/tests/order_in.rs`) |
| The store answers its vendor through the cloud: accept, reject, ready, busy | #1283 | `connectors::a_store_answers_its_delivery_vendor_through_the_cloud` |
| The recipient is a personal record on the cloud, under the store's retention, never in the order, an event or a log | #1284 | `connectors::a_delivery_recipient_is_kept_apart_and_only_its_store_reads_it`; DPIA note in [`data-residency.md`](../data-residency.md) |
| The store keeps the recipient with the order and prints it once, on the delivery slip | #1286 | `a_delivery_order_keeps_its_recipient_with_the_order_and_tells_its_slip` (`crates/pos-edge/tests/relay_client.rs`) |
| An erasure of the recipient reaches the store's copy too | #1288 | `subjects::erasing_a_subject_the_cloud_and_a_store_hold_reaches_both`; `custom_values::a_subject_the_cloud_kept_for_a_store_is_held_by_that_store` |
| The `vendors` node: the vendors a store takes, `staff_confirm` and `prep_minutes` each; an answer may name the order by the store's own id | #1285 | `integrations::publishing_integrations_sends_a_store_the_vendors_it_takes`; `connectors::a_store_answers_its_delivery_vendor_through_the_cloud` |
| The store holds a vendor's orders for staff where the node says so, and accepts, rejects and readies them with the vendor through the cloud | `feat/store-takes-delivery-orders-by-vendor` (in review) | `a_vendor_s_orders_wait_for_staff_where_it_says_and_are_accepted_when_taken`, `a_store_rejects_and_readies_only_orders_of_its_vendors` (`crates/pos-edge/tests/relay_client.rs`) |

### A partner places an order through the public API

Tier 1 of ADR-0216: `POST /v1/orders` with a scoped API key ([ADR-0056](../adr/0056-public-order-intake.md);
[`guides/integrate-with-the-api.md`](../guides/integrate-with-the-api.md), "Placing an order"). The
order goes on the same relay a vendor's does, the store reprices it from its own menu and accepts it
offline, and a retry with the same reference is one order. Evidence in `crates/pos-cloud/tests/cloud.rs`
("Public order intake"): `orders_submit_accepts_and_creates`, `orders_submit_is_idempotent`,
`orders_unknown_item_is_bad_request`, `orders_for_another_tenants_store_is_not_found` and the budget
tests; the store's pull, `pulling_orders_requires_the_relay_orders_scope`, and
`crates/pos-edge/tests/relay_client.rs`. A partner hears back through signed webhooks filtered by
event type (#1264).

## Built beside the criterion

- **A store knows each person's roles** ([ADR-0213](../adr/0213-a-store-knows-each-persons-roles.md), #1261),
  closing the first item phase 4 left open.
- **Secrets under versioned per-tenant keys** ([ADR-0217](../adr/0217-secrets-are-sealed-under-versioned-tenant-keys.md)):
  the `KeyKeeper` port (#1276), tenant keys (#1278), rotation, re-seal and erasure by key (#1279),
  and production's key in `systemd-creds` (#1281).
- Two test fixes found on the way: the sign-out snapshot test (#1273) and the gateway tables in the
  integration truncate lists (#1280).

Every till change passed the full replay, 230 of 230 normally and on two CPUs, and the perf gate at
2× ([ADR-0208](../adr/0208-a-tap-is-measured-to-its-frame-ready-and-gated-per-pull-request.md)).

## What the phase's scope still leaves open

- **Functions** ([ADR-0199](../adr/0199-a-function-is-signed-pure-webassembly-that-returns-a-decision.md))
  wait for the owner to name a real case that tiers 1 and 2 cannot serve (ADR-0216 decision 3). No
  runtime dependency and no signing key were added.
- **Busy.** The cloud takes `DELIVERY_ANSWER_BUSY` (#1283), but no store control says a store is
  busy, so nothing sends it. A busy toggle on the till is new work with its own screen.
- **Answers are not durable.** A store's answers wait on a bounded in-memory queue (256) and are tried
  four times; a full queue, a restart, or a vendor still refusing after the last try loses the answer,
  logged by vendor and order id. A durable outbox for answers is open.
- **Ready needs the kitchen display.** `READY` is sent when a bump makes an order's last line; a store
  that does not bump tickets never sends it.
- **The delivery slip prints once.** It prints when the store takes the order; a printer that fails at
  that moment leaves no slip (the outcome is logged), and nothing reprints it. A reprint would need
  `data.personal.read` and an audit, as ADR-0206 decision 9 asks of a personal value.
- **The recipient's retention** is the store's personal-data retention, its locale's
  `default_retention_days`: 365 days in every country pack, each marked a starting value the operator
  must confirm. A shorter period for delivery recipients would be a new setting, and the owner's
  decision.
- **The tenant's obligations** for delivery recipients: the lawful basis under Decree 13/2023 (and the
  GDPR where it applies), the vendor agreement, and the DPIA sign-off before a live store takes
  delivery orders (gates L1, L2 and L6; the DPIA note is in [`data-residency.md`](../data-residency.md)).
- **One connector.** The only connector is the sandbox `delivery.demo`. A real marketplace is one cloud
  connector and its provider entry (ADR-0215 decision 6), each needing its vendor's agreement.
