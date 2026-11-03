// SPIKE (throwaway): a screen region drawn from a JSON description and a component catalogue.
//
// A node names a catalogue component, may be shown only `when` a host flag holds, may repeat over a
// list (`each` + `as` + `template`), and passes `props` whose `$name.path` values are read from the
// scope. The host screen supplies the scope: named sources (accessors) and the components' actions.
// An unknown component draws nothing, which is the fallback ADR-0190 asks for.

import { For, Show, type JSX } from "solid-js";

export interface Node {
  component: string;
  when?: string;
  each?: string;
  as?: string;
  template?: Node;
  props?: Record<string, unknown>;
  children?: Node[];
}

/** What a region reads: named values, each an accessor or a plain value. */
export type Scope = Record<string, unknown>;

/** Reads `$a.b.c` from the scope; anything else is a literal. */
export function resolve(value: unknown, scope: Scope): unknown {
  if (typeof value !== "string" || !value.startsWith("$")) {
    return value;
  }
  const [head, ...path] = value.slice(1).split(".");
  let current = read(scope[head ?? ""]);
  for (const key of path) {
    current = current === null || current === undefined ? undefined : (current as Record<string, unknown>)[key];
  }
  return current;
}

function read(value: unknown): unknown {
  return typeof value === "function" ? (value as () => unknown)() : value;
}

/** A list source by name (a host source) or by path (`$category.buttons`). */
export function list(name: string, scope: Scope): readonly unknown[] {
  const value = name.startsWith("$") ? resolve(name, scope) : read(scope[name]);
  return Array.isArray(value) ? value : [];
}

export type Component = (props: { node: Node; scope: Scope }) => JSX.Element;

/** Each repetition of `node.template`, with `node.as` bound to the element. */
export function Repeat(props: { node: Node; scope: Scope }): JSX.Element {
  return (
    <For each={list(props.node.each ?? "", props.scope)}>
      {(element) => (
        <Render
          node={props.node.template ?? { component: "none" }}
          scope={{ ...props.scope, [props.node.as ?? "it"]: element }}
        />
      )}
    </For>
  );
}

export function Children(props: { node: Node; scope: Scope }): JSX.Element {
  return <For each={props.node.children ?? []}>{(child) => <Render node={child} scope={props.scope} />}</For>;
}

let catalogue: Record<string, Component> = {};

export function useCatalogue(components: Record<string, Component>): void {
  catalogue = components;
}

export function Render(props: { node: Node; scope: Scope }): JSX.Element {
  const component = catalogue[props.node.component];
  if (component === undefined) {
    return null;
  }
  const shown = () => props.node.when === undefined || read(props.scope[props.node.when]) === true;
  return <Show when={shown()}>{component({ node: props.node, scope: props.scope })}</Show>;
}
