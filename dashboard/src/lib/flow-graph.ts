// A flow as the console's designer edits it: a graph of steps, each required or optional, perhaps
// conditional, waiting on the steps before it
// ([ADR-0192](../../../docs/adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md) decision 1).
//
// Every edit here returns a new flow and keeps the graph one the cloud can accept: a step's id is
// made from its name in the shape `pos-core` requires, a prerequisite can only be a step that does
// not already wait on the one choosing it (so no edit can make a cycle), and removing or renaming a
// step carries through to every step that waits on it. The cloud still checks the whole package at
// publish, conditions included (`check_flows`); `localProblems` only says early what it would say.
//
// Laid out without a graph library: a step's column is the length of the longest chain of
// prerequisites behind it, so every arrow points to the right.

import type { Flow, FlowStep } from "../api/schemas";
import type { MessageKey } from "../i18n";

/** The most steps one flow holds (`pos_proto::flow::STEPS_MAX`). */
export const STEPS_MAX = 64;

/** The most flows one package holds (`pos_proto::flow::FLOWS_MAX`). */
export const FLOWS_MAX = 32;

/** The longest id (`pos_proto::flow::ID_CHARS_MAX`). */
export const ID_CHARS_MAX = 64;

export const REQUIRED = "STEP_KIND_REQUIRED";
export const OPTIONAL = "STEP_KIND_OPTIONAL";

/** Whether `id` has the shape `pos-core` accepts for a flow or step id: `a`–`z`, `0`–`9`, `_`. */
export const isId = (id: string): boolean => id.length > 0 && id.length <= ID_CHARS_MAX && /^[a-z0-9_]+$/u.test(id);

/**
 * An id made from a name, unique among `taken`: lower case, accents dropped, everything else an
 * underscore, cut to the longest id, and numbered when it is taken. A name with nothing usable in it
 * becomes `step`.
 */
export function idFrom(name: string, taken: Iterable<string>, fallback = "step"): string {
  const used = new Set(taken);
  const base =
    name
      .normalize("NFD")
      .replace(/\p{Mn}/gu, "")
      .replace(/đ/gu, "d")
      .replace(/Đ/gu, "d")
      .toLowerCase()
      .replace(/[^a-z0-9]+/gu, "_")
      .replace(/^_+|_+$/gu, "")
      .slice(0, ID_CHARS_MAX - 4) || fallback;
  if (!used.has(base)) {
    return base;
  }
  for (let n = 2; ; n += 1) {
    const candidate = `${base}_${n}`;
    if (!used.has(candidate)) {
      return candidate;
    }
  }
}

/** Every step `stepId` waits on, directly or through others. */
function behind(flow: Flow, stepId: string): Set<string> {
  const byId = new Map(flow.steps.map((step) => [step.step_id, step]));
  const seen = new Set<string>();
  const walk = (id: string) => {
    for (const prerequisite of byId.get(id)?.prerequisites ?? []) {
      if (!seen.has(prerequisite)) {
        seen.add(prerequisite);
        walk(prerequisite);
      }
    }
  };
  walk(stepId);
  return seen;
}

/** Every step that waits on `stepId`, directly or through others. */
export function waitingOn(flow: Flow, stepId: string): Set<string> {
  return new Set(flow.steps.filter((step) => behind(flow, step.step_id).has(stepId)).map((step) => step.step_id));
}

/**
 * The steps `stepId` may wait on: every other step that does not already wait on it, in the flow's
 * order. Choosing only from these can never make a cycle.
 */
export function prerequisiteChoices(flow: Flow, stepId: string): readonly string[] {
  const after = waitingOn(flow, stepId);
  return flow.steps.map((step) => step.step_id).filter((id) => id !== stepId && !after.has(id));
}

/** What a new step does: show a screen or run an action. */
export type StepTarget = { readonly screen_id: string } | { readonly action_id: string };

/** The flow with a new required step named `name` at its end, waiting on `prerequisites`. */
export function addStep(flow: Flow, name: string, target: StepTarget, prerequisites: readonly string[] = []): Flow {
  const step: FlowStep = {
    step_id: idFrom(name, flow.steps.map((existing) => existing.step_id)),
    kind: REQUIRED,
    prerequisites: prerequisites.filter((id) => flow.steps.some((existing) => existing.step_id === id)),
    ...target,
  };
  return { ...flow, steps: [...flow.steps, step] };
}

/** The flow with `stepId` changed by `change`; its id is changed only through `renameStep`. */
export function updateStep(flow: Flow, stepId: string, change: Partial<Omit<FlowStep, "step_id">>): Flow {
  return {
    ...flow,
    steps: flow.steps.map((step) => {
      if (step.step_id !== stepId) {
        return step;
      }
      const next: FlowStep = { ...step, ...change };
      // A condition cleared is a field left off, as the wire has it.
      for (const key of ["applies_when", "done_when"] as const) {
        if (next[key] === undefined || next[key] === "") {
          delete next[key];
        }
      }
      return next;
    }),
  };
}

/** The flow with `stepId` waiting on `prerequisites`, keeping only those that make no cycle. */
export function setPrerequisites(flow: Flow, stepId: string, prerequisites: readonly string[]): Flow {
  const allowed = new Set(prerequisiteChoices(flow, stepId));
  return updateStep(flow, stepId, { prerequisites: prerequisites.filter((id) => allowed.has(id)) });
}

