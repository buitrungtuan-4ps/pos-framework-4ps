// The selling flows, declared once: what each task costs in taps, and what it must reach.
//
// One declaration, two gates ([ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md)):
//
// * `step-budget.mjs` reads it statically — the route must exist in `App.tsx`, its screen must
//   exist, the named action must be invoked by an interactive element there, that element must carry
//   `data-step="<action>"`, and the outcome's screen must carry `data-outcome="<mark>"`.
// * `tests/replay.spec.mjs` reads the same array and drives a browser — it clicks exactly these
//   taps, in this order, against a real `examples/minimal-edge`, and asserts the outcome appears.
//
// Splitting the declaration out is the whole point: a flow that grows a step has one place to say
// so, and neither gate can be satisfied by lying to the other. The static gate cannot see a tap
// nobody declared; the browser gate cannot see a renamed handler. Together they can.
//
// # The fields
//
// * `task` — the operator's sentence for it. Unique, and the key the replay harness reports under.
// * `budget` — the ceiling from `docs/ui-ux.md` §6: two taps for a common action, three for a rare
//   one. A task at its ceiling is fine; a task that needs one more is a design conversation, not a
//   number to raise.
// * `steps` — the taps, in order. `route` is a path in `App.tsx`; `action` is the function the
//   element's handler calls **and** the value of its `data-step`.
// * `outcome` — where the flow ends: a `route` and the `mark` of a `data-outcome` element that is
//   visible there once the flow has succeeded. This is what makes an undeclared extra tap fail —
//   insert a confirmation into a pay flow and the last declared tap no longer reaches `settled`.
// * `unreplayable` — present only when the browser gate cannot run the flow against the on-fakes
//   example, naming why. The harness asserts the set it skips is exactly this set, so coverage
//   cannot quietly shrink.

export const TASKS = [
  {
    task: "Seat a table and start its order",
    budget: 2,
    note: "The floor plan is home. One tap on a free table seats it and opens the order.",
    steps: [{ route: "/", action: "onCard" }],
    outcome: { route: "/table/:id", mark: "order-open" },
  },
  {
    task: "Add an item to an open order",
    budget: 2,
    note: "The item grid is on the order screen, so an item is one tap. §6's headline case.",
    steps: [{ route: "/table/:id", action: "addItem" }],
    outcome: { route: "/table/:id", mark: "line-added" },
  },
  {
    task: "Fire the open lines to the kitchen",
    budget: 2,
    note: "The fire button is fixed on the order screen and shows the unfired count.",
    steps: [{ route: "/table/:id", action: "fire" }],
    outcome: { route: "/table/:id", mark: "line-fired" },
  },
  {
    task: "Settle a dine-in table in cash",
    budget: 3,
    note: "Pay from the order screen, choose the note tendered, take the cash. Three taps, at the ceiling for a money path — this is the flow to defend hardest.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "setTender" },
      { route: "/table/:id/pay", action: "payCash" },
    ],
    outcome: { route: "/table/:id/pay", mark: "settled" },
  },
  {
    task: "Settle a dine-in table in cash, taking a tip",
    budget: 4,
    note: "Four, and §6's ceiling for a rare action is three — declared anyway because the alternative is the blind spot this script warns about. The tip is *optional*: the flow above is what a settle costs, and this is what it costs when a guest leaves something. Shortening it would mean choosing the note for the cashier, which is the one thing on this screen nobody should guess.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "setTip" },
      { route: "/table/:id/pay", action: "setTender" },
      { route: "/table/:id/pay", action: "payCash" },
    ],
    outcome: { route: "/table/:id/pay", mark: "settled" },
  },
  {
    task: "Charge a counter order in cash, taking a tip",
    budget: 4,
    note: "The counter's twin of the case above, for the same reason.",
    steps: [
      { route: "/counter", action: "charge" },
      { route: "/counter", action: "setTip" },
      { route: "/counter", action: "setTender" },
      { route: "/counter", action: "payCash" },
    ],
    outcome: { route: "/counter", mark: "settled" },
    unreplayable:
      "a counter order arrives from the cloud over the relay (ADR-0093, ADR-0061), and the on-fakes example has no cloud_url, so no relay runs and the counter list is always empty",
  },
  {
    task: "Settle a dine-in table by card",
    budget: 3,
    note: "One tap fewer than cash: a card takes the exact amount, so there is no note to choose.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "payCard" },
    ],
    outcome: { route: "/table/:id/pay", mark: "settled" },
  },
  {
    task: "Bump a ticket on the kitchen display",
    budget: 1,
    note: "A tap anywhere on the card. One, not two: the kitchen has both hands full.",
    steps: [{ route: "/kds", action: "onBump" }],
    // The board's empty state, which is the honest outcome for the one-ticket fixture the harness
    // fires: bumping the only ticket clears the board. A busier kitchen would still show the rest.
    outcome: { route: "/kds", mark: "board-clear" },
  },
  {
    task: "Run away a course from the expo screen",
    budget: 1,
    note: "One tap on the group, for the same reason as the bump.",
    outcome: { route: "/expo", mark: "pass-clear" },
    steps: [{ route: "/expo", action: "runAway" }],
  },
  {
    task: "Charge a counter (takeaway) order in cash",
    budget: 3,
    note: "The counter list is home for that role, so a relayed order is charged without navigating to a table it does not have (ADR-0093).",
    steps: [
      { route: "/counter", action: "charge" },
      { route: "/counter", action: "setTender" },
      { route: "/counter", action: "payCash" },
    ],
    outcome: { route: "/counter", mark: "settled" },
    unreplayable:
      "the same missing relay as the tipped counter case above — there is no order at the counter to charge",
  },
  {
    task: "Charge a counter order by card",
    budget: 3,
    steps: [
      { route: "/counter", action: "charge" },
      { route: "/counter", action: "payCard" },
    ],
    outcome: { route: "/counter", mark: "settled" },
    unreplayable:
      "the same missing relay as the two counter cases above — there is no order at the counter to charge",
  },
  {
    task: "Open the cash shift with a float",
    budget: 3,
    note: "Rare, and it is a number being typed — §6 allows three for a rare action.",
    steps: [{ route: "/shift", action: "openShift" }],
    outcome: { route: "/shift", mark: "shift-open" },
  },
  {
    task: "Enter the blind cash count",
    budget: 3,
    note: "Blind by design (§11.1): the expected figure is not on screen, which is a control rather than a missing step.",
    steps: [{ route: "/shift", action: "countShift" }],
    outcome: { route: "/shift", mark: "shift-counted" },
  },
  {
    task: "Close the shift and reveal the variance",
    budget: 3,
    steps: [{ route: "/shift", action: "closeShift" }],
    outcome: { route: "/shift", mark: "shift-closed" },
  },
  {
    task: "Sign in on a paired device",
    budget: 3,
    note: "Before any selling happens, so it is outside the per-task budgets — declared to keep it measured too.",
    steps: [{ route: "/signin", action: "submit" }],
    // The floor, because that is where a signed-in device lands and what proves the sign-in took.
    outcome: { route: "/", mark: "floor" },
  },
];
