// `state/kitchen-writes.ts` (ADR-0196 decision 1): a guest order confirmed or refused leaves the
// queue at once and comes back where it stood when the edge refuses it or does not answer; a bump
// marks its lines made at once and, put back, unmarks only the lines it marked.

import { describe, expect, it, vi } from "vitest";

import { ApiError, api } from "../src/api/client";
import type { WaitingOrder } from "../src/api/schemas";
import { bump, confirmGuestOrder, rejectGuestOrder, takeOff, type WaitingList } from "../src/state/kitchen-writes";
import { state, updateStore } from "../src/state/store";

function queue(ids: readonly string[]): WaitingList & { held: () => string[] } {
  let orders: WaitingOrder[] | null = ids.map((order_id) => ({ order_id }) as WaitingOrder);
  return {
    get: () => orders,
    set: (next) => {
      orders = next;
    },
    held: () => (orders ?? []).map((order) => order.order_id),
  };
}

describe("a guest order answered from the queue", () => {
  it("leaves the queue at once, and its undo puts it back where it stood", () => {
    const list = queue(["a", "b", "c"]);
    const undo = takeOff(list, "b");
    expect(list.held()).toEqual(["a", "c"]);
    undo();
    expect(list.held()).toEqual(["a", "b", "c"]);
  });

  it("is not put back twice when a read brought it back first", () => {
    const list = queue(["a", "b"]);
    const undo = takeOff(list, "b");
    list.set([...(list.get() ?? []), { order_id: "b" } as WaitingOrder]);
    undo();
    expect(list.held()).toEqual(["a", "b"]);
  });

  it("comes back on a refusal, and nothing is read again", async () => {
    const list = queue(["a", "b"]);
    const reread = vi.fn();
    const refusal = new ApiError(409, "already answered", "TRANSITION_REFUSED");
    const send = vi.spyOn(api, "confirmOrder").mockImplementation(() => {
      expect(list.held()).toEqual(["a"]);
      return Promise.reject(refusal);
    });
    await expect(confirmGuestOrder(list, "b", reread)).rejects.toBe(refusal);
    expect(list.held()).toEqual(["a", "b"]);
    expect(reread).not.toHaveBeenCalled();
    expect(send).toHaveBeenCalledTimes(1);
    send.mockRestore();
  });

  it("comes back and is read again when no answer comes, never sent again", async () => {
    const list = queue(["a"]);
    const reread = vi.fn();
    const send = vi.spyOn(api, "rejectOrder").mockRejectedValue(new TypeError("network"));
    await expect(rejectGuestOrder(list, "a", "reason-1", reread)).rejects.toThrow();
    expect(list.held()).toEqual(["a"]);
    expect(reread).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledTimes(1);
    send.mockRestore();
  });
});

describe("a bump", () => {
  it("marks its lines made at once, and its undo unmarks only the ones it marked", async () => {
    updateStore((draft) => {
      draft.bumped = { "line-1": true };
    });
    const refusal = new ApiError(409, "not fired", "TRANSITION_REFUSED");
    const send = vi.spyOn(api, "bumpTicket").mockImplementation(() => {
      expect(state.bumped["line-2"]).toBe(true);
      return Promise.reject(refusal);
    });
    await expect(bump("order-1", ["line-1", "line-2"], "station-1")).rejects.toBe(refusal);
    expect(state.bumped).toEqual({ "line-1": true });
    send.mockRestore();
  });
});
