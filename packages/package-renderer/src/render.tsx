// The one renderer of a package screen (ADR-0190 decision 3), which the till draws its screens with
// and the console previews them with (ADR-0209): a package screen is a tree of nodes, and each node
// mounts the Solid component its `component_id` names. A package gives a node only literals
// (ADR-0203); the component reads its own part of the screen's context from the host screen.
//
// # What the renderer guarantees
//
// - **Every catalogued component has an implementation.** `Implementations` is a
//   `Record<ComponentId, …>`, so a component added to `pos-proto`'s catalogue without one here does
//   not compile (each front end's table, completing `BASE` in `./base.tsx`).
// - **An unknown component never fails the screen** (decision 4). A node naming a component this
//   release does not have draws a slot that says the till needs an update, and the rest of the
//   screen draws.
// - **The tree is bounded.** The cloud and the edge refuse a screen over `NODES_MAX` nodes or
//   `DEPTH_MAX` levels; a tree that arrives anyway is not drawn, and the host draws its own screen.
//
// Nothing here reads data, calls the edge or holds a word. The host screen supplies the context and
// runs the acts, and the front end that draws supplies its words (`Words`).

import { createContext, For, useContext, type JSX } from "solid-js";

import { COMPONENTS, DEPTH_MAX, NODES_MAX, type ActionBinding, type ComponentId, type PropertyValue, type ScreenNode } from "./components";

/** What a component's implementation is given: its node, and how deep it is (the root is 1). */
export interface NodeProps {
  readonly node: ScreenNode;
  readonly depth: number;
}

/** A component's implementation. */
export type Implementation = (props: NodeProps) => JSX.Element;

/** One implementation per catalogued component. */
export type Implementations = Readonly<Record<ComponentId, Implementation>>;

/** The ids a component may supply to the act it binds (ADR-0203 decision 3). */
export type Supplied = Readonly<Record<string, string>>;

/** What the screen hosting a package tree gives its components. */
export interface ScreenHost {
  /** The order and table the screen is for, which an `action_button` supplies. */
  readonly subject: () => Supplied;
  /** Whether the screen can run this act now; a button for one it cannot is disabled. */
  readonly offers: (actionId: string) => boolean;
  /**
   * Runs an act with the package's fixed parameters and the ids the component supplied. A request
   * it answers is the act on its way: the button takes no second tap until it settles, and says
   * what a failure was.
   */
  readonly run: (binding: ActionBinding, supplied: Supplied) => unknown;
}

/** The words a front end draws a package with, in its own language. */
export interface Words {
  /** A package label: a translation key a node names. A key the front end cannot show draws nothing. */
  readonly label: (key: string) => string;
  /** What a node of a component this release lacks says. */
  readonly needsUpdate: () => string;
  /** What a button says under itself when its act failed. */
  readonly failure: (caught: unknown) => string;
}

const NO_WORDS: Words = { label: () => "", needsUpdate: () => "", failure: () => "" };

const HostContext = createContext<ScreenHost>();
const WordsContext = createContext<Words>(NO_WORDS);
const ImplementationsContext = createContext<Implementations>();

/** The host screen, for a component that binds an act or reads the screen's subject. */
export function useHost(): ScreenHost | undefined {
  return useContext(HostContext);
}

const KNOWN: ReadonlySet<string> = new Set(COMPONENTS.map((component) => component.component_id));

function isComponentId(id: string): id is ComponentId {
  return KNOWN.has(id);
}

/** The words of the front end drawing the tree. */
export function useWords(): Words {
  return useContext(WordsContext);
}

/** Whether a tree is within the limits the cloud and the edge enforce. */
export function withinLimits(root: ScreenNode): boolean {
  let nodes = 0;
  const pending: [ScreenNode, number][] = [[root, 1]];
  for (let next = pending.pop(); next !== undefined; next = pending.pop()) {
    const [node, depth] = next;
    nodes += 1;
    if (nodes > NODES_MAX || depth > DEPTH_MAX) {
      return false;
    }
    for (const child of node.children ?? []) {
      pending.push([child, depth + 1]);
    }
  }
  return true;
}

/** A property's text: a translation key or a token. */
export function textProperty(node: ScreenNode, name: string): string | undefined {
  const value: PropertyValue | undefined = node.properties?.[name];
  return typeof value === "string" ? value : undefined;
}

/** An action property. */
export function actionProperty(node: ScreenNode, name: string): ActionBinding | undefined {
  const value = node.properties?.[name];
  return typeof value === "object" ? value : undefined;
}

/** What a node of a component this release lacks draws in its place, saying `text`. */
export function NeedsUpdateSlot(props: { text: string }): JSX.Element {
  return (
    <p class="rounded-token border border-dashed border-line p-3 text-sm text-ink-muted" data-package-fallback>
      {props.text}
    </p>
  );
}

/** One node: its component's implementation, or the slot that says the till needs an update. */
export function RenderNode(props: NodeProps): JSX.Element {
  const implementations = useContext(ImplementationsContext);
  const id = props.node.component_id;
  if (props.depth > DEPTH_MAX || implementations === undefined) {
    return null;
  }
  if (!isComponentId(id)) {
    return <NeedsUpdateSlot text={useWords().needsUpdate()} />;
  }
  return implementations[id](props);
}

/** A node's children, in order, one level deeper. */
export function Children(props: NodeProps): JSX.Element {
  return <For each={props.node.children ?? []}>{(child) => <RenderNode node={child} depth={props.depth + 1} />}</For>;
}

/**
 * A package tree drawn for a host screen, in `words`. A tree over the limits is not drawn;
 * `fallback`, the screen's own, is drawn instead.
 */
export function PackageTree(props: {
  root: ScreenNode;
  host: ScreenHost;
  implementations: Implementations;
  words: Words;
  fallback: JSX.Element;
}): JSX.Element {
  if (!withinLimits(props.root)) {
    return props.fallback;
  }
  return (
    <ImplementationsContext.Provider value={props.implementations}>
      <WordsContext.Provider value={props.words}>
        <HostContext.Provider value={props.host}>
          <RenderNode node={props.root} depth={1} />
        </HostContext.Provider>
      </WordsContext.Provider>
    </ImplementationsContext.Provider>
  );
}
