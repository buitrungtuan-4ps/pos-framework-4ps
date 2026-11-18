// The till's index of the menu's barcodes and PLUs (ADR-0222 decision 2): built once when the menu
// arrives, so a scan or a keyed PLU resolves to its item with no round trip. The edge still prices
// the line. Bounded by the menu, and at most `CODES_MAX` codes are kept.

import { createSignal } from "solid-js";

import type { MenuItemResponse } from "../api/schemas";

/** The most codes indexed: the ADR's bound for one menu. */
export const CODES_MAX = 20_000;

/** Where a store sets no window: how quickly a scanner's keystrokes arrive (`scan.keystroke_window_ms`). */
export const DEFAULT_SCAN_WINDOW_MS = 50;

const [index, setIndex] = createSignal<ReadonlyMap<string, string>>(new Map<string, string>());
const [window, setWindow] = createSignal(DEFAULT_SCAN_WINDOW_MS);
const [listening, setListening] = createSignal(false);

/** Indexes the menu's codes, and takes the store's scan window and whether it sells by barcode. */
export function indexCodes(
  items: readonly MenuItemResponse[],
  windowMs: number | undefined,
  barcodeEnabled: boolean | undefined,
): void {
  const next = new Map<string, string>();
  for (const item of items) {
    for (const code of [...(item.barcodes ?? []), ...(item.plu ? [item.plu] : [])]) {
      if (next.size < CODES_MAX && !next.has(code)) {
        next.set(code, item.menu_item_id);
      }
    }
  }
  setIndex(next);
  setWindow(Number.isInteger(windowMs) && (windowMs as number) >= 20 && (windowMs as number) <= 200 ? (windowMs as number) : DEFAULT_SCAN_WINDOW_MS);
  setListening(barcodeEnabled === true);
}

/** The item a scanned or keyed code names, or null for a code the menu does not carry. */
export function resolveCode(code: string): string | null {
  return index().get(code) ?? null;
}

/** The most milliseconds between two keystrokes of one scan. */
export function scanWindowMs(): number {
  return window();
}

/** Whether the store sells by barcode, so a screen that can take a scan listens for one. */
export function scansListened(): boolean {
  return listening();
}
