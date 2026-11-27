// The X/Z report's sales by channel and order type (ADR-0259 decision 4): each line the day's
// figures carry, "Grab · Delivery", then the same lines grouped by channel and by order type. A
// delivery app is named by its connection, from the report's `vendor_names`; a type or source the
// store named by `order_type_names` and `source_names` (ADR-0260); the rest by a key.

import { For, Show } from "solid-js";

import type { ChannelSales, XzReport } from "../../api/schemas";
import { locale, t, type MessageKey } from "../../i18n";
import { formatAmount } from "../../state/money";

const SOURCES: Readonly<Record<string, MessageKey>> = {
  SALE_SOURCE_POS: "reports.sourcePos",
  SALE_SOURCE_QR: "reports.sourceQr",
  SALE_SOURCE_KIOSK: "reports.sourceKiosk",
  SALE_SOURCE_API: "reports.sourceApi",
};

const ORDER_TYPES: Readonly<Record<string, MessageKey>> = {
  ORDER_TYPE_DINE_IN: "reports.orderTypeDineIn",
  ORDER_TYPE_TAKEAWAY: "reports.orderTypeTakeaway",
  ORDER_TYPE_DELIVERY: "reports.orderTypeDelivery",
};

/** A delivery app's name as the store's connection gives it, or its id's tail when it lists none. */
export function appName(report: XzReport, vendorId: string): string {
  return report.vendor_names?.[vendorId] || t("reports.sourceAppUnnamed", { id: vendorId.slice(-6) });
}

/** A name the store gave, in the console's language, else English (ADR-0260). */
function storeName(names: Readonly<Record<string, string>> | undefined): string | undefined {
  return names === undefined ? undefined : (names[locale()] ?? names.en);
}

/** Where a line's orders came from: the app's name, the store's name for the source, or ours. */
export function channelName(report: XzReport, line: ChannelSales): string {
  if (line.vendor_id) return appName(report, line.vendor_id);
  return storeName(report.source_names?.[line.source]) ?? t(SOURCES[line.source] ?? "reports.sourceOther");
}

/** How a line's orders were served: the store's name for the type, or ours. */
export function orderTypeName(orderType: string, report?: XzReport): string {
  return storeName(report?.order_type_names?.[orderType]) ?? t(ORDER_TYPES[orderType] ?? "reports.orderTypeUnspecified");
}

/** One row: what it is called, its bills, its sales. */
export type ChannelRow = readonly [string, number, number];

/** The lines summed by `label`, in the order each label first appears. */
export function groupLines(lines: readonly ChannelSales[], label: (line: ChannelSales) => string): ChannelRow[] {
  const rows = new Map<string, [number, number]>();
  for (const line of lines) {
    const name = label(line);
    const row = rows.get(name) ?? [0, 0];
    rows.set(name, [row[0] + line.bills, row[1] + line.sales]);
  }
  return [...rows].map(([name, [bills, sales]]) => [name, bills, sales] as const);
}

function Rows(props: { heading: MessageKey; rows: readonly ChannelRow[]; currency: string; outcome: string }) {
  return (
    <table class="mt-3 w-full text-sm" data-outcome={props.outcome}>
      <thead>
        <tr class="text-left text-ink-muted">
          <th>{t(props.heading)}</th>
          <th class="text-right">{t("reports.bills")}</th>
          <th class="text-right">{t("reports.net")}</th>
        </tr>
      </thead>
      <tbody>
        <For each={props.rows}>
          {([name, bills, sales]) => (
            <tr>
              <td>{name}</td>
              <td class="text-right tabular-nums">{bills}</td>
              <td class="text-right tabular-nums">
                {formatAmount({
                  amount_minor: sales,
                  currency_code: props.currency,
                })}
              </td>
            </tr>
          )}
        </For>
      </tbody>
    </table>
  );
}

export function XzChannels(props: { report: XzReport }) {
  const lines = () => props.report.revenue.by_channel ?? [];
  const currency = () => props.report.revenue.currency_code || "";
  const both = (line: ChannelSales) =>
    `${channelName(props.report, line)} · ${orderTypeName(line.order_type, props.report)}`;
  return (
    <Show when={lines().length > 0}>
      <Rows
        heading="reports.channelAndType"
        rows={groupLines(lines(), both)}
        currency={currency()}
        outcome="xz-channel-lines"
      />
      <Rows
        heading="reports.byChannel"
        rows={groupLines(lines(), (line) => channelName(props.report, line))}
        currency={currency()}
        outcome="xz-by-channel"
      />
      <Rows
        heading="reports.byOrderType"
        rows={groupLines(lines(), (line) => orderTypeName(line.order_type, props.report))}
        currency={currency()}
        outcome="xz-by-order-type"
      />
    </Show>
  );
}
