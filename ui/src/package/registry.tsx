// The implementation of every component in the catalogue `pos-proto` declares
// (`components.ts`, ADR-0190 decision 2). A `Record<ComponentId, Implementation>`: a component
// catalogued without one here does not compile, and one here that the catalogue does not hold is an
// excess property, so the till and the validator cannot disagree on what exists.
//
// Each implementation reads its node's literals and its own part of the screen's context, and
// nothing else (ADR-0203). A screen's own components live in the screen's file, beside the state they
// read, and join this table as the screen moves into the default package. They stay there because
// `scripts/step-budget.mjs` resolves a route's taps in `screens/<Name>.tsx`.
//
// That makes an import cycle (a screen draws its package tree, whose table names the screen's
// components), which is safe only because every implementation is a function declaration: hoisted,
// so it exists before either module finishes evaluating. Never make one a `const`.

import { Show, type JSX } from "solid-js";

import { PageHeader } from "../components/ui";
import { FloorAlerts, FloorPlan } from "../screens/Floor";
import {
  MenuButtons,
  MenuFields,
  MenuToolbar,
  OrderBill,
  OrderColumn,
  OrderHeader,
  OrderLayout,
  OrderMenu,
  OrderMove,
  OrderNotices,
} from "./order-regions";
import { actionProperty, Children, packageLabel, textProperty, useHost, type Implementations, type NodeProps } from "./render";

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

/** One line of text from the translation grid. */
function Text(props: NodeProps): JSX.Element {
  const variant = () => textProperty(props.node, "variant") ?? "body";
  return <p class={TEXT_CLASS[variant()] ?? TEXT_CLASS["body"]}>{packageLabel(textProperty(props.node, "text_key"))}</p>;
}

const BUTTON_CLASS: Readonly<Record<string, string>> = {
  primary: "bg-primary text-primary-ink",
  secondary: "border border-line bg-surface text-ink",
  danger: "bg-danger text-danger-ink",
};

/** A button that does one act on the order or table the screen is for. */
function ActionButton(props: NodeProps): JSX.Element {
  const host = useHost();
  const binding = () => actionProperty(props.node, "action");
  const variant = () => textProperty(props.node, "variant") ?? "secondary";
  const offered = () => {
    const act = binding();
    return host !== undefined && act !== undefined && host.offers(act.action_id);
  };
  return (
    <Show when={binding()}>
      {(act) => (
        <button
          type="button"
          class={`min-h-touch rounded-token px-4 font-semibold disabled:opacity-50 ${BUTTON_CLASS[variant()] ?? BUTTON_CLASS["secondary"]}`}
          disabled={!offered()}
          data-action-id={act().action_id}
          onClick={() => host?.run(act(), host.subject())}
        >
          {packageLabel(textProperty(props.node, "label_key"))}
        </button>
      )}
    </Show>
  );
}

/** A whole screen's frame. */
function ScreenFrame(props: NodeProps): JSX.Element {
  return (
    <section class="p-4">
      <Children {...props} />
    </section>
  );
}

/** A screen's heading. */
function Header(props: NodeProps): JSX.Element {
  return <PageHeader title={packageLabel(textProperty(props.node, "title_key"))} />;
}

/** Every catalogued component's implementation. */
export const IMPLEMENTATIONS: Implementations = {
  stack: Stack,
  text: Text,
  action_button: ActionButton,
  screen_frame: ScreenFrame,
  page_header: Header,
  floor_alerts: FloorAlerts,
  floor_plan: FloorPlan,
  order_layout: OrderLayout,
  order_column: OrderColumn,
  order_header: OrderHeader,
  order_notices: OrderNotices,
  order_move: OrderMove,
  order_bill: OrderBill,
  order_menu: OrderMenu,
  menu_toolbar: MenuToolbar,
  menu_fields: MenuFields,
  menu_buttons: MenuButtons,
};
