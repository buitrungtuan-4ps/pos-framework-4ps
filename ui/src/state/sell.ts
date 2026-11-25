// What the order screen lends the quick sale's parts (ADR-0222 decision 5): how to sell an item as
// if its button were tapped, whether selling is held, and the counter order on screen. And the price
// a cashier types for an open-price item (decision 4), which rides the next counter line of that
// item and nothing else.

import { createContext, createSignal, useContext } from "solid-js";

import type { MenuItemResponse } from "../api/schemas";
import type { Money } from "../lib/money";

// A weighed item asks its weight rather than its price (ADR-0223); the order screen asks both here.
export { askWeight, weighedLine } from "./weigh";

/** The order screen's selling, as `scan_field` and `plu_keypad` use it. */
export interface Sell {
  readonly sell: (item: MenuItemResponse) => void;
  readonly disabled: () => boolean;
  /** The counter order on screen, once it exists; a table's order or a draft has none. */
  readonly orderId: () => string | undefined;
  /** The counter order, opening a draft walk-in first; `undefined` where there is none to open. */
  readonly openOrder: () => Promise<string | undefined>;
  /** Whether a weighed item can be sold here: on a walk-in, drafted or open, and not on a table. */
  readonly weighable: () => boolean;
}

export const SellContext = createContext<Sell>({
  sell: () => undefined,
  disabled: () => true,
  orderId: () => undefined,
  openOrder: () => Promise.resolve(undefined),
  weighable: () => false,
});

export function useSell(): Sell {
  return useContext(SellContext);
}

const [pricing, setPricing] = createSignal<{ item: MenuItemResponse; then: (item: MenuItemResponse) => void } | null>(null);
let typed: { menuItemId: string; price: Money } | null = null;

/**
 * Asks the price of an open-price item first, and says whether it did; `then` sells it once typed.
 * An item with its price already typed, or a fixed-price one, is not asked.
 */
export function askOpenPrice(item: MenuItemResponse, then: (item: MenuItemResponse) => void): boolean {
  if (item.open_price === undefined || typed?.menuItemId === item.menu_item_id) {
    return false;
  }
  setPricing({ item, then });
  return true;
}

/** The item whose price is being asked, or `null`. */
export function pricingItem(): MenuItemResponse | null {
  return pricing()?.item ?? null;
}

/** The price typed for the item asked about: it sells, carrying the price. */
export function priceTyped(price: Money): void {
  const asked = pricing();
  if (asked !== null) {
    typed = { menuItemId: asked.item.menu_item_id, price };
    setPricing(null);
    asked.then(asked.item);
  }
}

export function cancelPricing(): void {
  setPricing(null);
}

/** The price typed for `menuItemId`'s next line, once: the line that carries it clears it. */
export function takeTypedPrice(menuItemId: string): Money | undefined {
  if (typed?.menuItemId !== menuItemId) {
    return undefined;
  }
  const price = typed.price;
  typed = null;
  return price;
}
