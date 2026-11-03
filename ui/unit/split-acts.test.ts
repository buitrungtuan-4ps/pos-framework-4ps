// `state/split-acts.ts` (ADR-0196, ADR-0204): what the till refuses about a split before sending
// it, and how a re-read of the table's bills says what came of a split or a merge with no answer.

import { beforeEach, describe, expect, it, vi } from "vitest";

const bills = new Map<string, { state: string; order_line_ids: string[] }>();
let open: string[] = [];

vi.mock("../src/api/client", async (actual) => ({
  ...(await actual<typeof import("../src/api/client")>()),
  api: {
    billCheck: async (id: string) => bills.get(id) ?? { state: "BILL_STATE_OPEN", order_line_ids: [] },
    liveOrders: async () => [{ order_id: "order-1", table_id: "table-1", open_bill_ids: open, lines: [] }],
  },
}));

const { mergeRefusal, recheckMerge, recheckSplit, splitRefusal } = await import("../src/state/split-acts");

const WHERE = { orderId: "order-1", tableId: "table-1" };

beforeEach(() => {
  bills.clear();
  open = [];
});

describe("a split, before it is sent", () => {
  const covered = ["l1", "l2", "l3"];
  it("goes when its parts are a partition of the bill's lines", () => {
    expect(splitRefusal(covered, [["l1"], ["l2", "l3"]])).toBeNull();
  });
  it("is refused when the parts are not one", () => {
    for (const parts of [[["l1", "l2", "l3"]], [["l1"], []], [["l1"], ["l1", "l2", "l3"]], [["l1"], ["l2"]], [["l1"], ["l2", "x"]]]) {
      expect(splitRefusal(covered, parts), JSON.stringify(parts)).not.toBeNull();
    }
  });
});

describe("a merge, before it is sent", () => {
  it("goes for bills this device cannot place on another order", () => {
    expect(mergeRefusal("bill-a", ["bill-b"])).toBeNull();
  });
});

describe("a split with no answer", () => {
  it("is committed once the source is split, naming the parts in the order given", async () => {
    bills.set("src", { state: "BILL_STATE_SPLIT", order_line_ids: ["l1", "l2", "l3"] });
    bills.set("p2", { state: "BILL_STATE_OPEN", order_line_ids: ["l3", "l2"] });
    bills.set("p1", { state: "BILL_STATE_OPEN", order_line_ids: ["l1"] });
    bills.set("other", { state: "BILL_STATE_OPEN", order_line_ids: ["l9"] });
    open = ["other", "p2", "p1"];
    expect(await recheckSplit("src", [["l1"], ["l2", "l3"]], WHERE)).toEqual({ committed: ["p1", "p2"] });
  });
  it("is rolled back when the source is still open, and not called made when it ended another way", async () => {
    bills.set("src", { state: "BILL_STATE_OPEN", order_line_ids: ["l1", "l2"] });
    expect(await recheckSplit("src", [["l1"], ["l2"]], WHERE)).toHaveProperty("refused");
    bills.set("src", { state: "BILL_STATE_SETTLED", order_line_ids: ["l1", "l2"] });
    expect(await recheckSplit("src", [["l1"], ["l2"]], WHERE)).toHaveProperty("refused");
  });
});

describe("a merge with no answer", () => {
  it("is committed only once every absorbed bill is merged", async () => {
    bills.set("b", { state: "BILL_STATE_MERGED", order_line_ids: [] });
    bills.set("c", { state: "BILL_STATE_MERGED", order_line_ids: [] });
    expect(await recheckMerge("a", ["b", "c"])).toEqual({ committed: true });
    bills.set("c", { state: "BILL_STATE_SETTLED", order_line_ids: [] });
    expect(await recheckMerge("a", ["b", "c"])).toHaveProperty("refused");
  });
});
