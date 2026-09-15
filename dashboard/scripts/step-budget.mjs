// The console's step budget (roadmap-v3 **Q7**, the dashboard half).
//
// `ui/scripts/step-budget.mjs` measures the till's selling flows against `docs/ui-ux.md` §6. The
// console had no equivalent, so nothing measured the flows an *operator* runs from the office — and
// the one that matters most is changing a price: it is done under time pressure, it is done wrong
// expensively, and it has a second step (**Publish**) that is easy to forget. A price changed in the
// office and never published leaves the till charging the old one, and nothing on either screen says
// so.
//
// # This gate rules
//
// It did not, at first. §6's two-and-three-tap rule is about a till in service, and applying it
// unchanged to a back-office console would have been a number picked to look strict rather than one
// anybody had argued for — so every task declared `budget: null`, the script printed the measured
// cost, and the ceilings were left to be set from that evidence. They are set now (roadmap-v3
// **D7**), and the script:
//
//  * **fails** when a declared tap cannot be resolved against the source — the flow was renamed,
//    moved or deleted and this file did not follow. That is the drift the ui/ gate catches, and it is
//    worth catching here from the first commit.
//  * **fails** when a task exceeds its budget.
//
// # Where the numbers come from, and where they part company with D7
//
// D7 expected **4 / 6 / 3** (create / publish / find). Two flows do not fit under those, and the
// reason is written into their notes below rather than discovered again later:
//
//  * **Provisioning is 5, not 4.** The wizard's three steps are a dependency order — the store must
//    exist before a key can be scoped to it, and the key must exist before the installer can embed
//    it. The only way to reach 4 is to stop scoping the key to the store.
//  * **The price change is 7, not 6.** Six of the seven are catalog navigation and the edit itself;
//    the seventh is **Publish**, which is the whole reason this flow is measured. Dropping to 6 means
//    publishing on save, which is the safeguard, not the overhead. (It was 8 until the publish card
//    started following the menu you opened — that was the removable tap, and it is gone.)
//
// So the ceilings here are each flow's **measured cost**, which makes this a no-regression ratchet:
// nobody can add a tap to a core flow without turning the build red and having to argue the new
// number into this file. That is D7's stated purpose — "daily work cannot get slower by degrees
// without anyone noticing" — and it is the half a round 4/6/3 would have bought at the price of
// being unsatisfiable without undoing a safeguard. Where a flow already sits at or under D7's
// number, the ratchet *is* D7's number.
//
// # What a "tap" is here
//
// Three kinds, because the console is not a till:
//
//  * `{ nav: "<ScreenId>" }` — clicking the screen's entry in the sidebar. Resolved by requiring the
//    id to be a real screen **and** to appear in a nav group, so a screen nobody can reach from the
//    nav cannot be counted as one click away. This caught something on its first run: the new-store
//    wizard is in `SCREENS` and in **no** nav group, so "open the wizard" is not one click from
//    anywhere — it is reached from the Stores screen, and the declaration below now says so.
//  * `{ link: { from: "<ScreenId>", to: "<ScreenId>" } }` — following an in-app link from one screen
//    to another. Resolved by requiring `from`'s file to actually build a URL for `to` with
//    `screenHref("<to>"`, which is the one way the console makes such a link.
//  * `{ screen: "<ScreenId>", action: "ident" }` — a tap on a routed screen, resolved through
//    `SCREENS` → `COMPONENTS` → the `lazy(() => import(…))` specifier that names its file.
//  * `{ file: "screens/catalog/Menus.tsx", action: "ident" }` — a tap on a component that is not
//    itself a route (a Catalog tab, a shared panel), named by its path under `src/`.
//
// # What this gate does not prove
//
// The same blind spot the till's gate has, and for the same reason: it cannot see a tap nobody
// declared. Add a confirmation dialog to the publish flow and leave this file alone, and the gate
// stays green while the flow is one click worse. Catching that needs a browser driving the real
// console, which needs a running cloud and a signed-in admin — a harness this repo does not have
// (`docs/gate-register.md`).

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

// The console's core flows. `budget` is the decided ceiling and `steps` is what the flow costs
// today, resolved against the source; where the two differ from D7's expected 4 / 6 / 3, the note
// says why — see the top of this file.
const TASKS = [
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
  },
  {
    task: "Check whether a shop is online",
    budget: 1,
    note: "One click, because the store overview is the tenant-scoped index (ADR-0099). It was five screens before that, which is the whole argument for the hub. D7's find ceiling is 3; this sits at 1 and the ceiling holds it there, because the hub is the reason a second click would be a regression rather than a cost.",
    steps: [{ nav: "storeHub" }],
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
  },
  {
    task: "Acknowledge a firing alert",
    budget: 2,
    note: "Two. Acknowledging from the list rather than from a detail drawer is what keeps it at two — the drawer offers the same action for someone who opened it to read the detail first.",
    steps: [{ nav: "alerts" }, { screen: "alerts", action: "acknowledge" }],
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
  },
];

