// The UX step budget (roadmap-v3 Q7, `docs/ui-ux.md` §6, design principle 1 "dễ sử dụng").
//
// The principle the budget defends: "a normal operator sells without training". `docs/ui-ux.md` §6
// states it as a rule — a common action takes at most two taps from the role's home screen, a rare
// one at most three — and a rule nothing measures is a rule that decays one convenient extra dialog
// at a time.
//
// Each task in `step-tasks.mjs` declares the taps it takes, and every declared tap is **resolved
// against the source**: the route must exist in App.tsx, its screen must exist, the named action
// must actually be invoked by an interactive element on that screen, and that element must carry
// `data-step="<action>"`. The task's outcome is resolved the same way — its screen must carry a
// `data-outcome="<mark>"` element for the browser gate to wait on. So the declaration cannot drift
// from the code by renaming or deleting a handler, and it cannot claim a flow that was never built.
// Run by `pnpm steps`, which `pnpm build` invokes, so the `ui` CI job fails on a breach.
//
// # What this gate does not prove, and who does
//
// It cannot see a tap nobody declared. Add a required confirm dialog to the pay flow and leave the
// declaration alone, and this script stays green while the flow is one tap worse — a question about
// the rendered page, not about the syntax tree, so no amount of static resolution closes it
// ([ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md)).
//
// `tests/replay.spec.mjs` closes it, by clicking the same declared taps in a browser against a real
// edge and asserting the flow still reaches its outcome. The two gates lock together and neither is
// sufficient alone: this one requires each resolved element to carry `data-step="<action>"`, so an
// attribute cannot name a handler that does not exist; the browser one finds the element by that
// attribute, so an attribute cannot point at something unreachable. This script is also the faster
// half — it needs no browser and catches a rename the harness would only report as a missing
// element.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import ts from "typescript";

// The one declaration both gates read. Shared rather than duplicated: a flow that grows a step says
// so in a single place, and the browser harness cannot drift from the numbers this script enforces.
import { TASKS } from "./step-tasks.mjs";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

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

/**
 * The first parse error in a source file, or `null`.
 *
 * A file that does not parse yields an empty tree, so every lookup against it comes back "nothing
 * here" and the failure reads as a missing attribute rather than as broken syntax. Naming the real
 * cause costs one line and saves the wrong bug being chased — this script hit it on its own change.
 */
function parseError(source) {
  const [first] = source.parseDiagnostics ?? [];
  if (first === undefined) {
    return null;
  }
  return ts.flattenDiagnosticMessageText(first.messageText, " ");
}

/** `path` → `component` for every `<Route path=… component=…>` in App.tsx. */
function routeTable() {
  const path = `${SRC}/App.tsx`;
  const text = read(path);
  if (text === null) {
    console.error("step-budget: cannot read src/App.tsx, so no route can be resolved");
    process.exit(1);
  }
  const source = parse(path, text);
  const broken = parseError(source);
  if (broken !== null) {
    console.error(`step-budget: src/App.tsx does not parse (${broken}), so no route can be resolved`);
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
  walk(source);
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
  const broken = parseError(source);
  if (broken !== null) {
    return { actions: new Set(), marked: new Set(), outcomes: new Set(), parseError: broken };
  }
  const handlers = new Set(["onClick", "onSubmit", "onChange"]);
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
  // Per *element*, not per file: the `data-step` and the handler have to be on the same tag, or the
  // attribute could name an action some other button on the screen happens to call
  // ([ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md)).
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
      }
      if (name === "data-step" && ts.isStringLiteral(attribute.initializer)) {
        step = attribute.initializer.text;
      }
      // An outcome mark carries no handler and needs none: it names a thing that becomes visible
      // when a flow has succeeded, which is what the browser gate waits on. Collected per file
      // rather than per element, because nothing about it has to sit on the tap.
      if (name === "data-outcome" && ts.isStringLiteral(attribute.initializer)) {
        outcomes.add(attribute.initializer.text);
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
    if (ts.isJsxSelfClosingElement(node) || ts.isJsxOpeningElement(node)) {
      visitElement(node.attributes);
    }
    ts.forEachChild(node, walk);
  };
  walk(source);
  return { actions, marked, outcomes };
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
      : { ...tapActions(path, text), component };
  if (resolved.parseError !== undefined && resolved.parseError !== null) {
    screenCache.set(route, {
      error: `route ${route} renders ${component}, and src/screens/${component}.tsx does not parse (${resolved.parseError}) — fix the syntax; nothing on that screen can be resolved until it does`,
    });
    return screenCache.get(route);
  }
  screenCache.set(route, resolved);
  return resolved;
}

const failures = [];
for (const { task, budget, steps, outcome } of TASKS) {
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
      continue;
    }
    // The element the browser harness will click has to be findable. Without this the two gates
    // drift apart: the analyser stays green on a flow the harness cannot walk, which is the
    // failure ADR-0109 exists to prevent.
    if (!resolved.marked.has(step.action)) {
      failures.push(
        `"${task}" step ${index + 1} calls \`${step.action}\` on ${step.route} (${resolved.component}), but no element there carries \`data-step="${step.action}"\` — add it to the element with that handler, or the browser step gate cannot find the tap`,
      );
    }
  }
  // A task with no stated outcome is a task the browser gate can only walk, not judge: it would
  // click the declared taps and assert nothing, which is the blind spot with extra steps.
  if (outcome === undefined) {
    failures.push(
      `"${task}" declares no outcome — say where the flow ends, as { route, mark }, or the replay proves only that the taps exist`,
    );
    continue;
  }
  const landing = actionsForRoute(outcome.route);
  if (landing.error !== undefined) {
    failures.push(`"${task}" outcome: ${landing.error}`);
    continue;
  }
  if (!landing.outcomes.has(outcome.mark)) {
    failures.push(
      `"${task}" ends at \`data-outcome="${outcome.mark}"\` on ${outcome.route} (${landing.component}), and no element there carries it — mark whatever appears once the flow has succeeded, or the browser gate has nothing to wait for`,
    );
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
