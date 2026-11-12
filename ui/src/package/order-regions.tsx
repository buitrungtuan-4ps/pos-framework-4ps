// The order screen's parts as catalogued components (ADR-0190, ADR-0203 decision 1).
//
// The order screen hands its regions over through `./regions.tsx`; these three containers draw the
// screen's grid around them, and the default package's `order` screen arranges both.

import type { JSX } from "solid-js";

import { Children, type NodeProps } from "../../../packages/package-renderer/src/render";

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
