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
    steps: [{ route: "/table/:id", action: "onItem" }],
    outcome: { route: "/table/:id", mark: "line-added" },
  },
  {
    task: "Add an item that needs a choice",
    budget: 3,
    note: "An item the store attaches a modifier group to: tap it, choose, confirm. Three, and the ceiling for a *common* action is two — declared at three anyway, for the reason the tipped settle is declared at four. The choices are the point: a pizza sold without its size is priced wrong, and the edge refuses it (ADR-0127 decision 5), so there is no two-tap shape of this that is also correct. What the declaration does buy is the guarantee the common case did not move: an item attaching no group never opens the picker and still sells in the one tap \"Add an item\" declares — put the picker in front of every item and that task goes red, not this one.",
    steps: [
      { route: "/table/:id", action: "onItem" },
      { route: "/table/:id", action: "chooseModifier" },
      { route: "/table/:id", action: "confirmItem" },
    ],
    outcome: { route: "/table/:id", mark: "line-modifiers" },
  },
  {
    task: "Change how many of a line",
    budget: 2,
    note: "One tap on the line's own stepper. Declared on the **+** button: both controls call the same action, and the map names an action rather than an element, so declaring the pair would measure the same tap twice. Minus is the same single tap and is deliberately disabled at one — zero is not a smaller order, it is a void, which carries a reason and a manager once the kitchen has the ticket.",
    steps: [{ route: "/table/:id", action: "setQuantity" }],
    outcome: { route: "/table/:id", mark: "line-quantity" },
  },
  {
    task: "Order an item for a particular seat",
    budget: 2,
    note: "Two, at the ceiling, and the second tap is the item itself — choosing the seat is the first. Declared as its own task rather than as a step inside \"Add an item\", for the reason the tipped settles are separate: a seat is *optional*, and folding it in would make the common flow read as two taps when it is one. The choice is sticky because a server orders a whole seat's worth at once; per-item it would cost a tap per dish. The control is absent entirely unless the store assigns seats, so on most stores this task does not exist.",
    steps: [
      { route: "/table/:id", action: "chooseSeat" },
      { route: "/table/:id", action: "onItem" },
    ],
    outcome: { route: "/table/:id", mark: "line-seat" },
  },
  {
    task: "Find an item by name and add it",
    budget: 2,
    note: "One tap, and the typing before it is not one — the same accounting the shift float and the manager's PIN get. That is the whole claim: a menu too long for the grid costs the flow nothing extra to sell from. Declared separately from \"Add an item\" although it taps the same control and ends the same way, because the claim is different and the harness proves it differently: the precondition types the query **and asserts the grid narrowed to one button**, so a search that stopped filtering fails here while the plain add stays green. Put search behind a button and this goes red twice over — the box the precondition fills would be gone, and the flow would have grown the tap this says it does not need.",
    steps: [{ route: "/table/:id", action: "onItem" }],
    outcome: { route: "/table/:id", mark: "line-added" },
  },
  {
    task: "Fire the open lines to the kitchen",
    budget: 2,
    note: "The send button is fixed on the order screen and shows the unsent count. This note described a button that did not exist: the screen carried a Send on every row, so this task really cost one tap per line and the gate could not see it — a declaration naming an action is satisfied by any element calling it, however many of them there are. One button now sends the whole order in one transaction, which is what makes the two honest.",
    steps: [{ route: "/table/:id", action: "fireOrder" }],
    outcome: { route: "/table/:id", mark: "line-fired" },
  },
  {
    task: "Fire one course to the kitchen",
    budget: 2,
    note: "\"Starters away\" — the send button narrowed to one course (ADR-0130). One tap, like sending the whole order, because it is the same act on a smaller set and a server saying it out loud does not first say which table twice. The row is drawn only for courses that still have food waiting, in the store's published service order, so the tap count does not grow with the menu. A store with courses off draws no row at all and this flow is simply absent there — which is the point of declaring it: put the course picker behind a menu, or make it ask which course in a dialog, and this goes to three and the gate says so.",
    steps: [{ route: "/table/:id", action: "fireCourse" }],
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
    task: "Void an unfired line",
    budget: 3,
    note: "Rare, and §6 allows three. It costs two: the void control is on the line itself, and the reason is the second tap — the reason is mandatory (ADR-0115), so it is part of the act rather than a question asked afterwards. Nothing was made and no stock moved, so no manager is involved.",
    steps: [
      { route: "/table/:id", action: "askVoid" },
      { route: "/table/:id", action: "voidReason" },
    ],
    outcome: { route: "/table/:id", mark: "line-voided" },
  },
  {
    task: "Void a line the kitchen has already been given",
    budget: 3,
    note: "The same two taps as above. Declared separately because the act is not the same one — §5 puts a fired line behind `VoidFiredLine` and a verified PIN — and this is where that shows: the manager's badge and PIN are *typed*, and typing is not a tap, exactly as the shift float is not one. The claim being pinned here is that requiring a second person costs the flow no extra tap.",
    steps: [
      { route: "/table/:id", action: "askVoid" },
      { route: "/table/:id", action: "voidReason" },
    ],
    outcome: { route: "/table/:id", mark: "line-voided" },
    unreplayable:
      "the manager's badge and PIN go into fields that exist only once the picker is open, and the harness fills a form in a precondition — before the first tap — so it has no moment to type them in",
  },
  {
    task: "Void a bill before it settles",
    budget: 3,
    note: "Three, at §6's ceiling for a rare action: open the bill, void it, cite a reason. A bill is money whether or not the kitchen started (§6), so this one always needs a manager and there is no cheaper shape of it.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "askVoidBill" },
      { route: "/table/:id/pay", action: "voidBillReason" },
    ],
    outcome: { route: "/table/:id/pay", mark: "bill-voided" },
    unreplayable:
      "the same missing moment as the fired-line void above — the manager's badge and PIN are typed between the second and third taps, and the harness types only before the first",
  },
  {
    task: "Take money off a bill",
    budget: 3,
    note: "Three, at §6's ceiling for a rare action: open the bill, take money off, cite a reason. The amount and the manager's badge are **typed**, and typing is not a tap — the same accounting the shift float and the void's PIN get. The manager is not this screen's choice: `billing.discount.apply` is granted to a server and carries no PIN flag, but no store publishes the ceiling that permission's own description refers to, so the edge reads it as zero and answers `403` naming the override. When a ceiling is published a small discount will go through without one, and this flow will not have grown or lost a tap either way.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "askDiscount" },
      { route: "/table/:id/pay", action: "discountReason" },
    ],
    outcome: { route: "/table/:id/pay", mark: "bill-discounted" },
    unreplayable:
      "the same missing moment as the two voids above — the amount and the manager's badge and PIN go into fields that exist only once the panel is open, and the harness types only in a precondition, before the first tap",
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
