// Giving a line away and changing a line's price as money acts (ADR-0219, ADR-0196, ADR-0204): a comp
// of a whole line on an open bill, and a new price on a line before its bill opens. Each needs a
// reason the store publishes for it and, where the person may not act alone, an approver. Each is
// refused at once where the till can tell, pending until the edge answers, and, with no answer,
// checked by reading again: a comp by whether the bill's comp grew, a price by the line's total.

import { api } from "../api/client";
import type { ApproverRequest, CheckResponse, RepriceResponse } from "../api/schemas";
import { t } from "../i18n";
import { reasonMessage } from "../lib/errors";
import type { Money } from "../lib/money";
import { PRECHECKED, WRITES_PENDING } from "../package/prechecks";
import { writesPending } from "./edge-write";
import { type MoneyAct, type Recheck, useMoneyAct } from "./money-act";
import { applyRepriced, state } from "./store";

/** The first refusal the till can decide, from `checks` that `act` declares it checks, or null. */
function refusalOf(act: "billing.line.comp" | "sales.line.reprice", checks: readonly (readonly [string, boolean])[]) {
  const checked = PRECHECKED[act] ?? [];
  const found = checks.find(([token, refused]) => refused && checked.includes(token));
  if (found === undefined) {
    return null;
  }
  return found[0] === WRITES_PENDING ? t("pay.wait_for_writes") : reasonMessage(found[0]);
}

/** What the till can tell about a comp before sending it. */
export function compRefusal(asks: boolean, approval: ApproverRequest | undefined): string | null {
  return refusalOf("billing.line.comp", [
    [WRITES_PENDING, writesPending()],
    ["APPROVAL_REQUIRED", asks && approval === undefined],
  ]);
}

/** What the till can tell about a new price before sending it. */
export function repriceRefusal(asks: boolean, approval: ApproverRequest | undefined, billOpen: boolean): string | null {
  return refusalOf("sales.line.reprice", [
    [WRITES_PENDING, writesPending()],
    ["APPROVAL_REQUIRED", asks && approval === undefined],
    ["BILL_ALREADY_OPEN", billOpen],
  ]);
}

export interface CompAct extends MoneyAct<CheckResponse> {
  readonly send: (billId: string, orderLineId: string, reasonCodeId: string, approval: ApproverRequest | undefined, asks: boolean) => void;
}

/** A screen's comp as a money act; `onComped` takes the bill as the edge now has it. */
export function useCompAct(onComped: (totals: CheckResponse) => void): CompAct {
  let before: number | null = null;
  const recheck = async (billId: string): Promise<Recheck<CheckResponse>> => {
    const bill = await api.billCheck(billId);
    if (bill.state !== "BILL_STATE_OPEN") {
      return { refused: t("pay.ended_unconfirmed") };
    }
    return before !== null && bill.comp_total.amount_minor > before ? { committed: bill } : { refused: t("pay.comp_not_made") };
  };
  const act = useMoneyAct<CheckResponse>({ act: "comp", recheck, committed: onComped });
  const send: CompAct["send"] = (billId, orderLineId, reasonCodeId, approval, asks) => {
    before = null;
    void act.run(
      billId,
      () => compRefusal(asks, approval),
      async () => {
        before = (await api.billCheck(billId)).comp_total.amount_minor;
        return api.compLine(billId, { order_line_id: orderLineId, reason_code_id: reasonCodeId, ...approval });
      },
    );
  };
  return { ...act, send };
}

/** One new price, as the reprice panel sends it. */
export interface Reprice {
  readonly orderId: string;
  readonly orderLineId: string;
  readonly unitPrice: Money;
  readonly reasonCodeId: string;
  readonly approval: ApproverRequest | undefined;
  readonly asks: boolean;
  readonly billOpen: boolean;
}

export interface RepriceAct extends MoneyAct<RepriceResponse> {
  readonly send: (reprice: Reprice) => void;
}

/** A screen's price override as a money act; `onRepriced` gets the line's new figures. */
export function useRepriceAct(onRepriced: (answer: RepriceResponse) => void): RepriceAct {
  let sent: Reprice | null = null;
  // The line's total when the request went: one that changed since is the price taken.
  let before: number | null = null;
  const recheck = async (orderLineId: string): Promise<Recheck<RepriceResponse>> => {
    const order = (await api.liveOrders()).find((live) => live.order_id === sent?.orderId);
    const line = order?.lines.find((entry) => entry.order_line_id === orderLineId);
    if (sent === null || line === undefined || line.line_total.amount_minor === before) {
      return { refused: t("order.reprice_not_made") };
    }
    const unit = sent.unitPrice;
    return {
      committed: {
        order_id: sent.orderId,
        order_line_id: orderLineId,
        unit_price: unit,
        previous_unit_price: unit,
        line_total: line.line_total,
      },
    };
  };
  const act = useMoneyAct<RepriceResponse>({
    act: "reprice",
    recheck,
    committed: (answer) => {
      applyRepriced(answer.order_line_id, answer.line_total);
      onRepriced(answer);
    },
  });
  const send = (reprice: Reprice) => {
    sent = reprice;
    before = state.lines[reprice.orderLineId]?.lineTotal.amount_minor ?? null;
    void act.run(
      reprice.orderLineId,
      () => repriceRefusal(reprice.asks, reprice.approval, reprice.billOpen),
      () =>
        api.repriceLine(reprice.orderLineId, {
          unit_price: reprice.unitPrice,
          reason_code_id: reprice.reasonCodeId,
          ...reprice.approval,
        }),
    );
  };
  return { ...act, send };
}
