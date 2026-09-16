// The console's core flows, declared once and read by both halves of the step gate.
//
// `scripts/step-budget.mjs` resolves every step against the source — the screen exists, the nav
// entry exists, the named handler is really called by an interactive element, and that element
// carries `data-step="<action>"`. `tests/replay.spec.mjs` then clicks those same elements in a real
// browser against a real `pos_cloud` and asserts the flow reaches its `outcome`. One declaration
// rather than two, for the reason the till's copy gives: a flow that grows a step says so in a
// single place, and the browser harness cannot drift from the numbers the static gate enforces.
//
// # The fields
//
//  * `task` — what an operator is doing, in their words. The test's name, too.
//  * `budget` — the decided ceiling (D7, or the note's reasoning where D7's number does not fit).
//  * `steps` — the clicks, in order. Four shapes; `step-budget.mjs` documents each.
//  * `outcome` — `{ screen | file, mark }`: what becomes true when the flow has worked. The screen
//    must carry a `data-outcome="<mark>"` element, and the browser gate waits for it. A flow with
//    no outcome is one the browser could only walk, asserting nothing — the blind spot with extra
//    steps — so the gate requires one.
//  * `unreplayable` — why the browser half skips this flow. A sentence, not a flag: the replay's
//    last test asserts the skipped set is exactly the declared one, so coverage cannot shrink one
//    quiet flow at a time.

export const TASKS = [
  {
    task: "Change an item's price on a menu and publish it to the store",
    budget: 7,
    note: "The flow Q7 exists to measure, and D7's publish ceiling of 6 does not fit it — see the top of this file. The last step is the operational risk: a price saved and not published leaves the till charging the old one, and neither screen says so. Any proposal to shorten this should shorten the *first* six, not merge the publish into the save. It was eight until `openMenuDetail` started selecting the opened menu in the publish card, which removed the one tap that was asking the operator to say twice which menu they were working on.",
    steps: [
      { nav: "catalog" },
      { file: "screens/catalog/CatalogShell.tsx", action: "setTab" },
      { file: "screens/catalog/Menus.tsx", action: "openMenuDetail" },
      { file: "screens/catalog/Menus.tsx", action: "openEditPlacement" },
      { file: "screens/catalog/Menus.tsx", action: "setChannelAmount" },
      { file: "screens/catalog/Menus.tsx", action: "savePlacement" },
      { file: "screens/catalog/Menus.tsx", action: "doPublish" },
    ],
    outcome: { file: "screens/catalog/Menus.tsx", mark: "menu-published" },
  },
  {
    task: "Check whether a shop is online",
    budget: 1,
    note: "One click, because the store overview is the tenant-scoped index (ADR-0099). It was five screens before that, which is the whole argument for the hub. D7's find ceiling is 3; this sits at 1 and the ceiling holds it there, because the hub is the reason a second click would be a regression rather than a cost.",
    steps: [{ nav: "storeHub" }],
    outcome: { screen: "storeHub", mark: "store-liveness" },
  },
  {
    task: "Provision a new store and get its installer",
    budget: 5,
    note: "Five, and none of them is removable: the wizard is not in the sidebar (this gate caught that), so it is reached through Stores, and inside it the store must exist before a key can be scoped to it and the key must exist before the installer can embed it. The wizard's three steps are the dependency order, not a form split for looks. That is the one flow D7's create ceiling of 4 cannot hold without unscoping the key from its store, which is why the ceiling here is 5.",
    steps: [
      { nav: "stores" },
      { link: { from: "stores", to: "newStore" } },
      { screen: "newStore", action: "createStore" },
      { screen: "newStore", action: "issueKey" },
      { screen: "newStore", action: "downloadInstaller" },
    ],
    outcome: { screen: "newStore", mark: "installer-ready" },
  },
  {
    task: "Replace a store's machine and get its files back",
    budget: 4,
    note: "Four, and the middle two are the point rather than overhead: the drawer opens without writing anything (so reading what a dead machine costs cannot mint a credential), and the key is issued deliberately, because it is a new secret and the old one keeps working until somebody revokes it. It was unbounded before this flow existed — the only way to get an installer for an existing store was to create a second store — so the honest comparison is not four against three, it is four against reading a generator's source.",
    steps: [
      { nav: "stores" },
      { screen: "stores", action: "openHandoff" },
      { screen: "stores", action: "issueHandoffKey" },
      { screen: "stores", action: "downloadHandoff" },
    ],
    outcome: { screen: "stores", mark: "handoff-ready" },
  },
  {
    task: "Publish one menu to a whole cohort of shops",
    budget: 4,
    note: "Four, against 3N for the same change made shop by shop — 150 taps at fifty shops, of which 147 are repetition (ADR-0122). The two pickers in the middle are the instruction itself and are not removable: which cohort, and what to send it. What this number does not show is the half the record is actually about — the fourth tap answers with an outcome per shop, where fifty separate publishes answered fifty times and nobody counted.",
    steps: [
      { nav: "storeGroups" },
      { screen: "storeGroups", action: "setTarget" },
      { screen: "storeGroups", action: "setNode" },
      { screen: "storeGroups", action: "publish" },
    ],
    outcome: { screen: "storeGroups", mark: "batch-report" },
  },
  {
    task: "Acknowledge a firing alert",
    budget: 2,
    note: "Two. Acknowledging from the list rather than from a detail drawer is what keeps it at two — the drawer offers the same action for someone who opened it to read the detail first.",
    steps: [{ nav: "alerts" }, { screen: "alerts", action: "acknowledge" }],
    outcome: { screen: "alerts", mark: "alert-acknowledged" },
    unreplayable:
      "an alert has to be *firing*, and the evaluator raises one from a real fleet symptom — a store gone quiet, a background task stalled, a JetStream near its cap. Seeding one means either waiting for a threshold to trip or writing a row the console can act on, which is a fixture that asserts the alert table rather than the flow. Left to the static half until the evaluator can be driven directly.",
  },
  {
    task: "Turn a capability off for a store and publish it",
    budget: 3,
    note: "Three. Same shape as the price flow and the same risk: the change is authored and then published, and only the published half reaches the till.",
    steps: [
      { nav: "config" },
      { screen: "config", action: "applyPreset" },
      { screen: "config", action: "publishCapabilities" },
    ],
    outcome: { screen: "config", mark: "capabilities-published" },
  },
];
