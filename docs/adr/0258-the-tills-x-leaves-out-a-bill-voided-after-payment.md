# ADR-0258 — The till's X leaves out a bill voided after payment

**Status** Accepted 2026-11-27 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-27 · Phase 11 ("data matched from edge to console") · Details
[ADR-0187](0187-a-shift-is-handed-over-at-the-till-and-read-there.md) decision 4 under
[ADR-0184](0184-a-settled-bill-is-voided-whole-not-refunded.md) decision 9

## Context

ADR-0184 decision 9 says the sales, tax and tender totals of a business date exclude a voided bill, and
the console's X/Z does so. ADR-0187 decision 4 gives the till an X per shift and for the store, with
"sales and bill count", "totals by tender" and "voids after payment", but does not say whether a
shift's sales keep a bill that is voided later. The edge keeps it: a real store run on one shift showed
the till's X at 17 bills and 3,091,000 ₫ of sales, beside "7 voided after payment", while the console
showed 10 bills and 1,846,000 ₫ for the same day. Both are arithmetic of the same events, but a
manager reading both sees two different sales figures.

## Options considered

1. **Keep the till as it is and label the difference.** Two definitions of "sales" stay in one product.
2. **Take the bill out of the shift that settled it**, as the cloud takes it out of the day it settled
   on, while that shift is open.
3. **Take it out of the shift the void is done in.** A shift that sold nothing could show negative
   sales.

## Decision

1. A void after payment takes the bill out of the X of **the shift that settled it, while that shift
   is open**: its bill count, sales, discounts and tax components, exactly what its settle added; and
   each of its payments out of the tender totals of the shift that took it, while that shift is open.
2. A bill whose settling shift is already closed leaves that shift's close and its printed report as
   they are (whether a Z is immutable stays open, ADR-0187); the console's day figures still take it
   out.
3. Unchanged: "voids after payment" (count) and the voided cash are reported at the drawer the void is
   done at, and that drawer's expected cash falls by the bill's cash (ADR-0184 decision 6).
4. The till's X and the console's X/Z therefore agree on a business date whose shifts are open: bills,
   sales, tenders and voids after payment.

## Consequences accepted

- The till's X changes meaning for sales and tenders, with an upgrade note; the routes and their
  fields are unchanged (ADR-0111).
- The edge remembers, for each paid bill, what its settle and each payment added and to which shift.
  It knows the settling shift and the cash today; it keeps the rest from now on, folded from its log,
  so a restart rebuilds the same X.