/** The flow without `stepId`, and with no step still waiting on it. */
export function removeStep(flow: Flow, stepId: string): Flow {
  return {
    ...flow,
    steps: flow.steps
      .filter((step) => step.step_id !== stepId)
      .map((step) =>
        step.prerequisites?.includes(stepId) === true
          ? { ...step, prerequisites: step.prerequisites.filter((id) => id !== stepId) }
          : step,
      ),
  };
}

/** The flow with `stepId` called `nextId` everywhere; unchanged if `nextId` is malformed or taken. */
export function renameStep(flow: Flow, stepId: string, nextId: string): Flow {
  if (!isId(nextId) || flow.steps.some((step) => step.step_id === nextId)) {
    return flow;
  }
  return {
    ...flow,
    steps: flow.steps.map((step) => ({
      ...step,
      step_id: step.step_id === stepId ? nextId : step.step_id,
      ...(step.prerequisites === undefined
        ? {}
        : { prerequisites: step.prerequisites.map((id) => (id === stepId ? nextId : id)) }),
      ...(step.offline?.step_id === stepId ? { offline: { step_id: nextId } } : {}),
    })),
  };
}

/** The flow with `stepId` moved one place earlier (`-1`) or later (`1`) in the till's order. */
export function moveStep(flow: Flow, stepId: string, by: -1 | 1): Flow {
  const index = flow.steps.findIndex((step) => step.step_id === stepId);
  const target = index + by;
  if (index < 0 || target < 0 || target >= flow.steps.length) {
    return flow;
  }
  const steps = [...flow.steps];
  const [moved] = steps.splice(index, 1);
  steps.splice(target, 0, moved as FlowStep);
  return { ...flow, steps };
}

/**
 * Each step's column: 0 for a step that waits on nothing, otherwise one more than the furthest of
 * its prerequisites. A step caught in a cycle (which only a hand-written flow can hold) is put after
 * every other.
 */
export function columns(flow: Flow): ReadonlyMap<string, number> {
  const byId = new Map(flow.steps.map((step) => [step.step_id, step]));
  const placed = new Map<string, number>();
  const visiting = new Set<string>();
  const place = (id: string): number => {
    const known = placed.get(id);
    if (known !== undefined) {
      return known;
    }
    if (visiting.has(id)) {
      return Number.POSITIVE_INFINITY;
    }
    visiting.add(id);
    const prerequisites = (byId.get(id)?.prerequisites ?? []).filter((prerequisite) => byId.has(prerequisite));
    const column = prerequisites.length === 0 ? 0 : Math.max(...prerequisites.map(place)) + 1;
    visiting.delete(id);
    placed.set(id, column);
    return column;
  };
  flow.steps.forEach((step) => place(step.step_id));
  const finite = [...placed.values()].filter(Number.isFinite);
  const last = finite.length === 0 ? 0 : Math.max(...finite) + 1;
  return new Map([...placed].map(([id, column]) => [id, Number.isFinite(column) ? column : last]));
}

/** The arrows to draw: from each prerequisite to the step that waits on it. */
export function arrows(flow: Flow): readonly { readonly from: string; readonly to: string }[] {
  const ids = new Set(flow.steps.map((step) => step.step_id));
  return flow.steps.flatMap((step) =>
    (step.prerequisites ?? []).filter((id) => ids.has(id)).map((from) => ({ from, to: step.step_id })),
  );
}

/** One thing the cloud would refuse, said early: an i18n key and the step it is about, if any. */
export type FlowProblem = { readonly key: MessageKey; readonly stepId?: string };

/** What the cloud's `check_flows` would refuse in `flow` that the designer can see without it. */
export function localProblems(flow: Flow): readonly FlowProblem[] {
  const problems: FlowProblem[] = [];
  if (!isId(flow.flow_id)) {
    problems.push({ key: "flow.problem.flowId" });
  }
  if (flow.start_action_id === "") {
    problems.push({ key: "flow.problem.noStart" });
  }
  if (flow.steps.length === 0 || flow.steps.length > STEPS_MAX) {
    problems.push({ key: "flow.problem.stepCount" });
  }
  const ids = new Set<string>();
  for (const step of flow.steps) {
    if (!isId(step.step_id) || ids.has(step.step_id)) {
      problems.push({ key: "flow.problem.stepId", stepId: step.step_id });
    }
    ids.add(step.step_id);
    if ((step.screen_id === undefined) === (step.action_id === undefined)) {
      problems.push({ key: "flow.problem.oneThing", stepId: step.step_id });
    }
  }
  const cycle = flow.steps.filter((step) => behind(flow, step.step_id).has(step.step_id));
  for (const step of cycle) {
    problems.push({ key: "flow.problem.cycle", stepId: step.step_id });
  }
  for (const step of flow.steps) {
    for (const id of step.prerequisites ?? []) {
      if (!ids.has(id) || id === step.step_id) {
        problems.push({ key: "flow.problem.prerequisite", stepId: step.step_id });
      }
    }
  }
  return problems;
}
