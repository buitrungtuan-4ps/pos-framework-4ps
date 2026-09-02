// Small helpers shared by the catalog sub-screens (ADR-0082, Track F3). The 1,898-line monolith is
// split into kit-based sub-screens under `screens/catalog/`; the few things they all need — the
// channel-label map, the per-locale/price-sheet helpers, an error-to-message coercion, and the
// active/archived status pill — live here so no sub-screen re-declares them. All user-visible text is
// still fed through `t()` (ADR-0020); this file carries none of its own beyond the status labels.

import type { JSX } from "solid-js";

import { ApiError } from "../../api/client";
import type { EntityStatus, SalesChannel } from "../../api/types";
import { SALES_CHANNELS } from "../../api/types";
import { t, type MessageKey } from "../../i18n";
import { StatusBadge } from "../../components/kit";

/** The i18n key for each sales channel's short label (used by the Menus placement editor). */
export const CHANNEL_LABEL: Record<SalesChannel, MessageKey> = {
  SALES_CHANNEL_DINE_IN: "channel.dineIn",
  SALES_CHANNEL_TAKEAWAY: "channel.takeaway",
  SALES_CHANNEL_DELIVERY: "channel.delivery",
  SALES_CHANNEL_QR: "channel.qr",
  SALES_CHANNEL_API: "channel.api",
};

/** A blank per-channel price sheet: every channel maps to `null` (not priced). Amounts are integer
 *  minor units (what `MoneyField` edits and `amount_minor` stores). */
export const emptyPriceSheet = (): Record<SalesChannel, number | null> =>
  Object.fromEntries(SALES_CHANNELS.map((channel) => [channel, null])) as Record<
    SalesChannel,
    number | null
  >;

// Drops blank-key or blank-value entries from an edited per-locale name map and trims both sides, so a
// row the operator left empty never ships as a `""` translation (ADR-0074). The server cleans too;
// this keeps the request tidy.
export const cleanTranslations = (raw: Record<string, string>): Record<string, string> => {
  const cleaned: Record<string, string> = {};
  for (const [locale, name] of Object.entries(raw)) {
    const key = locale.trim();
    const value = name.trim();
    if (key && value) {
      cleaned[key] = value;
    }
  }
  return cleaned;
};

/**
 * The message to surface for a caught API failure — the server's message, or a stringified fallback.
 *
 * A `412` is the one failure whose server message is not the most useful thing to show: it says the
 * record changed, but not what the reader should do about it (ADR-0094). The prose here says both,
 * and [`isStale`] is what tells the caller to reload so the reader can see what changed.
 */
export const errorMessage = (caught: unknown): string => {
  if (isStale(caught)) {
    return t("catalog.stale");
  }
  return caught instanceof ApiError ? caught.message : String(caught);
};

/**
 * Whether a caught failure is "somebody else saved this first" (ADR-0094).
 *
 * The recovery is always a reload, never a retry: retrying would re-apply the overwrite the refusal
 * exists to prevent.
 */
export const isStale = (caught: unknown): boolean => caught instanceof ApiError && caught.isStale;

/** The already-translated active/archived label. */
export const statusLabel = (status: EntityStatus): string =>
  status === "archived" ? t("catalog.statusArchived") : t("catalog.statusActive");

/** The active/archived status pill every catalog list shows, rendered from the shared label. */
export function StatusCell(props: { status: EntityStatus }): JSX.Element {
  return (
    <StatusBadge
      label={statusLabel(props.status)}
      tone={props.status === "archived" ? "archived" : "active"}
    />
  );
}
