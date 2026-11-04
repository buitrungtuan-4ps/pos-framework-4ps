// `state/edge-write.ts` (ADR-0196 decision 1, ADR-0173 decision 4): every edge write paints first,
// is confirmed by the edge's answer, is put back on a refusal, and is put back and read again when
// no answer comes. It is sent once, and only an edge write may take this path.

import { describe, expect, it } from "vitest";

import { ApiError } from "../src/api/client";
import { editWrite, writesPending } from "../src/state/edge-write";

function trace() {
  const steps: string[] = [];
  return {
    steps,
    write: (send: () => Promise<string>) => ({
      paint: () => {
        steps.push("paint");
        return () => steps.push("undo");
      },
      send: () => {
        steps.push(`send pending=${writesPending()}`);
        return send();
      },
      confirmed: (answer: string) => steps.push(`confirmed ${answer}`),
      reread: () => steps.push("reread"),
    }),
  };
}

describe("an edge write", () => {
  it("paints, sends once while counted as pending, and takes the edge's answer", async () => {
    const { steps, write } = trace();
    expect(await editWrite("sales.order.fire", write(async () => "fired"))).toBe("fired");
    expect(steps).toEqual(["paint", "send pending=true", "confirmed fired"]);
    expect(writesPending()).toBe(false);
  });

  it("is put back on a refusal, and nothing is read again", async () => {
    const { steps, write } = trace();
    const refusal = new ApiError(409, "already voided", "TRANSITION_REFUSED");
    await expect(editWrite("sales.line.void", write(() => Promise.reject(refusal)))).rejects.toBe(refusal);
    expect(steps).toEqual(["paint", "send pending=true", "undo"]);
  });

  it("is put back and read again when no answer comes, never sent again", async () => {
    const { steps, write } = trace();
    await expect(editWrite("sales.course.fire", write(() => Promise.reject(new TypeError("network"))))).rejects.toThrow();
    expect(steps).toEqual(["paint", "send pending=true", "undo", "reread"]);
  });

  it("takes only the catalogue's edge writes", () => {
    const { write } = trace();
    // @ts-expect-error A money act waits for the edge on its own path (ADR-0196 decision 1).
    void editWrite("billing.bill.settle", write(async () => "x")).catch(() => undefined);
  });
});
