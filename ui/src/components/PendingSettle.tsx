// A settle on its way to the edge (ADR-0196 decision 1, ADR-0204): the amount the edge confirmed,
// and what the till is waiting for. Never the change, the receipt or a second way to pay: the
// drawer opens and the receipt prints only once the edge commits (safeguard 1). Once the store server
// cannot be reached, a manager may leave it to be looked up later; the bill then offers no settle on
// this till until the edge says what came of it.

import { Show } from "solid-js";

import { t } from "../i18n";
import { holdsOwn } from "../state/permissions";
import type { MoneyPhase } from "../state/money-act";

export function PendingSettle(props: {
  readonly phase: MoneyPhase;
  readonly amount: string;
  readonly onLeave: () => void;
}) {
  return (
    <div class="mt-6 rounded-token border border-line bg-surface p-4" role="status" data-outcome="settle-pending">
      <p class="text-sm text-ink-muted">{t("pay.amount_due")}</p>
      <p class="text-2xl font-semibold tabular-nums">{props.amount}</p>
      <p class="mt-3 font-semibold">
        {t(props.phase === "sending" ? "pay.paying" : props.phase === "checking" ? "pay.checking" : "pay.unreachable")}
      </p>
      <Show when={props.phase === "unreachable" && holdsOwn("billing.bill.void")}>
        <button
          type="button"
          class="mt-4 min-h-touch w-full rounded-token border border-line"
          data-step="lookUpLater"
          onClick={() => props.onLeave()}
        >
          {t("pay.look_up_later")}
        </button>
      </Show>
    </div>
  );
}
