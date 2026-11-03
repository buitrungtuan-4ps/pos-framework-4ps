// `state/money-act.ts` (ADR-0196, ADR-0204): a money act refused before sending paints nothing, a
// refusal rolls back with its sentence, and an act with no answer stays pending and is read again —
// fast, then slow and "unreachable" — until the edge says, and is never sent twice.

import { createRoot } from "solid-js";
import { describe, expect, it } from "vitest";

import { ApiError } from "../src/api/client";
import { isUnconfirmed, type Recheck, useMoneyAct } from "../src/state/money-act";

/** A clock and timers the test moves by hand. */
function fakeTime() {
  let clock = 0;
  let queue: { at: number; run: () => void; id: number }[] = [];
  let next = 1;
  return {
    now: () => clock,
    timers: {
      set: (run: () => void, ms: number) => {
        queue.push({ at: clock + ms, run, id: next });
        return next++;
      },
      clear: (id: number) => {
        queue = queue.filter((entry) => entry.id !== id);
      },
    },
    async advance(ms: number) {
      clock += ms;
      const due = queue.filter((entry) => entry.at <= clock);
      queue = queue.filter((entry) => entry.at > clock);
      for (const entry of due) entry.run();
      await Promise.resolve();
      await Promise.resolve();
    },
  };
}

function setup(recheck: (subject: string) => Promise<Recheck<string>>, kind?: string) {
  const time = fakeTime();
  const committed: string[] = [];
  const act = createRoot(() =>
    useMoneyAct<string>({
      act: kind,
      recheck,
      committed: (value) => committed.push(value),
      timers: time.timers,
      now: time.now,
    }),
  );
  return { act, committed, time };
}

describe("a money act", () => {
  it("refused before sending sends nothing and paints nothing", async () => {
    const { act } = setup(async () => null);
    let sent = 0;
    await act.run("bill-1", () => "No.", async () => {
      sent += 1;
      return "x";
    });
    expect(sent).toBe(0);
    expect(act.phase()).toBe("idle");
    expect(act.refusal()).toBe("No.");
    // Blocked until dismissed.
    await act.run("bill-1", () => null, async () => "x");
    expect(sent).toBe(0);
    act.dismiss();
    expect(act.refusal()).toBeNull();
  });

  it("refused by the edge rolls back with the edge's sentence", async () => {
    const { act, committed } = setup(async () => null);
    await act.run("bill-2", () => null, async () => {
      throw new ApiError(409, "the bill is not open", null);
    });
    expect(act.phase()).toBe("idle");
    expect(act.refusal()).toBe("the bill is not open");
    expect(committed).toEqual([]);
    expect(isUnconfirmed("bill-2")).toBe(false);
  });

  it("with no answer stays pending, reads again, and takes the edge's word", async () => {
    let answer: Recheck<string> = null;
    let reads = 0;
    const { act, committed, time } = setup(async () => {
      reads += 1;
      return answer;
    });
    await act.run("bill-3", () => null, async () => {
      throw new TypeError("network");
    });
    expect(act.phase()).toBe("checking");
    expect(isUnconfirmed("bill-3")).toBe(true);
    // Never offered again while unconfirmed.
    let resent = 0;
    await act.run("bill-3", () => null, async () => {
      resent += 1;
      return "y";
    });
    expect(resent).toBe(0);

    await time.advance(2_000);
    expect(reads).toBe(1);
    expect(act.phase()).toBe("checking");
    for (let elapsed = 2_000; elapsed < 32_000; elapsed += 2_000) await time.advance(2_000);
    expect(act.phase()).toBe("unreachable");
    const fast = reads;
    await time.advance(2_000);
    expect(reads).toBe(fast);
    answer = { committed: "receipt 42" };
    await time.advance(10_000);
    expect(committed).toEqual(["receipt 42"]);
    expect(act.phase()).toBe("idle");
    expect(isUnconfirmed("bill-3")).toBe(false);
  });

  it("with no answer rolls back only when the edge says it was not made", async () => {
    const { act, committed, time } = setup(async () => ({ refused: "Not taken." }));
    await act.run("bill-4", () => null, async () => {
      throw new ApiError(503, "unavailable", "STORE_UNAVAILABLE");
    });
    expect(act.phase()).toBe("checking");
    await time.advance(2_000);
    expect(act.phase()).toBe("idle");
    expect(act.refusal()).toBe("Not taken.");
    expect(committed).toEqual([]);
  });
});

describe("a money act left unconfirmed", () => {
  it("is picked up again only by the act that left it", async () => {
    const split = setup(async () => null, "split");
    await split.act.run("bill-5", () => null, async () => {
      throw new TypeError("network");
    });
    // A screen opened again: the settle there does not take the split's subject as its own.
    const settle = setup(async () => ({ committed: "settled" }), "settle");
    settle.act.resume("bill-5");
    expect(settle.act.phase()).toBe("idle");
    const again = setup(async () => ({ committed: "split" }), "split");
    again.act.resume("bill-5");
    expect(again.act.phase()).toBe("checking");
    await again.time.advance(2_000);
    expect(again.committed).toEqual(["split"]);
    expect(isUnconfirmed("bill-5")).toBe(false);
  });
});

describe("a flow's refusal (ADR-0192)", () => {
  it("names the step the edge says comes first", async () => {
    const { errorMessage } = await import("../src/lib/errors");
    const message = errorMessage(
      new ApiError(409, "step guests of flow dine_in comes first", "FLOW_PREREQUISITE_UNMET"),
    );
    expect(message).toContain("guests");
  });
});
