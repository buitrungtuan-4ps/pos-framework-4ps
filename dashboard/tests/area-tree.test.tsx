// The area tree (ADR-0211): the console draws a store's areas in the order the cloud publishes,
// offers only the moves the cloud would take, moves a branch by drag and drop or from a list, shows a
// refusal on the row it is about, and warns (never refuses) past three levels.

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "../src/api/client";
import type { Area } from "../src/api/schemas";
import { areaTree, breadcrumb, descendants, levelsOf, moveTargets, treeRefusal } from "../src/lib/area-tree";
import { AreaTree } from "../src/screens/floor/AreaTree";
import { inOperatorWords } from "../src/screens/floor/area-writes";

const area = (area_id: string, name: string, parent: string | null = null, status = "active") =>
  ({ area_id, name, parent_area_id: parent, status, etag: "v1" }) as unknown as Area;

/** Tầng 2 (A1) ⊃ VIP (A3) ⊃ Phòng 1 (A4); Tầng 1 (A2) at the top. */
const STORE = [area("A4", "Phòng 1", "A3"), area("A2", "Tầng 1"), area("A3", "VIP", "A1"), area("A1", "Tầng 2")];

/** A chain of `n` areas, each inside the one before. */
const chain = (n: number) =>
  Array.from({ length: n }, (_, at) => area(`C${at + 1}`, `Level ${at + 1}`, at === 0 ? null : `C${at}`));

describe("the area tree", () => {
  it("is drawn in the published order, with each area's depth and breadcrumb", () => {
    const nodes = areaTree(STORE);
    expect(nodes.map((node) => [node.area.area_id, node.depth])).toEqual([
      ["A1", 0],
      ["A3", 1],
      ["A4", 2],
      ["A2", 0],
    ]);
    expect(breadcrumb(nodes[2]!)).toBe("Tầng 2 › VIP › Phòng 1");
  });

  it("draws every area once even when the store is broken", () => {
    const broken = [area("X1", "One", "X2"), area("X2", "Two", "X1"), area("X3", "Lost", "NOPE")];
    expect(areaTree(broken).map((node) => node.area.area_id).sort()).toEqual(["X1", "X2", "X3"]);
  });

  it("offers no move into the area itself, an area inside it, or past eight levels", () => {
    expect([...descendants(STORE, "A1")].sort()).toEqual(["A3", "A4"]);
    expect(moveTargets(STORE, "A1").map((target) => target.area_id)).toEqual(["A2"]);
    expect(moveTargets(STORE, "A2").map((target) => target.area_id)).toEqual(["A1", "A3", "A4"]);
    const eight = [...chain(7), area("Z", "Alone")];
    expect(moveTargets(eight, "Z").map((target) => target.area_id)).toContain("C7");
    const branch = [...chain(7), area("Z", "Holder"), area("Z2", "Inside", "Z")];
    expect(moveTargets(branch, "Z").map((target) => target.area_id)).not.toContain("C7");
    expect(moveTargets(branch, "Z").map((target) => target.area_id)).toContain("C6");
  });

  it("counts the levels of the live plan", () => {
    expect(levelsOf([])).toBe(0);
    expect(levelsOf([area("A", "a"), area("B", "b")])).toBe(1);
    expect(levelsOf(STORE)).toBe(3);
    expect(levelsOf(chain(4))).toBe(4);
  });

  it("reads a tree refusal from a failure and puts it in the operator's words", () => {
    expect(treeRefusal([{ field: "parent_area_id", reason: "AREA_CYCLE" }] as never)).toBe("AREA_CYCLE");
    expect(treeRefusal([{ field: "name", reason: "REQUIRED" }] as never)).toBeNull();
    const caught = new ApiError(409, "an area cannot sit inside itself", "FAILED_PRECONDITION", [
      { field: "parent_area_id", reason: "AREA_TOO_DEEP" },
    ] as never);
    const worded = inOperatorWords(caught) as ApiError;
    expect(worded.message).not.toBe(caught.message);
    expect(worded.status).toBe(409);
    const other = new Error("offline");
    expect(inOperatorWords(other)).toBe(other);
  });
});

describe("the area tree view", () => {
  afterEach(cleanup);

  function mount(areas: readonly Area[], refusal: (id: string) => string | undefined = () => undefined, canManage = true) {
    const onMove = vi.fn();
    render(() => <AreaTree areas={areas} canManage={canManage} busy={false} refusal={refusal} onMove={onMove} />);
    return onMove;
  }

  /** Drops the area `areaId` onto `target`, as a drag of its row carries it. */
  function drop(target: Element, areaId: string) {
    const data = { getData: () => `pos-area:${areaId}`, setData: () => undefined };
    fireEvent.dragOver(target, { dataTransfer: data });
    fireEvent.drop(target, { dataTransfer: data });
  }

  const row = (areaId: string) => document.querySelector(`[data-tree-area="${areaId}"] > div`)!;

  it("moves a dropped branch inside another area or to the top, and never into itself", () => {
    const onMove = mount(STORE);
    drop(row("A2"), "A3");
    expect(onMove).toHaveBeenLastCalledWith(STORE[2], "A2");
    drop(document.querySelector("[data-tree-top]")!, "A3");
    expect(onMove).toHaveBeenLastCalledWith(STORE[2], null);
    onMove.mockClear();
    drop(row("A4"), "A1");
    expect(onMove).not.toHaveBeenCalled();
  });

  it("moves an area from its list, the way a keyboard does", () => {
    const onMove = mount(STORE);
    const select = screen.getByLabelText("Move Phòng 1 to") as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "A2" } });
    expect(onMove).toHaveBeenLastCalledWith(STORE[0], "A2");
  });

  it("shows a refusal on its row, and moves nothing for a role that cannot manage", () => {
    const onMove = mount(STORE, (id) => (id === "A3" ? "Areas still sit inside this one." : undefined), false);
    expect(document.querySelector('[data-tree-refusal="A3"]')?.textContent).toBe("Areas still sit inside this one.");
    drop(row("A2"), "A3");
    expect(onMove).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("Move VIP to")).toBeNull();
  });

  it("warns past three levels, and only then", () => {
    mount(STORE);
    expect(document.querySelector("[data-tree-deep]")).toBeNull();
    cleanup();
    mount(chain(4));
    expect(document.querySelector("[data-tree-deep]")).not.toBeNull();
  });
});
