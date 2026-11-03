// Splitting a bill and putting it back together as money acts (ADR-0196, ADR-0204). Each is refused
// at once where the till can tell, pending until the edge answers, and, with no answer, checked by
// reading the table's bills again: a split by whether its source is split and which open bills now
// cover each part, a merge by whether every absorbed bill is merged.

import type { Accessor } from "solid-js";

import { api } from "../api/client";
import { t } from "../i18n";
import { reasonMessage } from "../lib/errors";
import { PRECHECKED, WRITES_PENDING } from "../package/prechecks";
import { type MoneyAct, type MoneyPhase, type Recheck, useMoneyAct } from "./money-act";
import { writesPending } from "./optimistic";
import { applyMerged, applySplit, orderOfBill } from "./store";

const SPLIT = PRECHECKED["billing.bill.split"] ?? [];
const MERGE = PRECHECKED["billing.bill.merge"] ?? [];

function waitForWrites(checked: readonly string[]): string | null {
  return checked.includes(WRITES_PENDING) && writesPending() ? t("pay.wait_for_writes") : null;
}

/**
 * What the till can tell about a split before sending it: a write still on its way, or parts that
 * are not a partition of the lines the bill covers (fewer than two, one empty, a line twice, or a
 * line missing or not the bill's).
 */
export function splitRefusal(covered: readonly string[], parts: readonly (readonly string[])[]): string | null {
  const pending = waitForWrites(SPLIT);
  if (pending !== null) {
    return pending;
  }
  const lines = parts.flat();
  const partition =
    parts.length >= 2 &&
    parts.every((part) => part.length > 0) &&
    new Set(lines).size === lines.length &&
    lines.length === covered.length &&
    lines.every((line) => covered.includes(line));
  return !partition && SPLIT.includes("SPLIT_NOT_A_PARTITION") ? reasonMessage("SPLIT_NOT_A_PARTITION") : null;
}

/** What the till can tell about a merge before sending it: a write on its way, or another order's bill. */
export function mergeRefusal(billId: string, absorbed: readonly string[]): string | null {
  const pending = waitForWrites(MERGE);
  if (pending !== null) {
    return pending;
  }
  const order = orderOfBill(billId);
  const elsewhere = absorbed.some((id) => {
    const theirs = orderOfBill(id);
    return order !== undefined && theirs !== undefined && theirs !== order;
  });
  return elsewhere && MERGE.includes("BILLS_ON_DIFFERENT_ORDERS") ? reasonMessage("BILLS_ON_DIFFERENT_ORDERS") : null;
}

/** The bills open now on the order, or on the table when this device does not know the order. */
async function openBills(orderId: string | undefined, tableId: string): Promise<readonly string[]> {
  const live = await api.liveOrders();
  const order = live.find((each) => (orderId === undefined ? each.table_id === tableId : each.order_id === orderId));
  return order?.open_bill_ids ?? [];
}

/**
 * What the edge says now about a split that went unanswered. Committed once the source is split:
 * the parts are the open bills that cover exactly each part's lines, in the order the parts were
 * given, leaving out any already paid on another till.
 */
export async function recheckSplit(
  source: string,
  parts: readonly (readonly string[])[],
  where: { readonly orderId: string | undefined; readonly tableId: string },
): Promise<Recheck<string[]>> {
  const bill = await api.billCheck(source);
  if (bill.state === "BILL_STATE_OPEN") {
    return { refused: t("pay.split_not_made") };
  }
  if (bill.state !== "BILL_STATE_SPLIT") {
    return { refused: t("pay.ended_unconfirmed") };
  }
  const open = await openBills(where.orderId, where.tableId);
  const covering = await Promise.all(open.map(async (id) => [id, (await api.billCheck(id)).order_line_ids] as const));
  const same = (a: readonly string[], b: readonly string[]) => a.length === b.length && a.every((line) => b.includes(line));
  const partIds = parts.flatMap((part) => covering.filter(([, lines]) => same(lines, part)).map(([id]) => id));
  applySplit(source, partIds);
  return { committed: partIds };
}

/** What the edge says now about a merge that went unanswered: committed once every absorbed bill is merged. */
export async function recheckMerge(survivor: string, absorbed: readonly string[]): Promise<Recheck<true>> {
  const states = await Promise.all(absorbed.map(async (id) => (await api.billCheck(id)).state));
  if (states.every((state) => state === "BILL_STATE_MERGED")) {
    applyMerged(absorbed);
    return { committed: true };
  }
  const kept = await api.billCheck(survivor);
  return kept.state === "BILL_STATE_OPEN" ? { refused: t("pay.merge_not_made") } : { refused: t("pay.ended_unconfirmed") };
}

/** Sends a split, and answers with the parts' ids in the order they were given. */
async function sendSplit(billId: string, parts: string[][]): Promise<string[]> {
  const { bill_ids: partIds } = await api.splitBill(billId, { parts });
  applySplit(billId, partIds);
  return partIds;
}

/** Sends a merge of `absorbed` into `billId`. */
async function sendMerge(billId: string, absorbed: string[]): Promise<true> {
  await api.mergeBills(billId, { absorbed_bill_ids: absorbed });
  applyMerged(absorbed);
  return true;
}

/** The pay screen's split and merge, as one hook: which is out, and the refusal to dismiss. */
export function useSplitActs(options: {
  readonly billId: Accessor<string | null>;
  readonly tableId: Accessor<string>;
  readonly onSplit: (partIds: string[]) => void;
  readonly onMerged: () => void;
}) {
  // What the act out was sent with, which its re-read compares the edge's bills against.
  const sent: { parts: string[][]; absorbed: string[]; orderId?: string } = { parts: [], absorbed: [] };
  const splitting: MoneyAct<string[]> = useMoneyAct<string[]>({
    act: "split",
    recheck: (source) => recheckSplit(source, sent.parts, { orderId: sent.orderId, tableId: options.tableId() }),
    committed: (partIds) => options.onSplit(partIds),
  });
  const merging: MoneyAct<true> = useMoneyAct<true>({
    act: "merge",
    recheck: (survivor) => recheckMerge(survivor, sent.absorbed),
    committed: () => options.onMerged(),
  });
  const both: readonly Pick<MoneyAct<never>, "dismiss" | "resume">[] = [splitting, merging];
  const pending = (): { phase: MoneyPhase; act: "split" | "merge" } | null =>
    splitting.phase() !== "idle"
      ? { phase: splitting.phase(), act: "split" }
      : merging.phase() !== "idle"
        ? { phase: merging.phase(), act: "merge" }
        : null;
  return {
    pending,
    refusal: () => splitting.refusal() ?? merging.refusal(),
    dismiss: () => both.forEach((act) => act.dismiss()),
    resume: () => {
      const id = options.billId();
      both.forEach((act) => id !== null && act.resume(id));
    },
    /** Splits the bill on screen into `into`, a partition of `covered`, the lines it covers. */
    split: (covered: readonly string[], into: string[][]) => {
      const id = options.billId();
      if (id !== null) {
        sent.parts = into;
        sent.orderId = orderOfBill(id);
        void splitting.run(id, () => splitRefusal(covered, into), () => sendSplit(id, into));
      }
    },
    /** Folds `others` into the bill on screen. */
    merge: (others: string[]) => {
      const id = options.billId();
      if (id !== null) {
        sent.absorbed = others;
        void merging.run(id, () => mergeRefusal(id, others), () => sendMerge(id, others));
      }
    },
  };
}
