-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- How a rate is broken out on the invoice (ADR-0104, ADR-0105). `rate_bps` stays the authority on
-- what the guest pays; this column says how that total is composed, for a country whose law requires
-- the parts printed separately.
--
-- India is the case it exists for: an intra-state tax invoice charged 5% GST must show **CGST 2.5%
-- and SGST 2.5% on separate lines**, because the two halves go to different governments. Printing
-- the sum is not a terser rendering of the same fact — it is not a valid invoice. `countries/in`
-- publishes that breakdown as a default, and until this column existed the console could not edit it:
-- a tenant could trade on the pack's rates and could not author its own.
--
-- `jsonb` rather than a child table, because the shape is exactly `pos_proto::TaxComponent`
-- (`[{"name":"CGST","rate":250}]`), it is read and written whole with its row, and nothing ever
-- queries into it. A child table would add a join, a second delete on the wholesale replace, and an
-- ordering column, to store a list of at most a handful of pairs.
--
-- Additive and defaulted, so every existing row reads back as the empty list it always meant: no
-- components is "one rate, printed as one line", which is Vietnam, Japan, and most of the world.
-- Forward-only, applied idempotently on every boot (ADR-0017). No backfill — the default is correct.
--
-- The parts must sum to `rate_bps`, and that is **not** a CHECK here: the invariant spans the JSON
-- and the integer column, it is checked where the table is authored
-- (`TaxRateTable::unbalanced_rows`, refused with a message naming the row), and a database error on
-- a config save would arrive as a 500 rather than as something an operator can act on.

ALTER TABLE catalog_tax_rates
    ADD COLUMN IF NOT EXISTS components jsonb NOT NULL DEFAULT '[]'::jsonb;
