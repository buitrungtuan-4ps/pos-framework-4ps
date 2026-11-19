// A weighed item (ADR-0223): sold by the kilogram, its line counting net grams, weighed on the
// store's scale beside the till or, where the store allows it, typed by hand. Tapping such an item
// asks for its weight rather than adding one; the edge reads the scale and prices the line.

import { createSignal } from "solid-js";

import { api } from "../api/client";
import type { MenuItemResponse, MenuResponse, WeighRequest } from "../api/schemas";
import { editWrite } from "./edge-write";

const [weighing, setWeighing] = createSignal<MenuItemResponse | null>(null);
const [manual, setManual] = createSignal(false);
const [leastGrams, setLeastGrams] = createSignal(1);

/** Applies the menu read's weighing settings; an edge older than them takes no typed weight. */
export function applyWeighing(response: MenuResponse): void {
  setManual(response.weighing_manual_entry === true);
  setLeastGrams(response.weighing_min_grams ?? 1);
}

/** Whether a weight may be typed by hand here (`weighing.manual_entry`, unless the law forbids it). */
export function manualWeightAllowed(): boolean {
  return manual();
}

/** The least net grams a weighed line carries (`weighing.min_grams`). */
export function minGrams(): number {
  return leastGrams();
}

/** Asks for the weight of an item sold by weight, and says whether it did. */
export function askWeight(item: MenuItemResponse): boolean {
  if (item.sold_by_weight !== true) {
    return false;
  }
  setWeighing(item);
  return true;
}

/** The item being weighed, or `null`. */
export function weighingItem(): MenuItemResponse | null {
  return weighing();
}

export function stopWeighing(): void {
  setWeighing(null);
}

/** Adds the weighed line on `orderId`, from the scale or the grams typed, through the edge-write path. */
export function weigh(orderId: string, item: MenuItemResponse, typedGrams: number | null, reread: () => void): Promise<unknown> {
  const body: WeighRequest =
    typedGrams === null
      ? { menu_item_id: item.menu_item_id, source: "WEIGHT_SOURCE_SCALE" }
      : { menu_item_id: item.menu_item_id, source: "WEIGHT_SOURCE_MANUAL", net_grams: typedGrams };
  return editWrite("sales.line.weigh", { send: () => api.weighLine(orderId, body), reread }).then((line) => {
    setWeighing(null);
    return line;
  });
}

/** A net weight in grams as the till shows it, in kilograms to the gram: `375` is `0.375 kg`. */
export function kilograms(grams: number): string {
  return `${Math.floor(grams / 1000)}.${String(grams % 1000).padStart(3, "0")} kg`;
}
