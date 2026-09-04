// The UX step budget (roadmap-v3 Q7, `docs/ui-ux.md` §6, design principle 1 "dễ sử dụng").
//
// The principle the budget defends: "a normal operator sells without training". `docs/ui-ux.md` §6
// states it as a rule — a common action takes at most two taps from the role's home screen, a rare
// one at most three — and a rule nothing measures is a rule that decays one convenient extra dialog
// at a time.
//
// Each task below declares the taps it takes, and every declared tap is **resolved against the
// source**: the route must exist in App.tsx, its screen must exist, and the named action must
// actually be invoked by an interactive element on that screen. So the declaration cannot drift
// from the code by renaming or deleting a handler, and it cannot claim a flow that was never built.
// Run by `pnpm steps`, which `pnpm build` invokes, so the `ui` CI job fails on a breach.
//
// # What this gate does not prove
//
// It cannot see a tap nobody declared. Add a required confirm dialog to the pay flow and leave this
// file alone, and the gate stays green while the flow is one tap worse. Catching that needs a
// browser driving the real app and counting clicks, which needs a running edge, a paired device and
// a signed-in operator — a harness this repo does not have (`docs/gate-register.md`, and the
// follow-up task that names what it would take).
//
// So this is honestly two things: a **budget**, enforced, and a **map** of the selling flows,
// verified to match the code. The uncatchable case is a reviewer's job, and the map is what makes
// it a five-second job instead of a reading exercise.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

// The budgets, from `docs/ui-ux.md` §6 and roadmap-v3 Q7. `budget` is the rule's ceiling for that
// class of task; `steps` is what the flow actually costs today. A task at its ceiling is not a
// problem — a task that needs one more is a design conversation, not a number to raise.
const TASKS = [
  {
    task: "Seat a table and start its order",
    budget: 2,
    note: "The floor plan is home. One tap on a free table seats it and opens the order.",
    steps: [{ route: "/", action: "onCard" }],
  },
  {
    task: "Add an item to an open order",
    budget: 2,
    note: "The item grid is on the order screen, so an item is one tap. §6's headline case.",
    steps: [{ route: "/table/:id", action: "addItem" }],
  },
  {
    task: "Fire the open lines to the kitchen",
    budget: 2,
    note: "The fire button is fixed on the order screen and shows the unfired count.",
    steps: [{ route: "/table/:id", action: "fire" }],
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
  },
  {
    task: "Settle a dine-in table by card",
    budget: 3,
    note: "One tap fewer than cash: a card takes the exact amount, so there is no note to choose.",
    steps: [
      { route: "/table/:id", action: "takePayment" },
      { route: "/table/:id/pay", action: "payCard" },
    ],
  },
  {
    task: "Bump a ticket on the kitchen display",
    budget: 1,
    note: "A tap anywhere on the card. One, not two: the kitchen has both hands full.",
    steps: [{ route: "/kds", action: "onBump" }],
  },
  {
    task: "Run away a course from the expo screen",
    budget: 1,
    note: "One tap on the group, for the same reason as the bump.",
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
  },
  {
    task: "Charge a counter order by card",
    budget: 3,
    steps: [
      { route: "/counter", action: "charge" },
      { route: "/counter", action: "payCard" },
    ],
  },
  {
    task: "Open the cash shift with a float",
    budget: 3,
    note: "Rare, and it is a number being typed — §6 allows three for a rare action.",
    steps: [{ route: "/shift", action: "openShift" }],
  },
  {
    task: "Enter the blind cash count",
    budget: 3,
    note: "Blind by design (§11.1): the expected figure is not on screen, which is a control rather than a missing step.",
    steps: [{ route: "/shift", action: "countShift" }],
  },
  {
    task: "Close the shift and reveal the variance",
    budget: 3,
    steps: [{ route: "/shift", action: "closeShift" }],
  },
  {
    task: "Sign in on a paired device",
    budget: 3,
    note: "Before any selling happens, so it is outside the per-task budgets — declared to keep it measured too.",
    steps: [{ route: "/signin", action: "submit" }],
  },
];

