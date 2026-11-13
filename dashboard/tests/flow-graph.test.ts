// The flow designer's edits keep a flow one the cloud accepts (ADR-0192): ids in `pos-core`'s shape,
// no prerequisite that makes a cycle, and no step left waiting on one that is gone.

import { describe, expect, it } from "vitest";

import type { Flow } from "../src/api/schemas";
import {
  addStep,
  arrows,
  columns,
  idFrom,
  isId,
  localProblems,
  moveStep,
  OPTIONAL,
  prerequisiteChoices,
  removeStep,
  renameStep,
  setPrerequisites,
  updateStep,
  waitingOn,
} from "../src/lib/flow-graph";

/** Seat, ask the guests, then take the order; the drinks step is optional and waits on seating. */
const seating = (): Flow => {
  let flow: Flow = { flow_id: "seat_and_order", start_action_id: "floor.table.seat", steps: [] };
  flow = addStep(flow, "Seat", { action_id: "floor.table.seat" });
  flow = addStep(flow, "Guests", { screen_id: "guest_count" }, ["seat"]);
  flow = addStep(flow, "Order", { action_id: "sales.line.add" }, ["guests"]);
  flow = addStep(flow, "Drinks", { screen_id: "drinks" }, ["seat"]);
  return updateStep(flow, "drinks", { kind: OPTIONAL });
};

describe("a step's id", () => {
  it("is made from its name in the shape the cloud accepts, and numbered when taken", () => {
    expect(idFrom("Số khách", [])).toBe("so_khach");
    expect(idFrom("Đặt bàn!", [])).toBe("dat_ban");
    expect(idFrom("Take the order", ["take_the_order"])).toBe("take_the_order_2");
    expect(idFrom("Take the order", ["take_the_order", "take_the_order_2"])).toBe("take_the_order_3");
    expect(idFrom("!!!", [])).toBe("step");
    expect(isId(idFrom("x".repeat(200), []))).toBe(true);
  });

  it("is renamed everywhere a step waits on it, and not to a malformed or taken one", () => {
    const flow = renameStep(seating(), "seat", "seat_table");
    expect(flow.steps.map((step) => step.step_id)).toEqual(["seat_table", "guests", "order", "drinks"]);
    expect(flow.steps[1]?.prerequisites).toEqual(["seat_table"]);
    expect(flow.steps[3]?.prerequisites).toEqual(["seat_table"]);
    expect(renameStep(seating(), "seat", "Seat Table")).toEqual(seating());
    expect(renameStep(seating(), "seat", "order")).toEqual(seating());
  });

  it("is renamed where another step's offline branch goes to it (ADR-0214)", () => {
    const flow = updateStep(seating(), "order", { offline: { step_id: "drinks" } });
    expect(renameStep(flow, "drinks", "drink_list").steps[2]?.offline).toEqual({ step_id: "drink_list" });
  });
});

describe("prerequisites", () => {
  it("offer only steps that do not already wait on the one choosing", () => {
    const flow = seating();
    expect(waitingOn(flow, "seat")).toEqual(new Set(["guests", "order", "drinks"]));
    expect(prerequisiteChoices(flow, "seat")).toEqual([]);
    expect(prerequisiteChoices(flow, "guests")).toEqual(["seat", "drinks"]);
    expect(prerequisiteChoices(flow, "drinks")).toEqual(["seat", "guests", "order"]);
  });

  it("drop a choice that would make a cycle, so no edit can make one", () => {
    const flow = setPrerequisites(seating(), "seat", ["order", "drinks"]);
    expect(flow.steps[0]?.prerequisites).toEqual([]);
    expect(localProblems(flow)).toEqual([]);
    const both = setPrerequisites(seating(), "order", ["guests", "drinks"]);
    expect(both.steps[2]?.prerequisites).toEqual(["guests", "drinks"]);
  });

  it("let go of a step that is removed", () => {
    const flow = removeStep(seating(), "guests");
    expect(flow.steps.map((step) => step.step_id)).toEqual(["seat", "order", "drinks"]);
    expect(flow.steps[1]?.prerequisites).toEqual([]);
    expect(localProblems(flow)).toEqual([]);
  });
});

describe("the drawing", () => {
  it("puts each step one column after its furthest prerequisite", () => {
    expect(Object.fromEntries(columns(seating()))).toEqual({ seat: 0, guests: 1, order: 2, drinks: 1 });
    expect(arrows(seating())).toEqual([
      { from: "seat", to: "guests" },
      { from: "guests", to: "order" },
      { from: "seat", to: "drinks" },
    ]);
  });

  it("puts a hand-written cycle after everything else rather than hanging", () => {
    const flow: Flow = {
      flow_id: "loop",
      start_action_id: "sales.line.add",
      steps: [
        { step_id: "a", action_id: "sales.line.add", kind: "STEP_KIND_REQUIRED", prerequisites: ["b"] },
        { step_id: "b", action_id: "sales.line.add", kind: "STEP_KIND_REQUIRED", prerequisites: ["a"] },
        { step_id: "c", action_id: "sales.line.add", kind: "STEP_KIND_REQUIRED" },
      ],
    };
    expect(columns(flow).get("c")).toBe(0);
    expect(columns(flow).get("a")).toBeGreaterThan(0);
    expect(localProblems(flow).filter((problem) => problem.key === "flow.problem.cycle")).toHaveLength(2);
  });
});

describe("editing", () => {
  it("clears a condition by leaving the field off, as the wire has it", () => {
    const set = updateStep(seating(), "drinks", { applies_when: 'order.channel == "SALES_CHANNEL_DINE_IN"' });
    expect(set.steps[3]?.applies_when).toBe('order.channel == "SALES_CHANNEL_DINE_IN"');
    const cleared = updateStep(set, "drinks", { applies_when: "" });
    expect("applies_when" in (cleared.steps[3] ?? {})).toBe(false);
  });

  it("moves a step within the till's order and no further", () => {
    expect(moveStep(seating(), "drinks", -1).steps.map((step) => step.step_id)).toEqual(["seat", "guests", "drinks", "order"]);
    expect(moveStep(seating(), "seat", -1)).toEqual(seating());
    expect(moveStep(seating(), "drinks", 1)).toEqual(seating());
  });

  it("says early what the cloud would refuse", () => {
    expect(localProblems(seating())).toEqual([]);
    const empty: Flow = { flow_id: "Bad Id", start_action_id: "", steps: [] };
    expect(localProblems(empty).map((problem) => problem.key)).toEqual([
      "flow.problem.flowId",
      "flow.problem.noStart",
      "flow.problem.stepCount",
    ]);
    const both = updateStep(seating(), "seat", { screen_id: "floor" });
    expect(localProblems(both)).toEqual([{ key: "flow.problem.oneThing", stepId: "seat" }]);
  });
});
