# ADR-0168 — A settled bill records its tax components

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-06,
revised and accepted 2026-10-07
· Extends [ADR-0104](0104-multi-component-and-inclusive-tax.md),
[ADR-0159](0159-a-fee-is-configuration.md) decision 4,
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md) decision 9 and
[ADR-0164](0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md) decision 5
· Relates to [ADR-0162](0162-a-bill-is-taxed-at-the-rates-of-one-stated-moment.md) and
[ADR-0167](0167-a-till-has-its-own-cash-drawer.md) (ADR-0162 is Proposed)

## The problem

An Indian tax invoice and the GST return need CGST and SGST as separate amounts (ADR-0104). The
`tax` node carries them as each row's `components`, `pos_core::billing` splits each class's rounded
tax into them, and the receipt and the pre-bill print them under their rate. Nothing keeps them.
`billing.bill.settled`'s tax line records only `tax_class_id`, `taxable_base`, `rate_basis_points`
and `tax`: a component's name is a `String`, which the personal-data fence in `pos_proto::pii` does
not admit to the log. So a copy (ADR-0164) recovers them only while today's table charges the same
tax, and no rollup, report or export sums them. The name is also free text:
`PUT /admin/catalog/tax-rates` checks only that it is not empty. On 2026-10-07 the owner asked for
the world's standard practice, with each policy choice configurable rather than hard-coded.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Keep recomputing components from the rate table | Wrong once the table changes, and a report would have to replay each day's table |
| B | Record each name as free text | A free-text field in the immutable log, which the fence exists to stop |
| C | **Record them under a bounded name token the fence can admit** | One small type, a check where a table is authored, additive fields |

## Decision

Option **C**. Which components exist is the `tax` node's data, never a country's code, and whether
a receipt prints them is a setting.

1. **A component's name is a token**: two to eight upper-case ASCII letters and digits, starting
   with a letter, such as `CGST`, `SGST`, `UTGST`, `IGST` or `CESS`. A new `pos_proto` type,
   `TaxComponentName`, is built only through that check, and the fence admits it: it names a tax,
   and eight characters with no space, `@` or lower case cannot carry a person's name, an e-mail or
   a phone number. The `tax` node's `name` keeps its wire form. This narrows ADR-0104, which left
   the name unbounded; a country that prints other words maps the token to them in its pack.
2. **Checked where a table is authored and where it is applied.** `TaxRateTable` gains a check
   beside `unbalanced_rows` listing the rows whose names are not tokens, so `TaxComponent::new`
   stays infallible. The cloud's tax grid refuses such a row (`400`, naming it), as it refuses an
   unbalanced one, and marks a stored one for the next save without rewriting it. An edge given one
   charges the row's rate, gives that row no breakdown and logs why: the money never depends on the
   components.
3. **The record.** Each settled tax line gains `components`: `name`, `rate_basis_points` and `tax`,
   summing exactly to the line's tax with the residual on the last, as `split_components` already
   allocates (ADR-0104). Each fee line gains `tax_components`, its class shares' tax split the same
   way and summed by name and rate, so they sum to the fee's `tax` (ADR-0159). Both are left out
   when empty, so a bill settled before this, or in a country without components, records none;
   none reads as "not split", never as zero.
4. **`printing.receipt_tax_components`**, a switch, default `true`, at tenant, brand, store group
   and store, whose `since` is the release that ships it. On, a receipt, the pre-bill and a copy
   print each tax line's named components under its rate wherever the store's `tax` node defines
   them, as receipts do today; off, the tax line alone. A copy prints the recorded components, and
   a copy of a bill settled before this change prints as it does today.
5. **Where else components appear.** The Today tile stays the store's one total, with none
   (`docs/pos-spec.md` §17). The shift close report lists its bills' tax per component and rate;
   the cloud's daily revenue gains the tax per component name and rate, per store, folded from the
   settled tax lines; Reports shows it under the day's tax; and a new `revenue-tax.csv`
   (`business_date,currency_code,component_name,rate_basis_points,tax`) lists it, behind
   `console.reports.revenue` like `revenue-fees.csv`. All of it reads the recorded components, so
   any country whose `tax` node names components gets them with no code of its own. Rebuilding from
   the log (ADR-0036) invents none: a day before the change shows its tax without components, and a
   day that straddles it shows only those recorded, short of the day's tax by the rest.
6. **IGST stays out.** A restaurant's place of supply is the restaurant, so its sales are
   intra-state: CGST with SGST, or with UTGST in a union territory. ADR-0104's shape already carries
   an IGST row and left choosing per bill to `countries/in`; nothing here chooses per bill.
7. **ADR-0162 does not block this.** A line's components split the tax it was charged, from the row
   that charged it. When ADR-0162 freezes a bill's rates, the snapshot carries each rate's
   components, so the split follows the same moment.

## Consequences accepted

- **Vietnam and Japan are unchanged, byte for byte, either way the switch is set.** They publish no
  components, so every new list and map is empty and left out: the settle event, its chain hash,
  the receipt, the copy, the shift report and the rollup blob are the same bytes. Slice 2 holds
  that with a byte comparison against main of the settle events and of a print dump, as #631 did
  for paper. The new export holds only its header.
- **Additive.** `PROTOCOL_VERSION` stays 1 ([ADR-0024](0024-protocol-version-negotiation.md)).
  `settings.txt` gains one setting. `events.txt` lists a payload's top-level fields only, so these
  nested fields would not move it, and slice 1 extends it to a payload's parts. No migration (the
  rollup is a jsonb blob) and no permission.
- **Offline.** The components come from the `tax` node the edge holds and are recorded at the
  settle with the line down; the cloud's reports catch up from the outbox.
- **pos-proto, pos-core and the event schema change**, so this needs the owner's review
  (AGENTS.md §6).
- **Phase 5 slices**, in order, with rough changed lines:
  1. pos-proto: `TaxComponentName` and the fence, the table check, both fields, the snapshot (300).
  2. pos-core, pos-edge: named and fee components, the record, the copy, the switch on the
     receipt, the pre-bill and the copy, entering the register; goldens (500).
  3. pos-edge: the shift close report's tax per component (150).
  4. pos-cloud: the grid's check, the rollup, `revenue-tax.csv`, the OpenAPI document (400).
  5. dashboard: Reports per component, the grid's rule and marked rows (250).

## The owner's answers (2026-10-07)

> "Làm theo best practice chuẩn của thế giới đi và flexible configuration được chứ không phải
> hardcode": follow the world's standard practice, and configure each policy choice.

1. CGST and SGST on every receipt, or only on an invoice a guest asks for: every receipt, as the
   owner answered, under `printing.receipt_tax_components`, default `true`.
2. Components in the Today tile or only in Reports: the best-practice rule, so the tile stays one
   total and the shift close report, Reports and `revenue-tax.csv` show them, from the `tax` node.

Approved in full on 2026-10-07 ("đồng ý làm hết"), with the amendment above: ADR-0104's component
name is narrowed to a token.
