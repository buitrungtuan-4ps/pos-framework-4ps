// A closed shift's card on the Shift screen: what moved besides its sales, its count note by note,
// what was expected and counted, the variance in colour and words, and what came of its report.
// Out of `screens/Shift.tsx` by the size rule; the figures are the edge's close, never worked out.

import { Show } from "solid-js";

import { t } from "../i18n";
import { money, type Money } from "../lib/money";
import { printOutcomeKey } from "../lib/print";
import { formatAmount, storeCurrency, type ShiftInfo } from "../state/store";
import { CountedRows } from "./CountField";
import { ShiftCashLines } from "./ShiftCashLines";

export function ClosedCard(props: { shift: ShiftInfo; variance: Money }) {
  const amount = (figure: Money | undefined) => formatAmount(figure ?? money(storeCurrency(), 0));
  return (
    <div class="mt-4 rounded-token border border-line bg-surface p-4">
      <p class="font-semibold">{t("shift.closed")}</p>
      <ShiftCashLines
        paidIn={props.shift.paidIn}
        paidOut={props.shift.paidOut}
        dropped={props.shift.dropped}
        voided={props.shift.voided}
      />
      <CountedRows counting={props.shift.counting} />
      <p class="mt-2 text-ink-muted">
        {t("shift.expected")} <span class="tabular-nums">{amount(props.shift.expected)}</span>
      </p>
      <p class="text-ink-muted">
        {t("shift.counted")} <span class="tabular-nums">{amount(props.shift.counted)}</span>
      </p>
      <p
        classList={{
          "text-danger": props.variance.amount_minor !== 0,
          "text-ok": props.variance.amount_minor === 0,
        }}
      >
        {t("shift.variance")} <span class="tabular-nums">{formatAmount(props.variance)}</span>
      </p>
      <Show when={props.shift.reportPrint}>
        {(outcome) => (
          <p class="mt-2 text-sm text-ink-muted" role="status" data-outcome="shift-report-print">
            {t(printOutcomeKey(outcome(), "shift.report_printed"))}
          </p>
        )}
      </Show>
    </div>
  );
}
