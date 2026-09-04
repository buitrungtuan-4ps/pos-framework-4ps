// The console's step budget (roadmap-v3 **Q7**, the dashboard half).
//
// `ui/scripts/step-budget.mjs` measures the till's selling flows against `docs/ui-ux.md` §6. The
// console had no equivalent, so nothing measured the flows an *operator* runs from the office — and
// the one that matters most is changing a price: it is done under time pressure, it is done wrong
// expensively, and it has a second step (**Publish**) that is easy to forget. A price changed in the
// office and never published leaves the till charging the old one, and nothing on either screen says
// so.
//
// # This gate measures; it does not yet rule
//
// §6's two-and-three-tap rule is about a till in service, and applying it unchanged to a back-office
// console would be a number picked to look strict rather than one anybody had argued for. So a task
// here declares `budget: null` until a ceiling is *decided*, and the script:
//
//  * **fails** when a declared tap cannot be resolved against the source — the flow was renamed,
//    moved or deleted and this file did not follow. That is the drift the ui/ gate catches, and it is
//    worth catching here from the first commit.
//  * **fails** when a task with a real budget exceeds it.
//  * **reports** the measured cost of every task, so the ceiling can be set from evidence.
//
// Setting a ceiling is then a one-line change per task. Until then this file's honest description is
// *a verified map of the console's core flows, with their measured cost*.
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

// The console's core flows. `budget` is `null` where no ceiling has been decided yet — see the note
// at the top of this file; `steps` is what the flow costs today, resolved against the source.
const TASKS = [
  {
    task: "Change an item's price on a menu and publish it to the store",
    budget: null,
    note: "The flow Q7 exists to measure. The last two steps are the operational risk: a price saved and not published leaves the till charging the old one, and neither screen says so. Any proposal to shorten this should shorten the *first* six, not merge the publish into the save.",
    steps: [
      { nav: "catalog" },
      { file: "screens/catalog/CatalogShell.tsx", action: "setTab" },
      { file: "screens/catalog/Menus.tsx", action: "openMenuDetail" },
      { file: "screens/catalog/Menus.tsx", action: "openEditPlacement" },
      { file: "screens/catalog/Menus.tsx", action: "setChannelAmount" },
      { file: "screens/catalog/Menus.tsx", action: "savePlacement" },
      { file: "screens/catalog/Menus.tsx", action: "setPublishMenu" },
      { file: "screens/catalog/Menus.tsx", action: "doPublish" },
    ],
  },
  {
    task: "Check whether a shop is online",
    budget: null,
    note: "One click, because the store overview is the tenant-scoped index (ADR-0099). It was five screens before that, which is the whole argument for the hub.",
    steps: [{ nav: "storeHub" }],
  },
  {
    task: "Provision a new store and get its installer",
    budget: null,
    note: "Five, and none of them is removable: the wizard is not in the sidebar (this gate caught that), so it is reached through Stores, and inside it the store must exist before a key can be scoped to it and the key must exist before the installer can embed it. The wizard's three steps are the dependency order, not a form split for looks.",
    steps: [
      { nav: "stores" },
      { link: { from: "stores", to: "newStore" } },
      { screen: "newStore", action: "createStore" },
      { screen: "newStore", action: "issueKey" },
      { screen: "newStore", action: "downloadInstaller" },
    ],
  },
  {
    task: "Acknowledge a firing alert",
    budget: null,
    note: "Two. Acknowledging from the list rather than from a detail drawer is what keeps it at two — the drawer offers the same action for someone who opened it to read the detail first.",
    steps: [{ nav: "alerts" }, { screen: "alerts", action: "acknowledge" }],
  },
  {
    task: "Turn a capability off for a store and publish it",
    budget: null,
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
  const handlers = new Set(["onClick", "onSubmit", "onChange", "onInput"]);
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
      `"${task}" takes ${steps.length} clicks but its budget is ${budget} — cut a step, or make the case for raising the budget in docs/ui-ux.md §6`,
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
const ruled = TASKS.filter(({ budget }) => budget !== null).length;
console.log(
  `step-budget: ok — ${TASKS.length} console flows, ${clicks} clicks, every one resolved to a real handler.`,
);
for (const { task, budget, steps } of TASKS) {
  const against = budget === null ? "(no ceiling decided yet)" : `/${budget}`;
  console.log(`  ${steps.length}${budget === null ? " " : against} ${task} ${budget === null ? against : ""}`.trimEnd());
}
if (ruled === 0) {
  console.log(
    "  No ceiling has been decided for any console flow yet — this run is the measurement Q7 asked for.",
  );
}
