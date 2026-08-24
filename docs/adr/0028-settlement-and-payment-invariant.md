# ADR-0028 — What "payments sum to the bill" actually means

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-19
**Amends** [`docs/architecture.md`](../architecture.md) §5, [`docs/pos-spec.md`](../pos-spec.md) §5

**Context.** `architecture.md` §5 lists "payments always sum to the bill total" among the invariants
`pos-core` must enforce, and `pos-spec.md` §14 promises property tests bind to it. As written it is
false, and a test against it would either fail on legitimate data or be weakened until it proves
nothing. Four ordinary things break it: **tips** (stored separately from the sale, §5), **over-tender
and change** (cash tendered is not cash applied), **cash rounding** (rounding to the nearest 500 VND
changes what is collected relative to what is owed), and **partial payments** (several methods on one
bill are explicitly supported).

**Decision.** The invariant `pos-core` enforces, for a bill in `SETTLED`:

```
sum(payment.applied_to_bill)  ==  bill.total_due

bill.total_due  ==  subtotal - discounts - comps
                    + service_charge + tax + rounding_adjustment

sum(payment.tendered) - sum(payment.applied_to_bill) - sum(tip)  ==  change_given
```

Three consequences for the data model, which is why this is settled before P3 rather than during it.

1. **A payment carries both `tendered` and `applied_to_bill`.** One field cannot be both what the
   guest handed over and what was put against the bill. Change and over-tender live in the gap.
2. **Tips are a separate ledger**, never a component of `total_due`. `tip_amount` sits beside the
   sale, adjustable after capture (`billing.tip.adjusted`), and a distribution report reads that
   ledger. A tip is money that moved and is not revenue, so folding it into the total corrupts both
   the revenue figure and the reconciliation.
3. **Cash rounding materialises as an explicit `rounding_adjustment`**, a line on the bill, not a
   silent adjustment. That is what keeps the payment sum exact *and* makes the printed receipt
   reconcile line-by-line to the printed total — which a VAT invoice must.

**Two rounding questions the issue left open, answered here.**

- **Tax rounds per tax-class subtotal.** Rounding per line and summing does not equal rounding the
  sum; a VAT invoice that does not reconcile is a real problem. So the domain groups lines by
  `tax_class`, applies the channel-keyed rate to each subtotal, rounds once per class, and sums. A
  property test asserts the printed per-class tax lines sum to the printed tax total. Per-line and
  per-bill were both rejected: per-line accumulates rounding error the invoice cannot explain, and
  per-bill loses the per-class breakdown a VAT invoice requires.
- **Service-charge taxability is configuration**, `store.tax.service_charge_taxable`, because
  jurisdictions differ and `pos-core` is country-neutral ([ADR-0005](0005-country-neutral-core.md)).
  It defaults to `true`, matching §5's placement of the service charge after discounts and before
  tax. The domain reads the flag; it does not hard-code the answer. When true, the service charge
  joins the taxable base for its configured tax class before tax is computed.

**The settlement transition itself** is the other half of this record. `bill:settle` is a one-time
transition (`pos-spec.md` §14.4): a second attempt returns `FAILED_PRECONDITION`. The domain makes
that a property of the `Bill` state machine — `SETTLED` is terminal — so the exclusivity does not
depend on a lock. The soft lock the UI takes when a payment screen opens is an optimistic nicety on
top; correctness does not rest on it. This is the same terminal-state reasoning ADR-0029 applies to
line merges, and the two records are meant to be read together.

**Consequences.**

- `Money` arithmetic stays inside the single `div_round` primitive in `pos_proto::money`; this record
  adds no second rounding path, it fixes *where* rounding happens (per tax-class subtotal) and *what*
  it produces (an explicit line).
- The four data-correctness laws in `pos-spec.md` §14 gain a corrected §14 payment law, and the
  property test binds to `sum(applied) == total_due` plus the change identity above, not to the
  false original.
- `architecture.md` §5's one-line invariant is replaced by a pointer to this record, so the codebase
  has one statement of the rule rather than a true one and a false one.
- A refund is a **new, signed** movement against a settled bill, not a mutation of it — the settled
  bill stays terminal, and the refund is its own event. That keeps the audit log append-only and the
  settled total immutable.