function read(path) {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return null;
  }
}

function parse(path, text) {
  return ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
}

/** Peels `as const` / `satisfies` wrappers off an initializer to reach the object literal inside. */
function unwrap(node) {
  let current = node;
  while (ts.isAsExpression(current) || ts.isSatisfiesExpression(current)) {
    current = current.expression;
  }
  return current;
}

function required(path, what) {
  const text = read(path);
  if (text === null) {
    console.error(`step-budget: cannot read ${what}, so nothing can be resolved`);
    process.exit(1);
  }
  return text;
}

/**
 * The screen ids `SCREENS` declares, and the ids any nav group lists.
 *
 * Parsed rather than imported because this script is plain Node with no TypeScript loader — the same
 * constraint the other three gates in this directory work under, and the reason they are all
 * `typescript`-AST readers rather than importers.
 */
function screenTable() {
  const path = `${SRC}/state/screens.ts`;
  const source = parse(path, required(path, "src/state/screens.ts"));
  const ids = new Set();
  const inNav = new Set();
  const walk = (node) => {
    if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.name.text === "SCREENS" &&
      node.initializer !== undefined
    ) {
      // `SCREENS` is written `{ … } as const satisfies Record<string, Screen>`, so the object literal
      // sits inside two wrappers. Peeling in a loop is what stops a later `satisfies` (or its
      // removal) silently emptying this table — an empty table would make every declared step
      // unresolvable rather than pass, which is the safe direction, but a confusing failure.
      const object = unwrap(node.initializer);
      if (ts.isObjectLiteralExpression(object)) {
        for (const property of object.properties) {
          if (ts.isPropertyAssignment(property)) {
            ids.add(property.name.getText().replace(/^["']|["']$/g, ""));
          }
        }
      }
    }
    ts.forEachChild(node, walk);
  };
  walk(source);

  // The nav ids, read from `NAV_GROUPS`'s own text: every entry is a string literal inside an
  // `items: [...]` array, and reading them positionally is what keeps this independent of how the
  // groups are nested.
  const navSource = source.text;
  const groups = navSource.slice(navSource.indexOf("export const NAV_GROUPS"));
  for (const match of groups.matchAll(/items:\s*\[([^\]]*)\]/g)) {
    for (const id of match[1].matchAll(/"([^"]+)"/g)) {
      inNav.add(id[1]);
    }
  }
  return { ids, inNav };
}

/** `ScreenId` → the file under `src/` that renders it, via `COMPONENTS` and the lazy imports. */
function componentFiles() {
  const path = `${SRC}/App.tsx`;
  const source = parse(path, required(path, "src/App.tsx"));
  const modules = new Map();
  const components = new Map();
  const walk = (node) => {
    if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.initializer !== undefined &&
      node.initializer.getText().startsWith("lazy(")
    ) {
      const specifier = node.initializer.getText().match(/import\("\.\/([^"]+)"\)/);
      if (specifier !== null) {
        modules.set(node.name.text, `${specifier[1]}.tsx`);
      }
    }
    if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.name.text === "COMPONENTS" &&
      node.initializer !== undefined
    ) {
      const object = unwrap(node.initializer);
      if (ts.isObjectLiteralExpression(object)) {
        for (const property of object.properties) {
          if (ts.isPropertyAssignment(property)) {
            components.set(
              property.name.getText().replace(/^["']|["']$/g, ""),
              property.initializer.getText(),
            );
          }
        }
      }
    }
    ts.forEachChild(node, walk);
  };
  walk(source);

  const files = new Map();
  for (const [id, component] of components) {
    const module = modules.get(component);
    if (module !== undefined) {
      files.set(id, module);
    }
  }
  return files;
}

/**
 * Every identifier called inside an interactive handler in this file.
 *
 * The *called* identifiers rather than the handler text: a tap is written
 * `onClick={() => void savePlacement()}`, and what the operator is doing is `savePlacement` — the
 * `void` and the arrow are plumbing. Collecting call targets sees through any depth of wrapper.
 */
function tapActions(path, text) {
  // The four DOM handlers, plus the kit props that *are* taps one indirection away.
  //
  // Every name in the second group is wired to a real `onClick` inside `components/kit.tsx` — the
  // confirm dialog's two buttons, the pager's arrows, the reorder arrows, a tab, the publish bar's
  // button. A screen that hands its handler to one of those has written a tap; the gate could not
  // see it, so adopting a kit component silently un-declared a flow step. That is backwards: the
  // kit exists so screens stop hand-rolling these controls.
  const handlers = new Set([
    "onClick",
    "onSubmit",
    "onChange",
    "onInput",
    "onCancel",
    "onConfirm",
    "onOffset",
    "onPage",
    "onPublish",
    "onReorder",
    "onSelect",
  ]);
  const actions = new Set();
  const collectCalls = (node) => {
    if (ts.isCallExpression(node)) {
      const target = node.expression;
      if (ts.isIdentifier(target)) {
        actions.add(target.text);
      } else if (ts.isPropertyAccessExpression(target)) {
        actions.add(target.name.text);
      }
    }
    ts.forEachChild(node, collectCalls);
  };
  const walk = (node) => {
    if (ts.isJsxAttribute(node) && handlers.has(node.name.getText())) {
      if (node.initializer !== undefined) {
        collectCalls(node.initializer);
      }
      // `onClick={handler}` with no call at all: the identifier itself is the tap.
      if (
        node.initializer !== undefined &&
        ts.isJsxExpression(node.initializer) &&
        node.initializer.expression !== undefined &&
        ts.isIdentifier(node.initializer.expression)
      ) {
        actions.add(node.initializer.expression.text);
      }
    }
    ts.forEachChild(node, walk);
  };
  walk(parse(path, text));
  return actions;
}

