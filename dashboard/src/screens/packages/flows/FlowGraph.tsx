// A flow drawn as its graph (ADR-0192 decision 1): one column per depth of prerequisites, so every
// step sits to the right of everything it waits on, and each step says what it waits on in words as
// well, so the graph never depends on reading arrows. A step is picked to edit it.

import { For, Show } from "solid-js";

import type { Flow, FlowStep, PackageRefusal } from "../../../api/schemas";
import { t } from "../../../i18n";
import { columns, OPTIONAL } from "../../../lib/flow-graph";

/** The steps in each column, in the flow's order within it. */
function byColumn(flow: Flow): FlowStep[][] {
  const placed = columns(flow);
  const count = Math.max(0, ...placed.values()) + 1;
  const grid: FlowStep[][] = Array.from({ length: count }, () => []);
  for (const step of flow.steps) {
    grid[placed.get(step.step_id) ?? 0]?.push(step);
  }
  return grid;
}

/** One step's card: its id, what it does, its kind, and what it waits on. */
function StepCard(props: { step: FlowStep; picked: boolean; refused: boolean; onPick: () => void }) {
  const does = () => props.step.screen_id ?? props.step.action_id ?? "";
  return (
    <button
      type="button"
      class={`flex w-48 flex-col gap-0.5 rounded-token border p-2 text-left text-sm ${
        props.picked ? "border-primary bg-surface-raised" : "border-line"
      } ${props.refused ? "border-danger" : ""}`}
      aria-pressed={props.picked}
      data-flow-card={props.step.step_id}
      onClick={() => props.onPick()}
    >
      <span class="font-medium text-ink">{props.step.step_id}</span>
      <span class="truncate text-ink-muted">{does()}</span>
      <span class="text-xs text-ink-muted">
        {props.step.kind === OPTIONAL ? t("flow.step.optional") : t("flow.step.required")}
        <Show when={props.step.applies_when !== undefined}> · {t("flow.step.conditional")}</Show>
        <Show when={props.step.done_when !== undefined}> · {t("flow.step.edgeGuard")}</Show>
      </span>
      <Show when={(props.step.prerequisites ?? []).length > 0}>
        <span class="text-xs text-ink-muted">
          {t("flow.step.waitsOnList", { steps: (props.step.prerequisites ?? []).join(", ") })}
        </span>
      </Show>
    </button>
  );
}

/** The graph: columns left to right, a step's card picks it. */
export function FlowGraph(props: {
  flow: Flow;
  picked: string | null;
  refusals: readonly PackageRefusal[];
  onPick: (stepId: string) => void;
}) {
  return (
    <ol class="flex gap-4 overflow-x-auto pb-2" aria-label={t("flow.graph", { flow: props.flow.flow_id })}>
      <For each={byColumn(props.flow)}>
        {(column, index) => (
          <li class="flex flex-col gap-2">
            <span class="text-xs text-ink-muted">{t("flow.column", { n: index() + 1 })}</span>
            <For each={column}>
              {(step) => (
                <StepCard
                  step={step}
                  picked={props.picked === step.step_id}
                  refused={props.refusals.some((refusal) => refusal.step_id === step.step_id)}
                  onPick={() => props.onPick(step.step_id)}
                />
              )}
            </For>
          </li>
        )}
      </For>
    </ol>
  );
}
