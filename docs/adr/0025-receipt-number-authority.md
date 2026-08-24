# ADR-0025 — The receipt number is gapless only while one store authority is reachable

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-19
**Amends** [`docs/pos-spec.md`](../pos-spec.md) §5 · **Relates to** [ADR-0003](0003-cattle-not-pets.md), [ADR-0005](0005-country-neutral-core.md)

**Context.** `pos-spec.md` §5 stated three things that cannot all be true at once:

1. receipt numbers increase without gaps per store;
2. two devices may work the same table concurrently;
3. offline operation never skips a number.

The failure is not a cloud outage. It is an **intra-store partition** — a dead Wi-Fi access point,
ordinary hardware failure. Two cashier devices that cannot see *each other* would each mint "the
next number" with no coordination, and collide. That is a CAP consequence, not an implementation
bug: no care inside a SQLite transaction fixes it, because the two devices write to different SQLite
files.

The stakes are not tidiness. [ADR-0003](0003-cattle-not-pets.md) records that duplicate numbering is
a **legal violation** for the invoice case, and the single-active lease exists precisely to prevent
it across machine replacement. The same reasoning applies within a store, and §5 as written gave it
no mechanism.

The legal invoice number is a **separate** concept ([ADR-0005](0005-country-neutral-core.md)) with
its own allocation behind `Fiscalization`. This record is only about the store's own receipt
counter; the two must never be conflated.

**Options considered** (from issue #8).

| | Approach | Holds | Breaks |
|---|---|---|---|
| **a** | A **store-local authority** owns the counter — the server, not each device | Survives a cloud outage; one writer, no collision | A device partitioned from the authority cannot settle |
| **b** | **Pre-allocated contiguous blocks per device** | Gapless per device, no coordination | Gapped per store, contradicting requirement 1 as written |
| **c** | Provisional device-local numbers, renumbered at day close | Gapless per store, eventually | The number on the printed receipt is not final — unacceptable |

**Decision.** Both (a) and (b) are legitimate, because which one is legal depends on the
jurisdiction, and this framework serves several. So the authority is **configuration**, not a fixed
guarantee:

```
store.billing.receipt_number_authority = store_server | device_blocks
```

- `store_server` (the default) is option (a). The store server owns one counter and hands out the
  next number inside the bill transaction. A device that cannot reach the server cannot settle — but
  it already cannot, because the server holds the database and drives the printers, so this adds no
  new failure mode. The honest guarantee: **gapless while a single store authority is reachable.**
- `device_blocks` is option (b), for jurisdictions Finance has confirmed accept per-device
  sequences. Each device draws a contiguous block; numbers are gapless *per device* and carry a
  device discriminator, and the store-level sequence is explicitly not gapless.

Option (c) is rejected outright: a receipt number that changes after it is printed is not a receipt
number.

**`pos-core`'s part.** The domain does not mint the number — minting is I/O, and the domain performs
none ([ADR-0013](0013-async-strategy.md)). The domain's decision to settle a bill *requires* a
receipt number as an input already allocated by the caller, so a bill cannot reach `SETTLED` without
one. Which authority allocated it, and how, is the edge adapter's concern, read from the capability
context. The domain enforces only that the number is present, is not reused within a store
(`uq_bills_store_id_receipt_number` at the schema, an invariant in the projection at the domain), and
is never conflated with an invoice number.

**Consequences.**

- §5's guarantee is restated to the one the system provides. A property test binds to the amended
  wording, not the impossible one.
- The receipt counter and the legal invoice number stay separate types and separate authorities.
  Nothing in the domain can accidentally use one for the other, because they are distinct newtypes.
- A store switching from `store_server` to `device_blocks` (or back) is a configuration change with a
  discontinuity in the sequence, which is expected and documented rather than a bug.
- The degraded mode is written down: partitioned from the authority under `store_server`, a device
  cannot settle, and the UI says so rather than inventing a number.
