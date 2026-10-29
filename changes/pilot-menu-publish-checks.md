---
kind: fixed
upgrade_note: "`POST /admin/catalog/publish` now answers `422` for a menu the store could not sell: an item priced in another currency than the store's, or sold on a channel where the store's tax table has no rate for its class. Each item is a detail, `items.<menu_item_id>`, with `CURRENCY_MISMATCH` or `TAX_RATE_NOT_CONFIGURED`. A menu that published before and has such an item is refused until the price or the tax table is fixed; a store whose tax table is empty is refused every item, so give it its rates (0% is a rate) before publishing a menu."
---
- **A menu publish checks prices against the store, as a fee publish does.** A placement priced in
  a currency other than the store's, or an item sold on a channel the store has no tax rate for, is
  refused at publish naming the item and why, in the console's publish bar, in English and
  Vietnamese, instead of publishing cleanly and failing every sale of it at the till.
