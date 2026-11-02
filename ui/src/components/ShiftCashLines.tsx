import { Show } from "solid-js";

import { t } from "../i18n";
import type { Money } from "../lib/money";
import { formatAmount } from "../state/store";

// The cash a closed shift moved besides its sales, on the close card: what was paid in and out
// outside a sale (ADR-0165), and the paid bills voided at this till during the shift with the cash
// they handed back (ADR-0184), which the expected amount leaves out. A line with nothing to say is
// not drawn, so a shift that moved none reads as it always did. The edge's figures, never added up
// here.
export function ShiftCashLines(props: {
  paidIn?: Money;
  paidOut?: Money;
  voided?: { bills: number; cash_returned: Money };
}) {
  const nonZero = (amount: Money | undefined) => amount !== undefined && amount.amount_minor !== 0;
  return (
    <>
      <Show when={nonZero(props.paidIn) && props.paidIn}>
        {(paidIn) => (
          <p class="mt-2 text-ink-muted">
            {t("shift.paid_in")} <span class="tabular-nums">{formatAmount(paidIn())}</span>
          </p>
        )}
      </Show>
      <Show when={nonZero(props.paidOut) && props.paidOut}>
        {(paidOut) => (
          <p class="text-ink-muted">
            {t("shift.paid_out")} <span class="tabular-nums">{formatAmount(paidOut())}</span>
          </p>
        )}
      </Show>
      <Show when={props.voided}>
        {(voided) => (
          <p class="text-ink-muted" data-outcome="shift-voided-after-payment">
            {t("shift.voided_after_payment", {
              count: voided().bills,
              amount: formatAmount(voided().cash_returned),
            })}
          </p>
        )}
      </Show>
    </>
  );
}
