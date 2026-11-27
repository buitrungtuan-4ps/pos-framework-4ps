// The `order_channels` settings as the console edits them (ADR-0260): the order types, each mapped
// onto a base kind, and the sale sources beyond the built-ins. The value reaches the console as
// plain JSON; this reads it, edits it and checks it by the register's rules before the cloud does,
// which checks them again. Nothing here is personal data: tokens and the names HQ gives them.

import type { Json } from "../api/types";

/** The register's kinds for the two lists. */
export const ORDER_TYPES_KIND = "SETTING_KIND_ORDER_TYPES";
export const SALE_SOURCES_KIND = "SETTING_KIND_SALE_SOURCES";

/** The base kinds a type maps onto, which alone set an order's channel, price and tax. */
export const BASE_KINDS = ["ORDER_TYPE_DINE_IN", "ORDER_TYPE_TAKEAWAY", "ORDER_TYPE_DELIVERY"] as const;
/** The sources whose orders a type may apply to. */
export const APPLIES_TO = ["SALE_SOURCE_QR", "SALE_SOURCE_API", "SALE_SOURCE_APP"] as const;
/** The built-in sources: listing one only renames it. `SALE_SOURCE_APP` is named by its connection. */
export const BUILT_IN_SOURCES = ["SALE_SOURCE_POS", "SALE_SOURCE_QR", "SALE_SOURCE_KIOSK", "SALE_SOURCE_API"] as const;

export const MAX_TYPES = 32;
export const MAX_SOURCES = 64;
export const MAX_NAME = 48;
const TOKEN_MAX = 48;

export type Names = { readonly [language: string]: string };

export interface OrderTypeEntry {
  readonly order_type: string;
  readonly base_order_type: string;
  readonly names?: Names;
  readonly active?: boolean;
  readonly default?: boolean;
  readonly applies_to_sources?: readonly string[];
}

export interface SourceEntry {
  readonly source: string;
  readonly names?: Names;
  readonly active?: boolean;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** A list value as entries; anything else reads as none. */
export function readEntries<T>(value: Json | undefined): readonly T[] {
  return Array.isArray(value) ? (value.filter(isObject) as unknown as T[]) : [];
}

/** The token a typed name suggests: `Tự đến lấy` under `ORDER_TYPE_` becomes `ORDER_TYPE_TU_DEN_LAY`. */
export function tokenFrom(prefix: "ORDER_TYPE_" | "SALE_SOURCE_", name: string): string {
  const rest = name
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/đ/gi, "d")
    .toUpperCase()
    .replace(/[^A-Z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  return (prefix + rest).slice(0, TOKEN_MAX);
}

/** Whether `token` is a well-formed token of `prefix`. */
export function soundToken(prefix: string, token: string): boolean {
  return token.length <= TOKEN_MAX && token.startsWith(prefix) && /^[A-Z0-9_]+$/.test(token.slice(prefix.length)) && token.length > prefix.length;
}

/** Drops empty names, so a cleared field writes nothing. */
export function withName(names: Names | undefined, language: string, name: string): Names {
  const next: Record<string, string> = { ...(names ?? {}) };
  if (name.trim() === "") {
    delete next[language];
  } else {
    next[language] = name;
  }
  return next;
}

/** An entry written without its defaults, as the node carries it. */
function compact<T extends object>(entry: T): T {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(entry)) {
    if (value === undefined) continue;
    if (key === "active" && value === true) continue;
    if (key === "default" && value === false) continue;
    if (key === "names" && isObject(value) && Object.keys(value).length === 0) continue;
    if (key === "applies_to_sources" && Array.isArray(value) && value.length === 0) continue;
    out[key] = value;
  }
  return out as T;
}

/** The list as the setting's value. */
export function toValue(entries: readonly object[]): Json {
  return entries.map(compact) as unknown as Json;
}

/** What is wrong with a list, in the register's terms, or `null`. */
export type ListFault =
  | { readonly kind: "too_many"; readonly max: number }
  | { readonly kind: "bad_token"; readonly token: string }
  | { readonly kind: "duplicate"; readonly token: string }
  | { readonly kind: "needs_english"; readonly token: string }
  | { readonly kind: "name_too_long"; readonly token: string }
  | { readonly kind: "two_defaults" }
  | { readonly kind: "inactive_default"; readonly token: string };

function namesFault(token: string, names: Names | undefined, builtIn: boolean): ListFault | null {
  if (!builtIn && (names?.en ?? "").trim() === "") return { kind: "needs_english", token };
  if (Object.values(names ?? {}).some((name) => [...name].length > MAX_NAME)) return { kind: "name_too_long", token };
  return null;
}

export function typesFault(entries: readonly OrderTypeEntry[]): ListFault | null {
  if (entries.length > MAX_TYPES) return { kind: "too_many", max: MAX_TYPES };
  const seen = new Set<string>();
  for (const entry of entries) {
    const builtIn = (BASE_KINDS as readonly string[]).includes(entry.order_type);
    if (!soundToken("ORDER_TYPE_", entry.order_type) || entry.order_type === "ORDER_TYPE_UNSPECIFIED") {
      return { kind: "bad_token", token: entry.order_type };
    }
    if (seen.has(entry.order_type)) return { kind: "duplicate", token: entry.order_type };
    seen.add(entry.order_type);
    const fault = namesFault(entry.order_type, entry.names, builtIn);
    if (fault !== null) return fault;
    if (entry.default === true && entry.active === false) return { kind: "inactive_default", token: entry.order_type };
  }
  if (entries.filter((entry) => entry.default === true).length > 1) return { kind: "two_defaults" };
  return null;
}

export function sourcesFault(entries: readonly SourceEntry[]): ListFault | null {
  if (entries.length > MAX_SOURCES) return { kind: "too_many", max: MAX_SOURCES };
  const seen = new Set<string>();
  for (const entry of entries) {
    const builtIn = (BUILT_IN_SOURCES as readonly string[]).includes(entry.source);
    if (!soundToken("SALE_SOURCE_", entry.source) || ["SALE_SOURCE_APP", "SALE_SOURCE_UNSPECIFIED"].includes(entry.source)) {
      return { kind: "bad_token", token: entry.source };
    }
    if (seen.has(entry.source)) return { kind: "duplicate", token: entry.source };
    seen.add(entry.source);
    const fault = namesFault(entry.source, entry.names, builtIn);
    if (fault !== null) return fault;
  }
  return null;
}