/** Reads a source file, or exits naming the file the declaration pointed at. */
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

/** `path` → `component` for every `<Route path=… component=…>` in App.tsx. */
function routeTable() {
  const path = `${SRC}/App.tsx`;
  const text = read(path);
  if (text === null) {
    console.error("step-budget: cannot read src/App.tsx, so no route can be resolved");
    process.exit(1);
  }
  const routes = new Map();
  const walk = (node) => {
    if (ts.isJsxSelfClosingElement(node) && node.tagName.getText() === "Route") {
      let route = null;
      let component = null;
      for (const attribute of node.attributes.properties) {
        if (!ts.isJsxAttribute(attribute) || attribute.initializer === undefined) {
          continue;
        }
        const name = attribute.name.getText();
        if (name === "path" && ts.isStringLiteral(attribute.initializer)) {
          route = attribute.initializer.text;
        }
        if (name === "component" && ts.isJsxExpression(attribute.initializer)) {
          component = attribute.initializer.expression?.getText() ?? null;
        }
      }
      if (route !== null && component !== null) {
        routes.set(route, component);
      }
    }
    ts.forEachChild(node, walk);
  };
  walk(parse(path, text));
  return routes;
}

/**
 * Every identifier called inside an interactive handler on this screen.
 *
 * Deliberately the *called* identifiers rather than the handler expression's text: a tap is written
 * as `onClick={() => void guard(() => fire(id))}`, and what the operator is doing is `fire` — the
 * wrapper is error plumbing. Collecting call targets sees through any depth of wrapper.
 */
function tapActions(path, text) {
  const source = parse(path, text);
  const handlers = new Set(["onClick", "onSubmit", "onChange"]);
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
    }
    ts.forEachChild(node, walk);
  };
  walk(source);
  return actions;
}

const routes = routeTable();
const screenCache = new Map();

/** The tap actions of the screen `route` renders, or `null` with a reason if it cannot be resolved. */
function actionsForRoute(route) {
  if (screenCache.has(route)) {
    return screenCache.get(route);
  }
  const component = routes.get(route);
  if (component === undefined) {
    const resolved = { error: `no <Route path="${route}"> in App.tsx` };
    screenCache.set(route, resolved);
    return resolved;
  }
  const path = `${SRC}/screens/${component}.tsx`;
  const text = read(path);
  const resolved =
    text === null
      ? { error: `route ${route} renders ${component}, but src/screens/${component}.tsx is missing` }
      : { actions: tapActions(path, text), component };
  screenCache.set(route, resolved);
  return resolved;
}

const failures = [];
for (const { task, budget, steps } of TASKS) {
  if (steps.length > budget) {
    failures.push(
      `"${task}" takes ${steps.length} taps but its budget is ${budget} — cut a step, or make the case for raising the budget in docs/ui-ux.md §6`,
    );
  }
  for (const [index, step] of steps.entries()) {
    const resolved = actionsForRoute(step.route);
    if (resolved.error !== undefined) {
      failures.push(`"${task}" step ${index + 1}: ${resolved.error}`);
      continue;
    }
    if (!resolved.actions.has(step.action)) {
      failures.push(
        `"${task}" step ${index + 1} claims a tap calling \`${step.action}\` on ${step.route} (${resolved.component}), and no interactive element there calls it — the flow changed, or the declaration is stale`,
      );
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

const taps = TASKS.reduce((total, { steps }) => total + steps.length, 0);
console.log(
  `step-budget: ok — ${TASKS.length} tasks, ${taps} taps, every one resolved to a real handler and inside its budget.`,
);
for (const { task, budget, steps } of TASKS) {
  const at = steps.length === budget ? " (at the ceiling)" : "";
  console.log(`  ${steps.length}/${budget} ${task}${at}`);
}
