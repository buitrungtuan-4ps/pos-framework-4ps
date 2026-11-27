// The X/Z report's drawer figures and its tender split (ADR-0187, UX-6). Only a close says what a
// drawer should hold, so while none of the day's shifts has closed the expected, counted and
// variance read as a dash rather than a zero nobody counted; and each tender's captured amount
// shows under them.

import { For, Show } from "solid-js";

import { t, type MessageKey } from "../../i18n";
import { formatAmount } from "../../state/money";
import type { XzReport } from "../../api/schemas";
import { appName } from "./XzChannels";

/** The tenders the X splits by, in the order a cash-up reads them. */
const TENDERS: readonly (readonly [string, MessageKey])[] = [
  ["PAYMENT_METHOD_CASH", "reports.tenderCash"],
  ["PAYMENT_METHOD_CARD", "reports.tenderCard"],
  ["PAYMENT_METHOD_QR", "reports.tenderQr"],
  // Paid in a delivery vendor's app and settled on accept (ADR-0248 decision 2): the vendor pays it
  // out, so a cash-up reads it apart from the tenders the store took.
  ["PAYMENT_METHOD_VENDOR", "reports.tenderVendor"],
];

/** A tender row: its message key, the amount, and the app's name for "Paid via {app}". */
export type TenderRow = readonly [MessageKey, number] | readonly [MessageKey, number, { app: string }];

/**
 * The day's captured amounts: cash, card, a wallet, each delivery app's in-app payments by its name
 * (ADR-0259 decision 5), and every other tender. A day folded before apps were named reads its
 * in-app payments as one "Paid via delivery apps" row, and any part of them no app accounts for
 * stays on that row.
 */
export function tenderSplit(
  report: XzReport,
  nameOf: (vendorId: string) => string = (id) => id,
): readonly TenderRow[] {
  const byTender = report.revenue.by_tender ?? {};
  const byApp = Object.entries(report.revenue.by_app_tender ?? {})
    .filter(([, amount]) => amount !== 0)
    .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
  const rows: TenderRow[] = [];
  for (const [token, key] of TENDERS) {
    const amount = byTender[token] ?? 0;
    if (token !== "PAYMENT_METHOD_VENDOR" || byApp.length === 0) {
      rows.push([key, amount]);
      continue;
    }
    for (const [vendorId, paid] of byApp) rows.push(["reports.tenderPaidVia", paid, { app: nameOf(vendorId) }]);
    const unnamed = amount - byApp.reduce((sum, [, paid]) => sum + paid, 0);
    if (unnamed !== 0) rows.push([key, unnamed]);
  }
  const other = Object.entries(byTender)
    .filter(([token]) => !TENDERS.some(([known]) => known === token))
    .reduce((sum, [, amount]) => sum + amount, 0);
  return [...rows, ["reports.tenderOther", other]];
}

export function XzCash(props: { report: XzReport }) {
  const cash = (minor: number) =>
    formatAmount({ amount_minor: minor, currency_code: props.report.cash.currency_code || "" });
  const closed = (minor: number) => (props.report.cash.shifts_closed > 0 ? cash(minor) : "—");
  const tender = (minor: number) =>
    formatAmount({ amount_minor: minor, currency_code: props.report.revenue.currency_code || "" });
  return (
    <>
      <div>
        <dt class="text-ink-muted">{t("reports.xzExpected")}</dt>
        <dd class="text-ink">{closed(props.report.cash.expected)}</dd>
      </div>
      <div>
        <dt class="text-ink-muted">{t("reports.xzCounted")}</dt>
        <dd class="text-ink">{closed(props.report.cash.counted)}</dd>
      </div>
      <div>
        <dt class="text-ink-muted">{t("reports.xzVariance")}</dt>
        <dd class="font-medium text-ink">{closed(props.report.cash.variance)}</dd>
      </div>
      <Show when={Object.keys(props.report.revenue.by_tender ?? {}).length > 0}>
        <For each={tenderSplit(props.report, (vendorId) => appName(props.report, vendorId))}>
          {([key, amount, args]) => (
            <div>
              <dt class="text-ink-muted">{t(key, args)}</dt>
              <dd class="text-ink">{tender(amount)}</dd>
            </div>
          )}
        </For>
      </Show>
    </>
  );
}
