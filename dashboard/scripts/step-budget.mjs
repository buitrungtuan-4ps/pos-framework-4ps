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
// # What this gate does not prove, and who does
//
// It cannot see a click nobody declared. Add a confirmation dialog to the publish flow and leave
// the declaration alone, and this script stays green while the flow is one click worse — a question
// about the rendered page, not about the syntax tree
// ([ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md)).
//
// `tests/replay.spec.mjs` closes it, by clicking the same declared steps in a browser against a
// real `pos_cloud` and asserting the flow still reaches its outcome. The two gates lock together
// and neither is sufficient alone: this one requires each resolved element to carry
// `data-step="<action>"`, so an attribute cannot name a handler that does not exist; the browser
// one finds the element by that attribute, so an attribute cannot point at something unreachable.
// This script is also the faster half — it needs no browser, no Postgres and no signed-in admin,
// and it catches a rename the harness would only report as a missing element.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import ts from "typescript";

// The one declaration both halves of the gate read (`tests/replay.spec.mjs` is the other). Shared
// rather than duplicated: a flow that grows a click says so in a single place, and the browser
// harness cannot drift from the numbers enforced here.
import { TASKS } from "./step-tasks.mjs";

const SRC = fileURLToPath(new URL("../src", import.meta.url));


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
 * What one file offers the two gates: the actions its interactive elements call, the ones whose
 * element also carries `data-step="<action>"`, and the `data-outcome` marks it can render.
 *
 * The *called* identifiers rather than the handler text: a tap is written
 * `onClick={() => void savePlacement()}`, and what the operator is doing is `savePlacement` — the
 * `void` and the arrow are plumbing. Collecting call targets sees through any depth of wrapper.
 *
 * `marked` is the half that makes a declared click findable in a browser. The attribute has to sit
 * on the same element as the handler, and name the same action, so it cannot drift into pointing at
 * a control that does something else — the lock `tests/replay.spec.mjs` closes from the other side.
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
  const marked = new Set();
  const outcomes = new Set();
  const collectCalls = (node, into) => {
    if (ts.isCallExpression(node)) {
      const target = node.expression;
      if (ts.isIdentifier(target)) {
        into.add(target.text);
      } else if (ts.isPropertyAccessExpression(target)) {
        into.add(target.name.text);
      }
    }
    ts.forEachChild(node, (child) => collectCalls(child, into));
  };
  const visitElement = (attributes) => {
    const called = new Set();
    let step = null;
    for (const attribute of attributes.properties) {
      if (!ts.isJsxAttribute(attribute) || attribute.initializer === undefined) {
        continue;
      }
      const name = attribute.name.getText();
      if (handlers.has(name)) {
        collectCalls(attribute.initializer, called);
        // `onClick={handler}` with no call at all: the identifier itself is the tap.
        if (
          ts.isJsxExpression(attribute.initializer) &&
          attribute.initializer.expression !== undefined &&
          ts.isIdentifier(attribute.initializer.expression)
        ) {
          called.add(attribute.initializer.expression.text);
        }
      }
      if (name === '"data-step"' || name === "data-step") {
        const value = attribute.initializer;
        if (ts.isStringLiteral(value)) {
          step = value.text;
        }
      }
      // An outcome mark carries no handler and needs none: it names something that becomes visible
      // once a flow has succeeded, which is what the browser gate waits on. Collected per file
      // rather than per element, because nothing about it has to sit on a control.
      if (name === '"data-outcome"' || name === "data-outcome") {
        const value = attribute.initializer;
        if (ts.isStringLiteral(value)) {
          outcomes.add(value.text);
        }
      }
    }
    for (const action of called) {
      actions.add(action);
    }
    // The attribute counts only when the element it sits on really calls what it names. That is the
    // half of the lock the browser harness cannot check for itself.
    if (step !== null && called.has(step)) {
      marked.add(step);
    }
  };
  const walk = (node) => {
    if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) {
      visitElement(node.attributes);
    }
    ts.forEachChild(node, walk);
  };
  walk(parse(path, text));
  return { actions, marked, outcomes };
}

const { ids, inNav } = screenTable();
const files = componentFiles();
const actionCache = new Map();

/** What `relative` (a path under `src/`) offers, or `null` if the file is missing. */
function elementsInFile(relative) {
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
  const found = elementsInFile(relative);
  if (found === null) {
    return `points at src/${relative}, which does not exist`;
  }
  if (!found.actions.has(step.action)) {
    return `claims a tap calling \`${step.action}\` in src/${relative}, and no interactive element there calls it — the flow changed, or the declaration is stale`;
  }
  if (!found.marked.has(step.action)) {
    return `claims a tap calling \`${step.action}\` in src/${relative}, and the element that calls it carries no \`data-step="${step.action}"\` — add it, or the browser gate has no way to find the click`;
  }
  return null;
}

/** Resolves a task's outcome mark, returning an error string or `null` when it holds. */
function resolveOutcome(outcome) {
  if (outcome === undefined) {
    return "declares no outcome — say where the flow ends, as { screen | file, mark }, or the replay proves only that the clicks exist";
  }
  const relative =
    outcome.file ?? (outcome.screen !== undefined ? files.get(outcome.screen) : undefined);
  if (outcome.screen !== undefined && !ids.has(outcome.screen)) {
    return `ends on screen "${outcome.screen}", which is not in SCREENS`;
  }
  if (relative === undefined) {
    return `ends on screen "${outcome.screen}", which App.tsx's COMPONENTS does not map to a lazy-imported file`;
  }
  const found = elementsInFile(relative);
  if (found === null) {
    return `ends in src/${relative}, which does not exist`;
  }
  if (!found.outcomes.has(outcome.mark)) {
    return `ends at \`data-outcome="${outcome.mark}"\` in src/${relative}, and nothing there carries it — mark whatever appears once the flow has succeeded, or the browser gate has nothing to wait for`;
  }
  return null;
}

/**
 * The sidebar's own hook, checked once rather than per screen.
 *
 * Every nav entry is one `<A data-nav={id}>` in the shell, so what can rot is that one attribute,
 * not thirty of them. A declared `{ nav: … }` step is a click on it.
 */
function navHook() {
  const text = read(`${SRC}/components/Shell.tsx`);
  if (text === null) {
    return "src/components/Shell.tsx does not exist, so no nav click can be resolved";
  }
  return text.includes("data-nav={id}")
    ? null
    : 'src/components/Shell.tsx no longer puts `data-nav={id}` on its nav entries, so every declared nav click is unfindable in a browser';
}

const failures = [];
const nav = navHook();
if (nav !== null) {
  failures.push(nav);
}
for (const { task, budget, steps, outcome } of TASKS) {
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
  const missing = resolveOutcome(outcome);
  if (missing !== null) {
    failures.push(`"${task}" ${missing}`);
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
const replayed = TASKS.filter(({ unreplayable }) => unreplayable === undefined).length;
console.log(
  `step-budget: ok — ${TASKS.length} console flows, ${clicks} clicks, every one resolved to a marked handler and inside its ceiling; ${replayed} replayable in a browser.`,
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
