// A tiny in-memory menu, standing in until the cloud config tree syncs the real one (P7). The edge
// does not hold the menu; the device sends the amounts it captured from what it is showing the guest
// (`sales.order_line.added` §14.2), so the menu lives here on the client for now.
//
// Every id is a ULID string, because that is what the edge parses. `TAX_CLASS_STANDARD` is the
// bootstrap's single class (`EdgeSession::standard_tax_class`, `Ulid::from_u128(1)`), so a line's tax
// resolves against the edge's one configured rate.

import type { Ratio } from "./money";

// Left-pads a short Crockford code to a 26-character ULID. Leading zeros keep the value small and
// valid; the codes below use only Crockford characters (no I, L, O, U).
const ulid = (code: string): string => code.padStart(26, "0");

export const TAX_CLASS_STANDARD = ulid("1");

// 10% VAT, the bootstrap standard rate. The edge recomputes tax from its own rate table; this rides
// on the line as the rate shown to the guest at the moment of the sale.
const VAT_10: Ratio = { numerator: 1000, denominator: 10000 };

export interface MenuItem {
  id: string;
  name: string;
  unitPriceMinor: number;
  taxClassId: string;
  taxRate: Ratio;
}

export const CURRENCY = "VND";

export const MENU: readonly MenuItem[] = [
  { id: ulid("P01"), name: "Margherita", unitPriceMinor: 150_000, taxClassId: TAX_CLASS_STANDARD, taxRate: VAT_10 },
  { id: ulid("P02"), name: "Marinara", unitPriceMinor: 140_000, taxClassId: TAX_CLASS_STANDARD, taxRate: VAT_10 },
  { id: ulid("P03"), name: "Diavola", unitPriceMinor: 190_000, taxClassId: TAX_CLASS_STANDARD, taxRate: VAT_10 },
  { id: ulid("P04"), name: "Quattro Formaggi", unitPriceMinor: 210_000, taxClassId: TAX_CLASS_STANDARD, taxRate: VAT_10 },
  { id: ulid("D01"), name: "Tiramisu", unitPriceMinor: 90_000, taxClassId: TAX_CLASS_STANDARD, taxRate: VAT_10 },
  { id: ulid("B01"), name: "Still Water", unitPriceMinor: 20_000, taxClassId: TAX_CLASS_STANDARD, taxRate: VAT_10 },
] as const;
