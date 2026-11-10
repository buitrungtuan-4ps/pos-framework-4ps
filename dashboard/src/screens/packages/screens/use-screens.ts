// The screen designer's state and acts: which variant and node are chosen, and every change to the
// draft's screens, each made with the tree functions in `lib/screen-tree.ts`.

import { createMemo, createSignal } from "solid-js";

import type { ScreenVariant } from "../../../api/schemas";
import type { ScreenNode } from "../../../package/components";
import {
  insertAt,
  moveNode,
  newNode,
  nodeAt,
  shiftNode,
  slotAfter,
  pathAtPreorder,
  removeAt,
  setProperty,
  type NodePath,
} from "../../../lib/screen-tree";
import type { PropertyValue } from "../../../package/components";
import type { PartProps } from "../designer/parts";
import type { Dropped, Slot } from "./Canvas";

/** A tree as the draft holds it: the same shape, typed by the generated schema. */
const wire = (node: ScreenNode) => node as unknown as ScreenVariant["body"]["root"];

/** The chosen variant and node of the draft's screens, and the acts that change them. */
export function useScreens(props: PartProps) {
  const [chosen, setChosen] = createSignal(0);
  const [selected, setSelected] = createSignal<NodePath>([]);
  const screens = () => (props.draft().screens ?? []) as readonly ScreenVariant[];
  const variant = () => screens()[chosen()];
  const root = () => variant()?.body.root as ScreenNode | undefined;
  const writeScreens = (next: readonly ScreenVariant[]) => props.change((draft) => ({ ...draft, screens: [...next] }));
  const writeRoot = (next: ScreenNode) =>
    writeScreens(
      screens().map((entry, at) => (at === chosen() ? { ...entry, body: { ...entry.body, root: wire(next) } } : entry)),
    );
  const screenFaults = createMemo(() => props.refusals.filter((refusal) => refusal.screen_index === chosen()));
  const faulty = () => {
    const fault = screenFaults().find((refusal) => refusal.node_index !== undefined);
    const tree = root();
    return fault && tree ? (pathAtPreorder(tree, fault.node_index ?? 0) ?? null) : null;
  };
  const choose = (index: number) => {
    setChosen(index);
    setSelected([]);
  };
  const addScreen = (screenId: string) => {
    writeScreens([...screens(), { selector: {}, body: { screen_id: screenId, root: wire(newNode("screen_frame")) } }]);
    choose(screens().length - 1);
  };
  const addVariant = (deviceClass: string) => {
    const base = variant();
    if (base) {
      writeScreens([...screens(), { selector: { device_class: deviceClass }, body: base.body }]);
      choose(screens().length - 1);
    }
  };
  const removeVariant = () => {
    const gone = variant();
    if (gone) {
      // The base variant goes with the whole screen: a screen without one cannot be published.
      const base = !gone.selector?.device_class;
      writeScreens(screens().filter((entry, at) => (base ? entry.body.screen_id !== gone.body.screen_id : at !== chosen())));
      choose(0);
    }
  };
  /** Adds a component where the keyboard is: into a selected holder, else after the selected node. */
  const addComponent = (componentId: string) => {
    const tree = root();
    const slot = tree && slotAfter(tree, selected());
    if (tree && slot) {
      writeRoot(insertAt(tree, slot.parent, slot.index, newNode(componentId)));
      setSelected([...slot.parent, slot.index]);
    }
  };
  const drop = (slot: Slot, what: Dropped) => {
    const tree = root();
    if (!tree) {
      return;
    }
    if ("componentId" in what) {
      writeRoot(insertAt(tree, slot.parent, slot.index, newNode(what.componentId)));
      setSelected([...slot.parent, slot.index]);
      return;
    }
    const moved = moveNode(tree, what.from, slot.parent, slot.index);
    if (moved) {
      writeRoot(moved);
      setSelected([]);
    }
  };
  const move = (by: -1 | 1) => {
    const tree = root();
    const moved = tree && shiftNode(tree, selected(), by);
    if (moved) {
      writeRoot(moved.root);
      setSelected(moved.path);
    }
  };
  const remove = () => {
    const tree = root();
    if (tree && selected().length > 0) {
      writeRoot(removeAt(tree, selected()));
      setSelected(selected().slice(0, -1));
    }
  };
  const node = () => {
    const tree = root();
    return tree ? nodeAt(tree, selected()) : undefined;
  };
  const set = (name: string, value: PropertyValue | undefined) => {
    const tree = root();
    if (tree) {
      writeRoot(setProperty(tree, selected(), name, value));
    }
  };
  return { chosen, choose, selected, setSelected, screens, variant, root, node, faulty, addScreen, addVariant, removeVariant, addComponent, drop, move, remove, set };
}
