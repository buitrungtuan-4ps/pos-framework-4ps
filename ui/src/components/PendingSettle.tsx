// A money act on its way to the edge (ADR-0196 decision 1, ADR-0204), a settle by default and
// another act with `act`: the amount the edge confirmed, and what the till is waiting for. Never the change, the receipt or a second way to pay: the
// drawer opens and the receipt prints only once the edge commits (safeguard 1). Once the store server
// cannot be reached, a manager may leave it to be looked up later; the bill then offers no settle on
// this till until the edge says what came of it.

import { Show } from "solid-js";

import { t, type MessageKey } from "../i18n";
import { holdsOwn } from "../state/permissions";
import type { MoneyPhase } from "../state/money-act";

/** What each act says while it is out, and once the store server cannot be reached. */
export type PendingActKind = "settle" | "void" | "discount" | "waive" | "split" | "merge" | "open" | "count" | "close" | "paid_in" | "paid_out" | "safe_drop";

const ACTS: Readonly<Record<PendingActKind, { doing: MessageKey; unreachable: MessageKey; outcome: string }>> = {
  settle: { doing: "pay.paying", unreachable: "pay.unreachable", outcome: "settle-pending" },
  void: { doing: "pay.voiding", unreachable: "pay.void_unreachable", outcome: "void-pending" },
  discount: { doing: "pay.discounting", unreachable: "pay.reduction_unreachable", outcome: "discount-pending" },
  waive: { doing: "pay.waiving", unreachable: "pay.reduction_unreachable", outcome: "waive-pending" },
  split: { doing: "pay.splitting", unreachable: "pay.reduction_unreachable", outcome: "split-pending" },
  merge: { doing: "pay.merging", unreachable: "pay.reduction_unreachable", outcome: "merge-pending" },
  open: { doing: "shift.opening", unreachable: "shift.unreachable", outcome: "open-pending" },
  count: { doing: "shift.counting", unreachable: "shift.unreachable", outcome: "count-pending" },
  close: { doing: "shift.closing", unreachable: "shift.unreachable", outcome: "close-pending" },
  paid_in: { doing: "shift.paying_in", unreachable: "shift.unreachable", outcome: "paid-in-pending" },
  paid_out: { doing: "shift.paying_out", unreachable: "shift.unreachable", outcome: "paid-out-pending" },
  safe_drop: { doing: "shift.dropping", unreachable: "shift.unreachable", outcome: "safe-drop-pending" },
};

export function PendingSettle(props: {
  readonly phase: MoneyPhase;
  readonly amount: string;
  readonly onLeave: () => void;
  readonly act?: PendingActKind;
}) {
  const act = () => ACTS[props.act ?? "settle"];
  return (
    <div class="mt-6 rounded-token border border-line bg-surface p-4" role="status" data-outcome={act().outcome}>
      <Show when={props.amount !== ""}>
        <p class="text-sm text-ink-muted">{t("pay.amount_due")}</p>
        <p class="text-2xl font-semibold tabular-nums">{props.amount}</p>
      </Show>
      <p class="mt-3 font-semibold">
        {t(props.phase === "sending" ? act().doing : props.phase === "checking" ? "pay.checking" : act().unreachable)}
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
