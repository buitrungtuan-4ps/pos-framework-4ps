// The order screen's parts as catalogued components (ADR-0190, ADR-0203 decision 1).
//
// The order screen keeps its state and its taps in `screens/Order.tsx`, where the step gate resolves
// them; it hands each region it draws to these components through `OrderContext`, and the default
// package's `order` screen arranges them. The three containers draw the screen's grid around them.

import { createContext, useContext, type JSX } from "solid-js";

import { Children, type NodeProps } from "./render";

// The order screen's regions, each drawn by the catalogued component of the same name (ADR-0203
// decision 1): what each reads is this screen's state, so the screen hands them over through a
// context. A region drawn on a screen that is not an order draws nothing.
export interface OrderRegions {
  readonly header: () => JSX.Element;
  readonly notices: () => JSX.Element;
  readonly move: () => JSX.Element;
  readonly bill: () => JSX.Element;
  readonly menuToolbar: () => JSX.Element;
  readonly menuFields: () => JSX.Element;
  readonly menuButtons: () => JSX.Element;
}

export const OrderContext = createContext<OrderRegions>();

function region(name: keyof OrderRegions): JSX.Element {
  return useContext(OrderContext)?.[name]() ?? null;
}

// `order_layout`: the order column and the menu side by side on a terminal, one above the other on a
// phone; on a tablet the menu is the screen and the bill a panel over its foot.
export function OrderLayout(props: NodeProps): JSX.Element {
  return (
    <section class="grid gap-4 p-4 terminal:grid-cols-[1fr_20rem]">
      <Children {...props} />
    </section>
  );
}

// `order_column`: the header and the bill. `min-w-0` on both grid items, or neither can shrink below
// its content and the column runs wider than a phone: a grid item's `min-width` is `auto`, not `0`.
// On a tablet this column's own box is dissolved (`contents`): its header becomes a row of the screen
// above the menu, and its body becomes the panel at the bottom.
export function OrderColumn(props: NodeProps): JSX.Element {
  return (
    <div class="min-w-0 tablet:contents terminal:block">
      <Children {...props} />
    </div>
  );
}

// `order_menu`: on a tablet the menu is the screen, with room at its foot for the closed panel.
export function OrderMenu(props: NodeProps): JSX.Element {
  return (
    <aside class="min-w-0 tablet:pb-56 terminal:pb-0">
      <Children {...props} />
    </aside>
  );
}

// `order_header`: the way back, the table or counter order, moving the table and its pre-bill. It
// wraps rather than squeezes: on a narrow phone, in a longer language, the move button goes to a
// line of its own instead of breaking every word in the row onto two.
export function OrderHeader(): JSX.Element {
  return region("header");
}

// `order_notices`: what the pre-bill print and a table move said.
export function OrderNotices(): JSX.Element {
  return region("notices");
}

// `order_move`: the free tables the guests can move to. A tap moves them, with their order, and
// opens the new table; the table they leave waits to be cleared.
export function OrderMove(): JSX.Element {
  return region("move");
}

// `order_bill`: the bill. A phone and a terminal draw it in the column, as they always did. A tablet
// slides it up from the bottom of the screen: closed, it shows the bar saying what is on it, the
// seat row, and the two buttons that end the screen, so sending and paying stay one tap away; open,
// it is the whole bill, over the menu, with the bar at its top to close it.
export function OrderBill(): JSX.Element {
  return region("bill");
}

// `menu_toolbar`: the menu's heading, marking dishes sold out, and why the menu sells nothing.
export function MenuToolbar(): JSX.Element {
  return region("menuToolbar");
}

// `menu_fields`: the search, and the note for the next dish.
export function MenuFields(): JSX.Element {
  return region("menuFields");
}

// `menu_buttons`: the menu's buttons as the store arranged them, or what the search found.
export function MenuButtons(): JSX.Element {
  return region("menuButtons");
}
