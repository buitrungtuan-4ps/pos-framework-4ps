// A package screen's tree, edited without hand-written JSON (ADR-0190, ADR-0203): every change the
// screen designer makes is one of these functions, each returning a new tree and leaving the old
// one as it was, so a change is a value the draft can keep.
//
// A node is addressed by its path, the positions of its ancestors' children from the root down; the
// root's path is `[]`. The cloud names a refused node by its position in the tree's pre-order, the
// root being 0 (`pathAtPreorder` turns one into the other).

import { COMPONENTS, type Component, type PropertyValue, type ScreenNode } from "../package/components";

/** Where a node is: the positions of its ancestors' children, from the root down. */
export type NodePath = readonly number[];

/** The catalogue's entry for `componentId`, if this console's catalogue has it. */
export function componentOf(componentId: string): Component | undefined {
  return COMPONENTS.find((component) => component.component_id === componentId);
}

/** A node of `componentId` with nothing given yet. */
export function newNode(componentId: string): ScreenNode {
  return { component_id: componentId };
}

/** The node at `path`, if there is one. */
export function nodeAt(root: ScreenNode, path: NodePath): ScreenNode | undefined {
  let node: ScreenNode | undefined = root;
  for (const at of path) {
    node = node?.children?.[at];
  }
  return node;
}

/** Whether a node of `node`'s component may hold others. */
export function holdsChildren(node: ScreenNode): boolean {
  return componentOf(node.component_id)?.holds_children ?? false;
}

/** `root` with the node at `path` replaced by what `change` makes of it. */
export function updateAt(root: ScreenNode, path: NodePath, change: (node: ScreenNode) => ScreenNode): ScreenNode {
  const [first, ...rest] = path;
  if (first === undefined) {
    return change(root);
  }
  const children = [...(root.children ?? [])];
  const child = children[first];
  if (child === undefined) {
    return root;
  }
  children[first] = updateAt(child, rest, change);
  return { ...root, children };
}

/** `root` with `node` inserted as the `index`th child of the node at `parent`. */
export function insertAt(root: ScreenNode, parent: NodePath, index: number, node: ScreenNode): ScreenNode {
  return updateAt(root, parent, (holder) => {
    const children = [...(holder.children ?? [])];
    children.splice(Math.max(0, Math.min(index, children.length)), 0, node);
    return { ...holder, children };
  });
}

/** `root` without the node at `path`. The root itself cannot be removed. */
export function removeAt(root: ScreenNode, path: NodePath): ScreenNode {
  const parent = path.slice(0, -1);
  const index = path.at(-1);
  if (index === undefined) {
    return root;
  }
  return updateAt(root, parent, (holder) => {
    const children = (holder.children ?? []).filter((_, at) => at !== index);
    const { children: _dropped, ...rest } = holder;
    return children.length === 0 ? rest : { ...rest, children };
  });
}

/** Whether `inner` is `outer` or below it. */
export function isWithin(inner: NodePath, outer: NodePath): boolean {
  return outer.length <= inner.length && outer.every((at, depth) => inner[depth] === at);
}

/**
 * `root` with the node at `from` moved to be the `index`th child of the node at `parent`, counted
 * as the parent's children were before the move. A node cannot move into itself or below itself,
 * and `null` says so.
 */
export function moveNode(root: ScreenNode, from: NodePath, parent: NodePath, index: number): ScreenNode | null {
  const node = nodeAt(root, from);
  if (node === undefined || from.length === 0 || isWithin(parent, from)) {
    return null;
  }
  return insertAt(removeAt(root, from), afterRemoval(parent, from), indexAfterRemoval(parent, from, index), node);
}

/** `path` as it reads once the node at `removed` is gone: a later sibling's position drops by one. */
function afterRemoval(path: NodePath, removed: NodePath): NodePath {
  const depth = removed.length - 1;
  const at = removed[depth] ?? 0;
  const below = path.length > depth && isWithin(path, removed.slice(0, -1));
  return below && (path[depth] ?? 0) > at ? path.map((step, level) => (level === depth ? step - 1 : step)) : path;
}

/** A child position under `parent`, counted before `removed` went, as it reads after. */
function indexAfterRemoval(parent: NodePath, removed: NodePath, index: number): number {
  const sameParent = parent.length === removed.length - 1 && isWithin(parent, removed.slice(0, -1));
  return sameParent && (removed.at(-1) ?? 0) < index ? index - 1 : index;
}

/** Where a component added at `path` goes: into the node there if it holds others, else after it. */
export function slotAfter(root: ScreenNode, path: NodePath): { parent: NodePath; index: number } | null {
  const node = nodeAt(root, path);
  if (node === undefined) {
    return null;
  }
  if (holdsChildren(node)) {
    return { parent: path, index: node.children?.length ?? 0 };
  }
  return path.length === 0 ? null : { parent: path.slice(0, -1), index: (path.at(-1) ?? 0) + 1 };
}

/** `root` with the node at `path` moved one place up (`-1`) or down (`1`) among its siblings. */
export function shiftNode(root: ScreenNode, path: NodePath, by: -1 | 1): { root: ScreenNode; path: NodePath } | null {
  const index = path.at(-1);
  const parent = path.slice(0, -1);
  const siblings = nodeAt(root, parent)?.children?.length ?? 0;
  if (index === undefined || index + by < 0 || index + by >= siblings) {
    return null;
  }
  const moved = moveNode(root, path, parent, by < 0 ? index - 1 : index + 2);
  return moved === null ? null : { root: moved, path: [...parent, index + by] };
}

/** `root` with the property `name` of the node at `path` set to `value`, or left out for `undefined`. */
export function setProperty(
  root: ScreenNode,
  path: NodePath,
  name: string,
  value: PropertyValue | undefined,
): ScreenNode {
  return updateAt(root, path, (node) => {
    const properties = { ...(node.properties ?? {}) };
    if (value === undefined) {
      delete properties[name];
    } else {
      properties[name] = value;
    }
    const { properties: _old, ...rest } = node;
    return Object.keys(properties).length === 0 ? rest : { ...rest, properties };
  });
}

/** The path of the node at position `index` of the tree's pre-order, the root being 0. */
export function pathAtPreorder(root: ScreenNode, index: number): NodePath | undefined {
  let seen = 0;
  const pending: [ScreenNode, NodePath][] = [[root, []]];
  for (let next = pending.pop(); next !== undefined; next = pending.pop()) {
    const [node, path] = next;
    if (seen === index) {
      return path;
    }
    seen += 1;
    const children = node.children ?? [];
    for (let at = children.length - 1; at >= 0; at -= 1) {
      const child = children[at];
      if (child !== undefined) {
        pending.push([child, [...path, at]]);
      }
    }
  }
  return undefined;
}

/** How many nodes the tree has. */
export function countNodes(root: ScreenNode): number {
  return 1 + (root.children ?? []).reduce((sum, child) => sum + countNodes(child), 0);
}

/** Whether a component is drawn on `deviceClass`, or on every class for a variant that names none. */
export function drawnOn(component: Component, deviceClass: string | undefined): boolean {
  if (deviceClass === undefined) {
    return DEVICE_CLASSES.every((known) => component.device_classes.includes(known));
  }
  return component.device_classes.includes(deviceClass);
}

/** The device classes a screen may have its own variant for (ADR-0190 decision 5). */
export const DEVICE_CLASSES = [
  "DEVICE_CLASS_TERMINAL",
  "DEVICE_CLASS_TABLET",
  "DEVICE_CLASS_PHONE",
  "DEVICE_CLASS_KITCHEN_DISPLAY",
  "DEVICE_CLASS_KIOSK",
] as const;
