// A read that reports a change never joins a read already out (`state/engine.ts`): that read may
// have been answered before the change, and joining it applied what the edge held before. An item
// brought back while a read of the price book was out stayed sold out on the till that brought it
// back, and nothing read it again.

import { describe, expect, it } from "vitest";

import { load, register, reload } from "../src/state/engine";

describe("a reload after a change", () => {
  it("reads again behind a read already out, and applies what the edge holds now", async () => {
    const answers: Array<(body: string) => void> = [];
    const applied: string[] = [];
    register([
      {
        key: "menu",
        kind: "configuration",
        path: () => "/api/menu",
        read: () => new Promise<unknown>((resolve) => answers.push(resolve)),
        apply: (body) => applied.push(String(body)),
        required: false,
        nodes: ["menu"],
        persist: false,
      },
    ]);

    // A read goes out while the item is sold out, and the item is brought back before its answer.
    const before = load("menu");
    const after = reload("menu");
    expect(answers).toHaveLength(1);
    answers[0]?.("sold out");
    await before;
    // The reload read again once the first was answered, rather than joining it.
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(answers).toHaveLength(2);
    answers[1]?.("back on sale");
    await after;
    expect(applied).toEqual(["sold out", "back on sale"]);
  });
});
