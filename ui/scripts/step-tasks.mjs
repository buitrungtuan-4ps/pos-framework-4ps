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

// Why the browser gate's walk skips the drawer-per-till flows: it boots the example's one-drawer
// store, and these need the `drawers` store, a device a manager has bound to a till on the Devices
// screen, and a manager's code and PIN typed between the taps. Dedicated replays run each there.
const DRAWERS_REPLAYED_APART =
  "the walk boots a store with one drawer, and these need the `drawers` store, a device bound to a till, and a manager's badge and PIN typed between the taps; dedicated replays below run them";

// Why the walk skips a close that gives a reason: it boots a store that asks none of any close, and
// the `drawers` store asks one of an over or short beyond 20,000₫ (ADR-0167 decision 12).
const VARIANCE_REPLAYED_APART =
  "the walk boots a store that asks no close for a reason, and the `drawers` store asks one beyond 20,000₫ of a till bound to it; a dedicated replay below runs it there";

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
    task: "Mark an item sold out on every till",
    budget: 2,
    note: "Two: turn the marking on, tap the item. The kitchen has run out, and the console that publishes the menu is not where anybody is standing. Marking is a mode on the order screen rather than a control on every item button, so a tap meant to sell in a rush can never take a dish off the menu. The same tap brings it back.",
    steps: [
      { route: "/table/:id", action: "startMarking" },
      { route: "/table/:id", action: "toggleSoldOut" },
    ],
    outcome: { route: "/table/:id", mark: "item-sold-out" },
  },
  {
    task: "Move a table's guests to another table",
    budget: 2,
    note: "Two: open the list of free tables, tap the one they are moving to. The order goes with them (every line, what the kitchen already has, the time they sat down) and the new table opens; the table they left waits to be cleared. Nothing is rung again. No confirmation, because a move is undone the same way: clear the old table, and move them back.",
    steps: [
      { route: "/table/:id", action: "openMove" },
      { route: "/table/:id", action: "moveTo" },
    ],
    outcome: { route: "/table/:id", mark: "table-moved" },
  },
  {
    task: "Release a table seated by mistake",
    budget: 3,
    note: "One tap, where *Take payment* would be: with nothing sold on the table, the order screen offers *Release table* instead, and the table is free again on every device (ADR-0163). Rare, so its ceiling is three, and it needs one. No confirmation, because it is undone by seating the table again, and the edge refuses it once a dish is on the order: releasing says nobody ate.",
    steps: [{ route: "/table/:id", action: "releaseTable" }],
    outcome: { route: "/", mark: "floor" },
  },
  {
    task: "Print a pre-bill for a table",
    budget: 2,
    note: "One tap on the order screen, which is where the server is standing when the guests ask how much it is so far. No bill opens: a pre-bill is the check on paper, unnumbered and marked as not a receipt, so the guests can read it and still order dessert. A split table prints one per open part from the same tap. The pay screen carries the same button for one part of a split, and it is not declared separately because it is the same act on a smaller set.",
    steps: [{ route: "/table/:id", action: "printPreBill" }],
    outcome: { route: "/table/:id", mark: "pre-bill-print" },
  },
  {
    task: "Reprint a receipt from today's bills",
    budget: 3,
    note: "Two taps on the Today screen: the bill, then *Reprint*. For the guest who comes back for a copy of their receipt. The copy is the original under the same number, marked COPY with which copy it is, and each press is counted (ADR-0164), so there is no confirmation: a copy printed by mistake costs paper and one more on the count. Choosing first, rather than a *Reprint* on every row, is so that a tap on a crowded list prints the bill that was meant. Rare, so its ceiling is three.",
    steps: [
      { route: "/today", action: "chooseBill" },
      { route: "/today", action: "reprintReceipt" },
    ],
    outcome: { route: "/today", mark: "receipt-reprinted" },
  },
  {
    task: "Print a guest's receipt again right after they pay",
    budget: 3,
    note: "One tap under the settled figures, which is where the cashier is when the guest asks. The same act as the Today screen's reprint, counted the same way (ADR-0164). The counter's settled screen carries the same button, and it is not declared separately because it is the same act on the other pay screen.",
    steps: [{ route: "/table/:id/pay", action: "printAgain" }],
    outcome: { route: "/table/:id/pay", mark: "receipt-reprinted" },
  },
  {
    task: "Mark a dish sold out from the kitchen board",
    budget: 2,
    note: "Two: open the panel, tap the dish. The cook is usually the first to know something has run out, and the board is where the cook is. A panel over the board rather than a control on each ticket, because a ticket is one whole-card tap that bumps it and a second target inside it would be hit by mistake in a rush.",
    steps: [
      { route: "/kds", action: "openSoldOut" },
      { route: "/kds", action: "markSoldOut" },
    ],
    outcome: { route: "/kds", mark: "kds-sold-out" },
  },
  {
    task: "Find an item by name and add it",
    budget: 2,
    note: "One tap, and the typing before it is not one — the same accounting the shift float and the manager's PIN get. That is the whole claim: a menu too long for the grid costs the flow nothing extra to sell from. Declared separately from \"Add an item\" although it taps the same control and ends the same way, because the claim is different and the harness proves it differently: the precondition types the query **and asserts the grid narrowed to one button**, so a search that stopped filtering fails here while the plain add stays green. Put search behind a button and this goes red twice over — the box the precondition fills would be gone, and the flow would have grown the tap this says it does not need.",
    steps: [{ route: "/table/:id", action: "onItem" }],
    outcome: { route: "/table/:id", mark: "line-added" },
  },
  {
    task: "Add a dish with a note for the kitchen",
    budget: 2,
    note: "The same one tap as any add: the note is typed into the field above the menu first, and typing is not a tap. It rides with the next item added and then clears, so it cannot land on a second dish by accident. The kitchen reads it on its board and its ticket; the log records only that a note existed (ADR-0157). The precondition types the note **and narrows the grid to one dish that asks no question**, so the tap adds it at once and the note must be on the line that appears.",
    steps: [{ route: "/table/:id", action: "onItem" }],
    outcome: { route: "/table/:id", mark: "line-note" },
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
    task: "Settle a dine-in table by QR transfer",
    budget: 3,
    note: "The card's shape: the guest scans for the exact amount, so there is no note to choose. The one tap on the pay screen is the cashier saying the transfer arrived; the till does not ask a bank, so a confirmation step would only repeat that tap.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "payQr" },
    ],
    outcome: { route: "/table/:id/pay", mark: "settled" },
  },
  {
    task: "Split a dine-in bill evenly between two guests, each paying by QR",
    budget: 4,
    note: "Four for two guests, and the count is the point: one tap to say how many, then one per guest. Each guest's tender is their own choice, so no shape of this is shorter than the guests plus the choice of how many. The guest counts are a row on the pay screen rather than behind a button, which keeps the split at one tap before the guests' own; the last guest's tender settles the bill, so there is no closing tap either.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "splitEvenly" },
      { route: "/table/:id/pay", action: "payQr" },
      { route: "/table/:id/pay", action: "payQr" },
    ],
    outcome: { route: "/table/:id/pay", mark: "settled" },
  },
  {
    task: "Split one guest's items off a dine-in bill, then settle each part by QR",
    budget: 7,
    note: "Seven, and every one is the guests' or the split's own. Splitting takes three: open the list, pick what this guest is paying for, split it off. That is the rare-action ceiling, and picking is one tap per line the guest takes, so a guest with one dish is the shortest shape there is. Each guest then pays with their own tender, and the next bill is one tap from the receipt that closed the last, not a trip back through the order. The list sits behind a button rather than on every bill, because on a phone it would push the tenders below the fold for every settle that never splits. Splitting evenly is four for two guests because an even share needs no picking; a partition of lines cannot be shorter than naming the lines (ADR-0128).",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "splitByItem" },
      { route: "/table/:id/pay", action: "pickLine" },
      { route: "/table/:id/pay", action: "splitOff" },
      { route: "/table/:id/pay", action: "payQr" },
      { route: "/table/:id/pay", action: "nextBill" },
      { route: "/table/:id/pay", action: "payQr" },
    ],
    outcome: { route: "/table/:id/pay", mark: "settled" },
  },
  {
    task: "Split a dine-in bill by seat, then settle each seat's part by QR",
    budget: 5,
    note: "Five for two seats, and the split itself is one of them. Which seat each dish was for is already written on the line, so splitting by seat is a single tap with nothing to pick. The rest is the guests: one tender each, and one tap from the receipt that closed a seat's bill to the next. It is split by item with the picking already done, which is why it is two taps shorter.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "splitBySeat" },
      { route: "/table/:id/pay", action: "payQr" },
      { route: "/table/:id/pay", action: "nextBill" },
      { route: "/table/:id/pay", action: "payQr" },
    ],
    outcome: { route: "/table/:id/pay", mark: "settled" },
  },
  {
    task: "Settle a dine-in table in cash, typing the amount handed over",
    budget: 3,
    note: "For the pile no quick key names. \"Other amount\" takes the place of choosing a note, so the flow costs what the cash settle costs; the figure is typed on the pad, and typing is not a tap.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "typeTender" },
      { route: "/table/:id/pay", action: "payCash" },
    ],
    outcome: { route: "/table/:id/pay", mark: "settled" },
    unreplayable:
      "the amount is typed on the pad between the second and third taps, and the harness types only before the first; the standalone test \"a bill larger than the largest note takes any amount handed over\" drives the whole flow, typing included",
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
    task: "Waive a fee on a bill",
    budget: 3,
    note: "Three, at §6's ceiling for a rare action: open the bill, waive the fee, cite a reason — the discount's shape (ADR-0159 decision 5). The manager's badge and PIN are **typed** where the person needs an approver, which `billing.fee.waive` asks for wherever the store does not enforce each person's own set. A fee shows **Waive** only where its rule is published waivable, so a store whose fees all stay on every bill never sees this task.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "askWaiveFee" },
      { route: "/table/:id/pay", action: "waiveFeeReason" },
    ],
    outcome: { route: "/table/:id/pay", mark: "fee-waived" },
    unreplayable:
      "the example store publishes no fee to waive, and the manager's badge and PIN are typed between the second and third taps, which the harness cannot do",
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
  },
  {
    task: "Charge a counter order by card",
    budget: 3,
    steps: [
      { route: "/counter", action: "charge" },
      { route: "/counter", action: "payCard" },
    ],
    outcome: { route: "/counter", mark: "settled" },
  },
  {
    task: "Charge a counter order by QR transfer",
    budget: 3,
    note: "The counter's twin of the table's QR settle.",
    steps: [
      { route: "/counter", action: "charge" },
      { route: "/counter", action: "payQr" },
    ],
    outcome: { route: "/counter", mark: "settled" },
  },
  {
    task: "Start a counter order for a walk-in guest",
    budget: 2,
    note: "One tap on the counter screen opens a tableless order with the day's next queue number and lands on it (ADR-0146, F12). Before it, a store with no tables could charge an order somebody else started and could not start one. A store that asks each guest whether they eat in or take away takes a second tap, on the answer (ADR-0160 decision 2), which the budget allows.",
    steps: [{ route: "/counter", action: "newOrder" }],
    outcome: { route: "/order/:id", mark: "order-open" },
  },
  {
    task: "Add an item to a counter order",
    budget: 2,
    note: "The same grid and the same one tap as a table's order; the edge prices the line at the order's own channel, so the till sends only what the guest chose.",
    steps: [{ route: "/order/:id", action: "onItem" }],
    outcome: { route: "/order/:id", mark: "line-added" },
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
    task: "Pay cash out of the drawer, for a supplier",
    budget: 3,
    note: "The amount is typed first, then two taps: **Paid out**, and the reason (ADR-0165). No manager, because `cash.movement.record` asks no PIN: every paid out is on the record, and it moves what the close expects. A paid in is the same two taps with the other button, and is not declared separately, as the counter's reprint is not.",
    steps: [
      { route: "/shift", action: "askPaidOut" },
      { route: "/shift", action: "movementReason" },
    ],
    outcome: { route: "/shift", mark: "cash-moved" },
  },
  {
    task: "Open the cash drawer without a sale",
    budget: 3,
    note: "Two taps, **Open drawer** and the reason, with a manager's code and PIN typed between them, because `cash.drawer.open_no_sale` asks for one (ADR-0165). Rare, and a control: the PIN is the point, not a step to remove. It needs no open shift, because it moves no cash.",
    steps: [
      { route: "/shift", action: "askOpenDrawer" },
      { route: "/shift", action: "drawerReason" },
    ],
    outcome: { route: "/shift", mark: "drawer-opened" },
    unreplayable:
      "the manager's badge and PIN are typed between the two taps, and the harness types only before the first; a dedicated replay below opens the drawer with them",
  },
  {
    task: "Close the shift and reveal the variance",
    budget: 3,
    steps: [{ route: "/shift", action: "closeShift" }],
    outcome: { route: "/shift", mark: "shift-closed" },
  },
  {
    task: "Close the shift over or short, giving a reason",
    budget: 3,
    note: "Where the store asks a reason of a variance beyond its limit (`shift.variance_reason_minor`, ADR-0167 decision 12), **Close & reveal** answers with the variance the count fixed and the store's reasons for one, and a reason closes it: two taps. Only after the count, so a blind close stays blind; within the limit it is the one tap above.",
    steps: [
      { route: "/shift", action: "closeShift" },
      { route: "/shift", action: "varianceReason" },
    ],
    outcome: { route: "/shift", mark: "shift-closed" },
    unreplayable: VARIANCE_REPLAYED_APART,
  },
  // Every till's drawer, where the store keeps one per till (ADR-0167 decision 3). Rare, a manager's
  // acts at either end of the day, so three taps each.
  {
    task: "Start another till's drawer",
    budget: 3,
    note: "Pick the bar's drawer in the list of every till's drawer, then **Start**: its float is filled in with that till's own, which may be changed first. A manager's code and PIN are typed between the two taps where the person holds `cash.shift.manage_other_till` only with approval, as the no-sale asks for one; the PIN is the control, not a step to remove.",
    steps: [
      { route: "/shift", action: "pickDrawer" },
      { route: "/shift", action: "startDrawers" },
    ],
    outcome: { route: "/shift", mark: "drawers-started" },
    unreplayable: DRAWERS_REPLAYED_APART,
  },
  {
    task: "Start two tills' drawers at once",
    budget: 3,
    note: "A pick for each drawer, then **Start**: three for two, at the ceiling, and a pick more for each further till. Each starts on its own till's float, one request each so the edge records each start, and the first refusal stops the rest.",
    steps: [
      { route: "/shift", action: "pickDrawer" },
      { route: "/shift", action: "pickDrawer" },
      { route: "/shift", action: "startDrawers" },
    ],
    outcome: { route: "/shift", mark: "drawers-started" },
    unreplayable: DRAWERS_REPLAYED_APART,
  },
  {
    task: "Count another till's drawer, blind",
    budget: 3,
    note: "Pick the open drawer, type what it holds, **Enter count**: one drawer at a time, and as blind as a till's own count, so nothing it should hold is on screen unless the store has turned the blind close off.",
    steps: [
      { route: "/shift", action: "pickDrawer" },
      { route: "/shift", action: "countDrawer" },
    ],
    outcome: { route: "/shift", mark: "drawer-counted" },
    unreplayable: DRAWERS_REPLAYED_APART,
  },
  {
    task: "Close two counted drawers together",
    budget: 3,
    note: "A pick for each counted drawer, then **Close**: one act, every drawer or none, under one approver, and each drawer's figures shown after (ADR-0167 decision 10). Three for two, at the ceiling. One drawer is the same with one pick, and closes on its own, as a till's own close does.",
    steps: [
      { route: "/shift", action: "pickDrawer" },
      { route: "/shift", action: "pickDrawer" },
      { route: "/shift", action: "closeDrawers" },
    ],
    outcome: { route: "/shift", mark: "drawers-closed" },
    unreplayable: DRAWERS_REPLAYED_APART,
  },
  {
    task: "Close another till's drawer over or short, giving a reason",
    budget: 3,
    note: "Pick the counted drawer, **Close & reveal**, then the reason the store asks of its variance (ADR-0167 decision 12): three, at the ceiling. A manager's code and PIN typed before the close where the person needs one go with the reason's close too. Drawers closed together are the same, a pick more for each and a reason for each drawer the store asks one of, every one or none.",
    steps: [
      { route: "/shift", action: "pickDrawer" },
      { route: "/shift", action: "closeDrawers" },
      { route: "/shift", action: "drawerVarianceReason" },
    ],
    outcome: { route: "/shift", mark: "drawers-closed" },
    unreplayable: DRAWERS_REPLAYED_APART,
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
