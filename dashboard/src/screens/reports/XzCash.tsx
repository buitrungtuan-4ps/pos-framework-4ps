// The X/Z report's drawer figures and its tender split (ADR-0187, UX-6). Only a close says what a
// drawer should hold, so while none of the day's shifts has closed the expected, counted and
// variance read as a dash rather than a zero nobody counted; and each tender's captured amount
// shows under them.

import { For, Show } from "solid-js";

import { t, type MessageKey } from "../../i18n";
import { formatAmount } from "../../state/money";
import type { XzReport } from "../../api/schemas";

/** The tenders the X splits by, in the order a cash-up reads them. */
const TENDERS: readonly (readonly [string, MessageKey])[] = [
  ["PAYMENT_METHOD_CASH", "reports.tenderCash"],
  ["PAYMENT_METHOD_CARD", "reports.tenderCard"],
  ["PAYMENT_METHOD_QR", "reports.tenderQr"],
];

/** The day's captured amounts in four kinds: cash, card, a wallet, and every other tender. */
export function tenderSplit(report: XzReport): readonly (readonly [MessageKey, number])[] {
  const byTender = report.revenue.by_tender ?? {};
  const named = TENDERS.map(([token, key]) => [key, byTender[token] ?? 0] as const);
  const other = Object.entries(byTender)
    .filter(([token]) => !TENDERS.some(([known]) => known === token))
    .reduce((sum, [, amount]) => sum + amount, 0);
  return [...named, ["reports.tenderOther", other] as const];
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
        <For each={tenderSplit(props.report)}>
          {([key, amount]) => (
            <div>
              <dt class="text-ink-muted">{t(key)}</dt>
              <dd class="text-ink">{tender(amount)}</dd>
            </div>
          )}
        </For>
      </Show>
    </>
  );
}
