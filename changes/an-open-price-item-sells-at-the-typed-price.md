---
kind: added
upgrade_note: "`POST /api/orders/{id}/lines` takes an optional `typed_unit_price`. An item the catalogue marks open-price is refused with `PRICE_OUT_OF_RANGE` unless its line carries a price of one within the item's bounds, on the counter's path and on the table path; every other item is priced as before."
---
- **An open-price item sells at the price the cashier types.** Within the item's published lowest
  and highest price the edge takes the typed price with no manager; outside them the line is
  refused, and a price override is the way (ADR-0222).
