// The screen designer's tree edits: each returns a new tree, and a move counts positions as they
// were before it, so a drop lands where the operator saw the slot.

import { describe, expect, it } from "vitest";

import type { ScreenNode } from "../../packages/package-renderer/src/components";
import {
  insertAt,
  moveNode,
  newNode,
  nodeAt,
  pathAtPreorder,
  removeAt,
  setProperty,
  shiftNode,
  slotAfter,
} from "../src/lib/screen-tree";

const text = (key: string): ScreenNode => ({ component_id: "text", properties: { text_key: key } });

/** A frame holding a heading, a stack of two texts, and a text. */
const TREE: ScreenNode = {
  component_id: "screen_frame",
  children: [
    { component_id: "page_header", properties: { title_key: "a.title" } },
    { component_id: "stack", children: [text("b.one"), text("b.two")] },
    text("c.last"),
  ],
};

const ids = (root: ScreenNode): string[] => [
  `${root.component_id}${typeof root.properties?.["text_key"] === "string" ? `:${root.properties["text_key"]}` : ""}`,
  ...(root.children ?? []).flatMap(ids),
];

describe("a screen's tree", () => {
  it("inserts, removes and sets a property without changing the tree it was given", () => {
    const added = insertAt(TREE, [1], 1, newNode("action_button"));
    expect(nodeAt(added, [1, 1])?.component_id).toBe("action_button");
    expect(nodeAt(TREE, [1, 1])?.properties?.["text_key"]).toBe("b.two");
    expect(removeAt(TREE, [1, 0]).children?.[1]?.children).toEqual([text("b.two")]);
    expect(nodeAt(setProperty(TREE, [2], "variant", "muted"), [2])?.properties).toEqual({ text_key: "c.last", variant: "muted" });
    expect(nodeAt(setProperty(TREE, [2], "text_key", undefined), [2])).toEqual({ component_id: "text" });
  });

  it("moves a node into another holder and along its own, counting slots as they were", () => {
    const intoStack = moveNode(TREE, [2], [1], 0);
    expect(ids(intoStack!)).toEqual(["screen_frame", "page_header", "stack", "text:c.last", "text:b.one", "text:b.two"]);
    // The heading moves below the stack: the stack's own path shifts up as the heading leaves.
    const headingIntoStack = moveNode(TREE, [0], [1], 2);
    expect(ids(headingIntoStack!)).toEqual(["screen_frame", "stack", "text:b.one", "text:b.two", "page_header", "text:c.last"]);
    // Down by one among siblings, and never into itself.
    expect(ids(moveNode(TREE, [0], [], 2)!)).toEqual(ids({ ...TREE, children: [TREE.children![1]!, TREE.children![0]!, TREE.children![2]!] }));
    expect(moveNode(TREE, [1], [1], 0)).toBeNull();
    expect(moveNode(TREE, [], [1], 0)).toBeNull();
  });

  it("shifts a node among its siblings and adds after a leaf or into a holder", () => {
    expect(shiftNode(TREE, [1, 1], -1)?.path).toEqual([1, 0]);
    expect(nodeAt(shiftNode(TREE, [1, 1], -1)!.root, [1, 0])).toEqual(text("b.two"));
    expect(shiftNode(TREE, [2], 1)).toBeNull();
    expect(slotAfter(TREE, [1])).toEqual({ parent: [1], index: 2 });
    expect(slotAfter(TREE, [1, 0])).toEqual({ parent: [1], index: 1 });
  });

  it("finds the node the cloud names by its pre-order position", () => {
    expect(pathAtPreorder(TREE, 0)).toEqual([]);
    expect(pathAtPreorder(TREE, 3)).toEqual([1, 0]);
    expect(pathAtPreorder(TREE, 5)).toEqual([2]);
    expect(pathAtPreorder(TREE, 6)).toBeUndefined();
  });
});
