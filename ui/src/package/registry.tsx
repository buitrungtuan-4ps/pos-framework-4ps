// The implementation of every component in the catalogue `pos-proto` declares
// (`components.ts`, ADR-0190 decision 2): the five that draw from the package alone, from the
// package renderer (`BASE`, ADR-0209), and the till's regions. A `Record<ComponentId,
// Implementation>`: a component catalogued without one here does not compile, and one here that the catalogue does not hold is an
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

import { BASE } from "../../../packages/package-renderer/src/base";
import { region } from "../../../packages/package-renderer/src/regions";
import type { Implementations } from "../../../packages/package-renderer/src/render";
import { FloorAlerts, FloorPlan } from "../screens/Floor";
import { OrderColumn, OrderLayout, OrderMenu } from "./order-regions";

/** Every catalogued component's implementation. */
export const IMPLEMENTATIONS: Implementations = {
  ...BASE,
  floor_alerts: FloorAlerts,
  floor_plan: FloorPlan,
  order_layout: OrderLayout,
  order_column: OrderColumn,
  order_header: region("order_header"),
  order_notices: region("order_notices"),
  order_move: region("order_move"),
  order_bill: region("order_bill"),
  order_menu: OrderMenu,
  menu_toolbar: region("menu_toolbar"),
  menu_fields: region("menu_fields"),
  menu_buttons: region("menu_buttons"),
  pay_back: region("pay_back"),
  pay_flow: region("pay_flow"),
  kds_frame: region("kds_frame"),
  kds_header: region("kds_header"),
  kds_notices: region("kds_notices"),
  kds_stations: region("kds_stations"),
  kds_tickets: region("kds_tickets"),
  expo_tickets: region("expo_tickets"),
  confirm_queue: region("confirm_queue"),
  counter_frame: region("counter_frame"),
  counter_flow: region("counter_flow"),
  today_body: region("today_body"),
  records_body: region("records_body"),
  shift_screen: region("shift_screen"),
  devices_body: region("devices_body"),
  pair_body: region("pair_body"),
  setup_body: region("setup_body"),
  sign_in_screen: region("sign_in_screen"),
};