const { ids, inNav } = screenTable();
const files = componentFiles();
const actionCache = new Map();

/** The tap actions in `relative` (a path under `src/`), or `null` if the file is missing. */
function actionsInFile(relative) {
  if (actionCache.has(relative)) {
    return actionCache.get(relative);
  }
  const text = read(`${SRC}/${relative}`);
  const resolved = text === null ? null : tapActions(`${SRC}/${relative}`, text);
  actionCache.set(relative, resolved);
  return resolved;
}

/** Resolves one declared step, returning an error string or `null` when it holds. */
function resolveStep(step) {
  if (step.nav !== undefined) {
    if (!ids.has(step.nav)) {
      return `claims a nav click to "${step.nav}", which is not a screen in SCREENS`;
    }
    if (!inNav.has(step.nav)) {
      return `claims a nav click to "${step.nav}", which is in SCREENS but in no NAV_GROUPS entry — it cannot be reached from the sidebar, so it is not one click away`;
    }
    return null;
  }
  if (step.link !== undefined) {
    const { from, to } = step.link;
    for (const id of [from, to]) {
      if (!ids.has(id)) {
        return `claims a link ${from} → ${to}, and "${id}" is not a screen in SCREENS`;
      }
    }
    const source = files.get(from);
    if (source === undefined) {
      return `claims a link from "${from}", which App.tsx's COMPONENTS does not map to a lazy-imported file`;
    }
    const text = read(`${SRC}/${source}`);
    if (text === null) {
      return `claims a link from "${from}", whose file src/${source} does not exist`;
    }
    if (!text.includes(`screenHref("${to}"`)) {
      return `claims a link ${from} → ${to}, and src/${source} builds no URL for it (no \`screenHref("${to}"\`) — the link was removed, or it never existed`;
    }
    return null;
  }
  const relative =
    step.file ??
    (step.screen !== undefined ? files.get(step.screen) : undefined);
  if (step.screen !== undefined && !ids.has(step.screen)) {
    return `names screen "${step.screen}", which is not in SCREENS`;
  }
  if (relative === undefined) {
    return `names screen "${step.screen}", which SCREENS has but App.tsx's COMPONENTS does not map to a lazy-imported file`;
  }
  const actions = actionsInFile(relative);
  if (actions === null) {
    return `points at src/${relative}, which does not exist`;
  }
  if (!actions.has(step.action)) {
    return `claims a tap calling \`${step.action}\` in src/${relative}, and no interactive element there calls it — the flow changed, or the declaration is stale`;
  }
  return null;
}

const failures = [];
for (const { task, budget, steps } of TASKS) {
  if (budget !== null && steps.length > budget) {
    failures.push(
      `"${task}" takes ${steps.length} clicks but its budget is ${budget} — cut a step, or argue the new number into this task's \`note\` (docs/cloud-admin-ux-plan.md D7)`,
    );
  }
  for (const [index, step] of steps.entries()) {
    const failure = resolveStep(step);
    if (failure !== null) {
      failures.push(`"${task}" step ${index + 1} ${failure}`);
    }
  }
}

if (failures.length > 0) {
  console.error("step-budget: FAILED");
  for (const failure of failures) {
    console.error(`  ${failure}`);
  }
  process.exit(1);
}

const clicks = TASKS.reduce((total, { steps }) => total + steps.length, 0);
const unruled = TASKS.filter(({ budget }) => budget === null);
console.log(
  `step-budget: ok — ${TASKS.length} console flows, ${clicks} clicks, every one resolved to a real handler and inside its ceiling.`,
);
for (const { task, budget, steps } of TASKS) {
  const against = budget === null ? " (no ceiling decided yet)" : `/${budget}`;
  console.log(`  ${steps.length}${budget === null ? "" : against} ${task}${budget === null ? against : ""}`);
}
// `budget: null` stays legal so a flow added tomorrow can be measured before it is ruled — that is
// how these seven got their numbers. It is a state to leave, not to live in, so the run says so.
if (unruled.length > 0) {
  console.log(
    `  ${unruled.length} flow(s) still measure without ruling; decide a ceiling from the count above.`,
  );
}
