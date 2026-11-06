// The order screen's menu search: the items whose captions hold what the operator typed
// (`../lib/search.ts`).

import { createMemo, type Accessor } from "solid-js";

import type {
  LayoutButton,
  LayoutCategory,
  MenuItemResponse,
} from "../api/schemas";
import { fold, matches } from "../lib/search";

/**
 * Every caption an item can be found by: the price book's own name, plus whatever the console
 * wrote on each button pointing at it (ADR-0066). The two differ on purpose — a button inside a
 * "Pizza" category can say "Large" — and an operator who knows an item by the grid's word for it
 * would otherwise search for it and be told there is no such thing.
 *
 * Folded here rather than at the comparison, so a caption is normalized once rather than once per
 * item per keystroke. Deduplicated on the folded form too: two buttons whose captions differ only
 * in case or tone marks are one string to search, and keeping both would scan the same text twice.
 */
function captionsOf(
  items: readonly MenuItemResponse[],
  layout: readonly LayoutCategory[],
): Map<string, string[]> {
  const byItem = new Map<string, string[]>(
    items.map((item) => [item.menu_item_id, [fold(item.display_name)]]),
  );
  const record = (button: LayoutButton) => {
    const known = byItem.get(button.menu_item_id);
    const folded = fold(button.label);
    if (known !== undefined && !known.includes(folded)) {
      known.push(folded);
    }
  };
  for (const category of layout) {
    category.buttons.forEach(record);
    for (const subcategory of category.subcategories) {
      subcategory.buttons.forEach(record);
    }
  }
  return byItem;
}

/**
 * The items matching `query`, in the price book's own order; empty while nothing is typed. A match
 * is drawn with `display_name`, not the caption that matched: a button's caption is shorthand that
 * means something inside its category and nothing outside it, and a flat result list is outside it.
 *
 * The captions are built when a search starts and kept while it lasts, once per menu-and-layout
 * rather than per keystroke; with no search they are not built at all, so opening an order screen
 * does not pay for them. The needle is its own memo for the same reason: folded once per keystroke
 * rather than once per item scanned.
 */
export function createMenuSearch(
  query: Accessor<string>,
  items: Accessor<readonly MenuItemResponse[]>,
  layout: Accessor<readonly LayoutCategory[]>,
): Accessor<MenuItemResponse[]> {
  const searching = createMemo(() => query().trim() !== "");
  const captions = createMemo(() =>
    searching() ? captionsOf(items(), layout()) : undefined,
  );
  const needle = createMemo(() => fold(query().trim()));
  return createMemo(() => {
    const known = captions();
    return known === undefined
      ? []
      : items().filter((item) =>
         matches(
           needle(),
           known.get(item.menu_item_id) ?? [fold(item.display_name)],
         ),
       );
  });
}
