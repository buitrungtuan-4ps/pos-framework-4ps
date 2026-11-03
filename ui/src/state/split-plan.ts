// An even split in progress, kept per bill for the length of the browser tab (UX-5).
//
// The edge settles a bill in one step, so the shares a split has taken live on the till until the
// last one lands (`screens/Pay.tsx`). They lived only in the screen, so leaving it or reloading
// forgot them: the bill came back owing its whole amount, and in the hands-on run a reopened bill
// was taken whole in cash after one guest had already paid their half. Now the plan — how many
// shares, and the payments taken — is kept in the tab's session storage under the bill's id and
// put back when that bill is on screen again, and leaving the screen with a share taken asks first.
//
// Bounded: the most recent `KEPT` bills only, and a bill's plan goes when it settles, is voided or
// the split is cancelled. No person is named: a plan is amounts and tender kinds. Where storage is
// blocked the split works as before, held on the screen alone.

import { createEffect, createSignal, type Accessor } from "solid-js";
import { useBeforeLeave } from "@solidjs/router";

import type { PaymentRequest } from "../api/schemas";
import { t } from "../i18n";

const PREFIX = "pay.split.";
const INDEX = "pay.split.index";
const KEPT = 20;

interface Plan {
  ways: number;
  taken: PaymentRequest[];
}

function storage(): Storage | null {
  try {
    return globalThis.sessionStorage ?? null;
  } catch {
    return null;
  }
}

/** The plan kept for `billId`, or null. */
export function readPlan(billId: string): Plan | null {
  try {
    const raw = storage()?.getItem(PREFIX + billId);
    const plan = raw == null ? null : (JSON.parse(raw) as Plan);
    return plan !== null && typeof plan.ways === "number" && Array.isArray(plan.taken) ? plan : null;
  } catch {
    return null;
  }
}

/** Keeps `plan` for `billId`, or forgets it on null; only the last `KEPT` bills are kept. */
export function writePlan(billId: string, plan: Plan | null): void {
  const store = storage();
  if (store === null) {
    return;
  }
  try {
    const index = (JSON.parse(store.getItem(INDEX) ?? "[]") as string[]).filter((id) => id !== billId);
    if (plan === null) {
      store.removeItem(PREFIX + billId);
    } else {
      store.setItem(PREFIX + billId, JSON.stringify(plan));
      index.push(billId);
    }
    for (const old of index.splice(0, Math.max(0, index.length - KEPT))) {
      store.removeItem(PREFIX + old);
    }
    store.setItem(INDEX, JSON.stringify(index));
  } catch {
    // Storage full or blocked: the split is held on the screen alone, as it was.
  }
}

/**
 * The pay screen's even split for the bill on screen: how many shares (`ways`, null for none) and
 * the payments taken. Kept for the bill, put back when that bill is shown again, forgotten once it
 * is closed, and leaving the screen with a share taken asks first.
 */
export function useSplitPlan(billId: Accessor<string | null>, closed: Accessor<boolean>) {
  const [ways, setWays] = createSignal<number | null>(null);
  const [taken, setTaken] = createSignal<PaymentRequest[]>([]);
  let shown: string | null = null;
  createEffect(() => {
    const id = billId();
    const n = ways();
    const paid = taken();
    if (id === null) {
      return;
    }
    if (id !== shown) {
      shown = id;
      const kept = readPlan(id);
      if (kept !== null) {
        setWays(kept.ways);
        setTaken(kept.taken);
      }
      return;
    }
    writePlan(id, closed() || n === null ? null : { ways: n, taken: paid });
  });
  useBeforeLeave((leave) => {
    if (!leave.defaultPrevented && !closed() && taken().length > 0) {
      if (!globalThis.confirm(t("pay.split_leave_confirm"))) {
        leave.preventDefault();
      }
    }
  });
  return { ways, setWays, taken, setTaken };
}
