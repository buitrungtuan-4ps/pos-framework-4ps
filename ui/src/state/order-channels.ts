// The store's order types and sale sources, as the till offers them on a new order (ADR-0260).
//
// The edge sends both with the price book (`GET /api/menu`): the active types, the store's default
// first, each with the base kind that sets the order's channel, and the custom sources the till may
// record. The names are HQ's, by language; a built-in the store left unnamed keeps the till's own
// words. An edge older than the lists sends neither, and the counter asks as it always did.

import { createSignal } from "solid-js";

import type { MenuResponse } from "../api/schemas";
import { locale, t, type MessageKey } from "../i18n";

export type OfferedType = MenuResponse["order_types"][number];
export type OfferedSource = MenuResponse["sale_sources"][number];

/** What the till records on an order it opens, beside the channel. */
export interface OrderRecording {
  readonly order_type?: string;
  readonly sale_source?: string;
}

const BUILT_IN_TYPES: Readonly<Record<string, MessageKey>> = {
  ORDER_TYPE_DINE_IN: "counter.eat_in",
  ORDER_TYPE_TAKEAWAY: "counter.take_away",
  ORDER_TYPE_DELIVERY: "x.type_delivery",
};

/** The channel each base kind opens an order on: the only thing a type changes about money. */
const BASE_CHANNEL: Readonly<Record<string, string>> = {
  ORDER_TYPE_DINE_IN: "SALES_CHANNEL_DINE_IN",
  ORDER_TYPE_TAKEAWAY: "SALES_CHANNEL_TAKEAWAY",
  ORDER_TYPE_DELIVERY: "SALES_CHANNEL_DELIVERY",
};

const [types, setTypes] = createSignal<readonly OfferedType[]>([]);
const [sources, setSources] = createSignal<readonly OfferedSource[]>([]);

/** Takes the lists the edge sent with the price book; an older edge's absent lists are empty. */
export function applyOrderChannels(response: Partial<Pick<MenuResponse, "order_types" | "sale_sources">>): void {
  setTypes(Array.isArray(response.order_types) ? response.order_types : []);
  setSources(Array.isArray(response.sale_sources) ? response.sale_sources : []);
}

/** The active types, the default first. */
export const offeredTypes = types;
/** The custom sources the till may record. */
export const offeredSources = sources;

/** Whether New order asks the type: the store lists one beyond the built-ins, or a source. */
export function listsBeyondBuiltIns(): boolean {
  return sources().length > 0 || types().some((offered) => BUILT_IN_TYPES[offered.order_type] === undefined);
}

/** A name HQ gave, in the till's language, else English, else none. */
export function localName(names: Readonly<Record<string, string>> | undefined): string | undefined {
  if (names === undefined) return undefined;
  return names[locale()] ?? names.en ?? Object.values(names)[0];
}

/** An order type as the counter shows it. */
export function typeLabel(offered: OfferedType): string {
  return localName(offered.names) ?? t(BUILT_IN_TYPES[offered.order_type] ?? "x.type_unspecified");
}

/** A source as the counter shows it. */
export function sourceLabel(offered: OfferedSource): string {
  return localName(offered.names) ?? offered.source;
}

/** The sales channel a type opens its order on. */
export function channelOf(offered: OfferedType): string {
  return BASE_CHANNEL[offered.base_order_type] ?? "SALES_CHANNEL_TAKEAWAY";
}
