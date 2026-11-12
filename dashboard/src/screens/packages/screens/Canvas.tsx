// A screen's tree as nested boxes, edited by dragging (ADR-0190): a component dragged from the
// palette, or a box dragged from the tree, lands in the slot it is dropped on. Native HTML drag and
// drop, so no library; every drag has a keyboard twin in the inspector (add, move up and down,
// remove), so the pointer is never the only way.

import { For, Show } from "solid-js";

import { t } from "../../../i18n";
import type { ScreenNode } from "../../../../../packages/package-renderer/src/components";
import { holdsChildren, type NodePath } from "../../../lib/screen-tree";
import { componentName } from "./names";

/** What was dropped: a component from the palette, or a node of the tree. */
export type Dropped = { readonly componentId: string } | { readonly from: NodePath };

/** Where a drop lands: the `index`th child of the node at `parent`. */
export type Slot = { readonly parent: NodePath; readonly index: number };

// Plain text with a prefix, the one type every browser carries through a drag unchanged.
const COMPONENT_PREFIX = "pos-component:";
const NODE_PREFIX = "pos-node:";

/** Marks a drag as carrying a component from the palette. */
export function dragComponent(event: DragEvent, componentId: string): void {
  event.dataTransfer?.setData("text/plain", `${COMPONENT_PREFIX}${componentId}`);
  if (event.dataTransfer) {
    event.dataTransfer.effectAllowed = "copy";
  }
}

function dropped(event: DragEvent): Dropped | null {
  const text = event.dataTransfer?.getData("text/plain") ?? "";
  if (text.startsWith(COMPONENT_PREFIX)) {
    return { componentId: text.slice(COMPONENT_PREFIX.length) };
  }
  if (!text.startsWith(NODE_PREFIX)) {
    return null;
  }
  try {
    const parsed: unknown = JSON.parse(text.slice(NODE_PREFIX.length));
    return Array.isArray(parsed) && parsed.every((at) => typeof at === "number") ? { from: parsed } : null;
  } catch {
    return null;
  }
}

type CanvasProps = {
  readonly selected: NodePath | null;
  readonly faulty: NodePath | null;
  /** A note beside a node, by its path joined with dots: the acts there over their tap budget. */
  readonly notes?: ReadonlyMap<string, readonly string[]>;
  readonly onSelect: (path: NodePath) => void;
  readonly onDrop: (slot: Slot, what: Dropped) => void;
};

/** A place between two boxes where a drop lands. */
function DropSlot(props: { slot: Slot; onDrop: CanvasProps["onDrop"] }) {
  return (
    <div
      class="h-2 rounded-token transition-colors data-[over]:h-6 data-[over]:bg-accent/30"
      data-drop-slot={`${props.slot.parent.join(".")}:${props.slot.index}`}
      onDragOver={(event) => {
        event.preventDefault();
        event.currentTarget.dataset["over"] = "";
      }}
      onDragLeave={(event) => delete event.currentTarget.dataset["over"]}
      onDrop={(event) => {
        event.preventDefault();
        delete event.currentTarget.dataset["over"];
        const what = dropped(event);
        if (what) {
          props.onDrop(props.slot, what);
        }
      }}
    />
  );
}

/** One node and, for a component that holds others, its children with a slot around each. */
function NodeBox(props: CanvasProps & { node: ScreenNode; path: NodePath }) {
  const same = (path: NodePath | null) => path !== null && path.join(".") === props.path.join(".");
  const keys = () =>
    Object.entries(props.node.properties ?? {})
      .filter(([, value]) => typeof value === "string")
      .map(([, value]) => value)
      .join(" · ");
  return (
    <div
      class="rounded-token border bg-surface p-2"
      classList={{
        "border-accent ring-2 ring-accent": same(props.selected),
        "border-danger": same(props.faulty),
        "border-line": !same(props.selected) && !same(props.faulty),
      }}
      draggable={props.path.length > 0}
      data-node-path={props.path.join(".")}
      onDragStart={(event) => {
        event.stopPropagation();
        event.dataTransfer?.setData("text/plain", `${NODE_PREFIX}${JSON.stringify(props.path)}`);
      }}
    >
      <button
        type="button"
        class="flex w-full items-baseline gap-2 text-left text-sm"
        aria-pressed={same(props.selected)}
        onClick={() => props.onSelect(props.path)}
      >
        <span class="font-medium text-ink">{componentName(props.node.component_id)}</span>
        <span class="truncate text-ink-muted">{keys()}</span>
      </button>
      <For each={props.notes?.get(props.path.join(".")) ?? []}>
        {(note) => (
          <p class="mt-1 text-xs text-ink-muted" data-outcome="tap-warning">
            {note}
          </p>
        )}
      </For>
      <Show when={holdsChildren(props.node)}>
        <div class="mt-1 flex flex-col pl-3">
          <DropSlot slot={{ parent: props.path, index: 0 }} onDrop={props.onDrop} />
          <For each={props.node.children ?? []}>
            {(child, at) => (
              <>
                <NodeBox {...props} node={child} path={[...props.path, at()]} />
                <DropSlot slot={{ parent: props.path, index: at() + 1 }} onDrop={props.onDrop} />
              </>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

/** The whole tree of one screen variant. */
export function Canvas(props: CanvasProps & { root: ScreenNode }) {
  return (
    <div class="flex flex-col gap-1" aria-label={t("designer.screens.canvas")} role="group">
      <NodeBox {...props} node={props.root} path={[]} />
    </div>
  );
}
