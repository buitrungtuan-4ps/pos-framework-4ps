// The components that draw from the package alone (ADR-0209 decision 1): `stack`, `text`,
// `action_button`, `screen_frame` and `page_header`. The till and the console's preview draw these
// with the same code; every other component in the catalogue is a region, which the front end
// drawing the screen supplies (the till its screens' regions, the preview an outline). Each front
// end's table is `{ ...BASE, …its regions }`, a `Record<ComponentId, Implementation>`, so a
// component catalogued without an implementation still does not compile.
//
// Every implementation is a function declaration, never a `const`: the till's table is part of an
// import cycle (a screen draws its package tree, whose table names the screen's components), which
// is safe only because hoisted functions exist before either module finishes evaluating.

import { createSignal, Show, type JSX } from "solid-js";

import type { ActionBinding, ComponentId } from "./components";
import { actionProperty, Children, textProperty, useHost, useWords, type Implementation, type NodeProps } from "./render";

/** Its children down the screen, or across it. */
function Stack(props: NodeProps): JSX.Element {
  const across = () => textProperty(props.node, "direction") === "horizontal";
  return (
    <div class="flex gap-2" classList={{ "flex-col": !across(), "flex-wrap items-center": across() }}>
      <Children {...props} />
    </div>
  );
}

const TEXT_CLASS: Readonly<Record<string, string>> = {
  title: "text-lg font-semibold",
  body: "text-base",
  muted: "text-sm text-ink-muted",
};

/** A node's label property in the front end's words; an absent one draws nothing. */
function label(words: { label: (key: string) => string }, node: NodeProps["node"], name: string): string {
  const key = textProperty(node, name);
  return key === undefined ? "" : words.label(key);
}

/** One line of text from the translation grid. */
function Text(props: NodeProps): JSX.Element {
  const words = useWords();
  const variant = () => textProperty(props.node, "variant") ?? "body";
  return <p class={TEXT_CLASS[variant()] ?? TEXT_CLASS["body"]}>{label(words, props.node, "text_key")}</p>;
}

const BUTTON_CLASS: Readonly<Record<string, string>> = {
  primary: "bg-primary text-primary-ink",
  secondary: "border border-line bg-surface text-ink",
  danger: "bg-danger text-danger-ink",
};

/**
 * A button that does one act on the order or table the screen is for. An act the host sends to the
 * edge takes no second tap until it is answered, and a failure is said under the button, in the
 * front end's words for it.
 */
function ActionButton(props: NodeProps): JSX.Element {
  const host = useHost();
  const words = useWords();
  const binding = () => actionProperty(props.node, "action");
  const variant = () => textProperty(props.node, "variant") ?? "secondary";
  const [sending, setSending] = createSignal(false);
  const [failure, setFailure] = createSignal<string | null>(null);
  const offered = () => {
    const act = binding();
    return host !== undefined && act !== undefined && host.offers(act.action_id);
  };
  const tap = (act: ActionBinding) => {
    if (host === undefined || sending()) {
      return;
    }
    setFailure(null);
    const ran = host.run(act, host.subject());
    if (ran instanceof Promise) {
      setSending(true);
      ran.catch((caught: unknown) => setFailure(words.failure(caught))).finally(() => setSending(false));
    }
  };
  return (
    <Show when={binding()}>
      {(act) => (
        <>
          <button
            type="button"
            class={`min-h-touch rounded-token px-4 font-semibold disabled:opacity-50 ${BUTTON_CLASS[variant()] ?? BUTTON_CLASS["secondary"]}`}
            disabled={!offered() || sending()}
            aria-busy={sending() ? true : undefined}
            data-action-id={act().action_id}
            onClick={() => tap(act())}
          >
            {label(words, props.node, "label_key")}
          </button>
          <Show when={failure()}>{(message) => <p role="alert" class="text-sm text-danger">{message()}</p>}</Show>
        </>
      )}
    </Show>
  );
}

const FRAME_CLASS: Readonly<Record<string, string>> = {
  full: "p-4",
  narrow: "mx-auto max-w-xl p-4",
  medium: "mx-auto max-w-2xl p-4",
  small: "mx-auto max-w-sm p-4",
};

/** A whole screen's frame. */
function ScreenFrame(props: NodeProps): JSX.Element {
  const width = () => FRAME_CLASS[textProperty(props.node, "width") ?? "full"] ?? FRAME_CLASS["full"];
  return (
    <section class={width()}>
      <Children {...props} />
    </section>
  );
}

/** A screen's heading, as the till's `PageHeader` draws one. */
function Header(props: NodeProps): JSX.Element {
  const words = useWords();
  const size = textProperty(props.node, "size") === "extra_large" ? "text-xl" : "text-lg";
  return <h1 class={`mb-4 ${size} font-semibold`}>{label(words, props.node, "title_key")}</h1>;
}

/** The ids of the components that draw from the package alone. */
export type BaseId = "stack" | "text" | "action_button" | "screen_frame" | "page_header";

/** The implementation of each component that draws from the package alone. */
export const BASE: Readonly<Record<BaseId, Implementation>> = {
  stack: Stack,
  text: Text,
  action_button: ActionButton,
  screen_frame: ScreenFrame,
  page_header: Header,
};

/** Whether `id` is drawn from the package alone, rather than being a region its host supplies. */
export function isBase(id: ComponentId): id is BaseId {
  return Object.hasOwn(BASE, id);
}
