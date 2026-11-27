// The till's X by channel and order type, and its tenders with each delivery app named (ADR-0259
// decisions 4 and 5). The figures are the edge's lines; this only names and groups them.

import type { XFigures } from "../api/schemas";
import { t, type MessageKey } from "../i18n";
import type { Money } from "./money";

/** One line of the edge's settled bills by channel and order type. */
export type ChannelLine = XFigures["channels"][number];

const SOURCES: Readonly<Record<string, MessageKey>> = {
  SALE_SOURCE_POS: "x.source_pos",
  SALE_SOURCE_QR: "x.source_qr",
  SALE_SOURCE_KIOSK: "x.source_kiosk",
  SALE_SOURCE_API: "x.source_api",
};

const ORDER_TYPES: Readonly<Record<string, MessageKey>> = {
  ORDER_TYPE_DINE_IN: "x.type_dine_in",
  ORDER_TYPE_TAKEAWAY: "x.type_takeaway",
  ORDER_TYPE_DELIVERY: "x.type_delivery",
};

/** A delivery app by the name the store gives it, or its id's tail once it no longer lists it. */
function appName(app: { vendor_id?: string; name?: string }): string {
  return app.name ?? t("x.app_unnamed", { id: (app.vendor_id ?? "").slice(-6) });
}

/** Where a line's orders came from: the app by its name, or the channel (ADR-0259). */
export function channelName(line: ChannelLine): string {
  return line.vendor_id ? appName(line) : t(SOURCES[line.source] ?? "x.source_other");
}

/** How a line's orders were served. */
export function orderTypeName(orderType: string): string {
  return t(ORDER_TYPES[orderType] ?? "x.type_unspecified");
}

/** The edge's lines summed by `label`, in the order each label first appears. */
export function groupChannels(
  lines: readonly ChannelLine[],
  label: (line: ChannelLine) => string,
): readonly (readonly [string, number, Money])[] {
  const rows = new Map<string, [number, Money]>();
  for (const line of lines) {
    const name = label(line);
    const row = rows.get(name);
    rows.set(
      name,
      row === undefined
        ? [line.bill_count, line.sales_amount]
        : [row[0] + line.bill_count, { ...row[1], amount_minor: row[1].amount_minor + line.sales_amount.amount_minor }],
    );
  }
  return [...rows].map(([name, [bills, sales]]) => [name, bills, sales] as const);
}

/**
 * The X's tenders as the till lists them: cash, card, a wallet, each delivery app's in-app payments
 * by its name ("Trả qua GrabFood"), and every other tender, which the edge sends without them.
 */
export function tenderRows(figures: XFigures): readonly (readonly [string, Money])[] {
  const tenders = figures.tenders;
  return [
    [t("x.cash"), tenders.cash],
    [t("x.card"), tenders.card],
    [t("x.qr"), tenders.qr],
    ...tenders.in_app.map((app) => [t("x.paid_via", { app: appName(app) }), app.amount] as const),
    [t("x.other"), tenders.other_than_apps],
  ];
}
