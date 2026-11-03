// SPIKE (throwaway): the catalogue the Order screen's button area is drawn from — five components
// plus `each`. Classes are the hand-written screen's own, so the two draw the same DOM.

import { For, type JSX } from "solid-js";

import { Children, list, Repeat, resolve, type Component, type Node, type Scope } from "./render";

const GRID = "grid grid-cols-2 gap-2 tablet:grid-cols-3 terminal:grid-cols-1";

/** The host's actions the catalogue calls; the screen keeps every rule. */
export interface OrderActions {
  sellItem: (item: unknown, caption: string) => JSX.Element;
  sellButton: (button: unknown) => JSX.Element;
  isShown: (category: unknown) => boolean;
  pick: (category: unknown) => void;
  label: (key: string) => string;
}

export function orderCatalogue(actions: OrderActions): Record<string, Component> {
  const stack: Component = (p) => (
    <div>
      <Children node={p.node} scope={p.scope} />
    </div>
  );
  const grid: Component = (p) => (
    <div class={GRID}>
      <Repeat node={p.node} scope={p.scope} />
    </div>
  );
  const each: Component = (p) => <Repeat node={p.node} scope={p.scope} />;
  const section: Component = (p) => {
    const title = () => String(resolve(p.node.props?.["title"], p.scope) ?? "");
    const level = p.node.props?.["level"] === 4 ? 4 : 3;
    return (
      <section class={level === 3 ? "mb-4" : "mt-3"}>
        {level === 3 ? (
          <h3 class="mb-2 text-xs font-semibold uppercase tracking-wide text-ink-muted">{title()}</h3>
        ) : (
          <h4 class="mb-2 text-xs text-ink-muted">{title()}</h4>
        )}
        <Children node={p.node} scope={p.scope} />
      </section>
    );
  };
  const tabs: Component = (p) => (
    <div class="-mx-1 mb-3 flex gap-2 overflow-x-auto px-1 pb-1" role="tablist" aria-label={actions.label("order.categories")}>
      <For each={list(String(p.node.props?.["source"] ?? ""), p.scope)}>
        {(category) => (
          <button
            type="button"
            role="tab"
            aria-selected={actions.isShown(category)}
            class="min-h-touch shrink-0 rounded-token border px-3 text-sm"
            classList={{
              "border-primary bg-selected text-selected-ink font-semibold": actions.isShown(category),
              "border-line bg-surface text-ink": !actions.isShown(category),
            }}
            onClick={() => actions.pick(category)}
          >
            {(category as { name: string }).name}
          </button>
        )}
      </For>
    </div>
  );
  const itemButton: Component = (p: { node: Node; scope: Scope }) => {
    const props = p.node.props ?? {};
    if (props["button"] !== undefined) {
      return actions.sellButton(resolve(props["button"], p.scope));
    }
    const item = resolve(props["item"], p.scope) as { display_name: string };
    return actions.sellItem(item, item.display_name);
  };
  return { stack, grid, each, section, tabs, item_button: itemButton };
}
