---
kind: added
upgrade_note: "The cloud's `catalog_items` table gains a `codes` jsonb column (migration 0103, additive, default `{}`). The item authoring routes take an optional `codes` object (`barcodes`, `plu`, `open_price`) and answer with those fields; a malformed code is `400` and a code another active item carries is `409`, naming both items, and an update that omits `codes` keeps them. The published `MenuEntry` and `GET /api/menu` items gain optional `barcodes`, `plu` and `open_price`."
---
- **Items carry barcodes, a PLU and an open price.** The catalogue keeps up to eight barcodes with
  valid check digits and a PLU per item, unique within the menu, and the bounds of a price the
  cashier types for an open-price item; the published menu carries them to the store (ADR-0222).
