// One step of a flow, edited in place (ADR-0192 decision 1): its name, required or optional, what it
// does (a screen or an act), the steps it waits on, when it applies, and when the edge counts it done.
//
// The prerequisites offered are `prerequisiteChoices`: no choice here can make steps wait on each
// other. A step's `done_when` makes it an edge guard, so its condition, and its `applies_when` with
// it, may read only what the edge holds.

import { For, Show } from "solid-js";

import { Button, CheckboxField, SelectField, TextField } from "../../../components/ui";
import type { Flow, FlowStep } from "../../../api/schemas";
import { t } from "../../../i18n";
import { CATALOGUE } from "../../../lib/action-catalogue";
import {
  isId,
  moveStep,
  OPTIONAL,
  prerequisiteChoices,
  removeStep,
  renameStep,
  REQUIRED,
  setPrerequisites,
  updateStep,
} from "../../../lib/flow-graph";
import { ConditionEditor } from "../rules/ConditionEditor";

/** What the step does: show a screen, or run an act from the catalogue. */
function StepTarget(props: { step: FlowStep; onChange: (change: Partial<FlowStep>) => void }) {
  const isScreen = () => props.step.screen_id !== undefined;
  return (
    <div class="grid gap-2 sm:grid-cols-2">
      <SelectField
        label={t("flow.step.does")}
        value={isScreen() ? "screen" : "action"}
        options={[
          { value: "screen", label: t("flow.step.showsScreen") },
          { value: "action", label: t("flow.step.runsAct") },
        ]}
        onChange={(kind) =>
          props.onChange(
            kind === "screen"
              ? { screen_id: props.step.action_id === undefined ? (props.step.screen_id ?? "") : "", action_id: undefined }
              : { action_id: CATALOGUE[0]?.action_id ?? "", screen_id: undefined },
          )
        }
      />
      <Show
        when={isScreen()}
        fallback={
          <SelectField
            label={t("flow.step.act")}
            value={props.step.action_id ?? ""}
            options={CATALOGUE.map((action) => ({ value: action.action_id, label: action.action_id }))}
            onChange={(action_id) => props.onChange({ action_id })}
          />
        }
      >
        <TextField
          label={t("flow.step.screen")}
          hint={t("flow.step.screenHint")}
          value={props.step.screen_id ?? ""}
          onInput={(screen_id) => props.onChange({ screen_id })}
        />
      </Show>
    </div>
  );
}

/** The steps this one waits on, from those that do not already wait on it. */
function Prerequisites(props: { flow: Flow; step: FlowStep; onChange: (flow: Flow) => void }) {
  const chosen = () => new Set(props.step.prerequisites ?? []);
  const toggle = (id: string, on: boolean) => {
    const next = on ? [...chosen(), id] : [...chosen()].filter((other) => other !== id);
    props.onChange(setPrerequisites(props.flow, props.step.step_id, next));
  };
  return (
    <fieldset>
      <legend class="text-sm font-medium text-ink">{t("flow.step.waitsOn")}</legend>
      <Show when={prerequisiteChoices(props.flow, props.step.step_id).length > 0} fallback={<p class="text-sm text-ink-muted">{t("flow.step.nothingToWaitOn")}</p>}>
        <div class="flex flex-wrap gap-x-4">
          <For each={prerequisiteChoices(props.flow, props.step.step_id)}>
            {(id) => <CheckboxField label={id} checked={chosen().has(id)} onChange={(on) => toggle(id, on)} />}
          </For>
        </div>
      </Show>
    </fieldset>
  );
}

/** The step's fields. `onChange` gets the whole flow, since a rename or a removal reaches others. */
function StepFields(props: { flow: Flow; step: FlowStep; onChange: (flow: Flow) => void; onRenamed: (id: string) => void }) {
  const set = (change: Partial<FlowStep>) => props.onChange(updateStep(props.flow, props.step.step_id, change));
  const rename = (id: string) => {
    if (isId(id)) {
      props.onChange(renameStep(props.flow, props.step.step_id, id));
      props.onRenamed(id);
    }
  };
  return (
    <div class="flex flex-col gap-3" data-flow-step={props.step.step_id}>
      <div class="grid gap-2 sm:grid-cols-2">
        <TextField label={t("flow.step.id")} hint={t("flow.step.idHint")} value={props.step.step_id} onInput={rename} />
        <SelectField
          label={t("flow.step.kind")}
          value={props.step.kind === OPTIONAL ? OPTIONAL : REQUIRED}
          options={[
            { value: REQUIRED, label: t("flow.step.required") },
            { value: OPTIONAL, label: t("flow.step.optional") },
          ]}
          onChange={(kind) => set({ kind })}
        />
      </div>
      <StepTarget step={props.step} onChange={set} />
      <Prerequisites flow={props.flow} step={props.step} onChange={props.onChange} />
      <ConditionEditor
        label={t("flow.step.appliesWhen")}
        value={props.step.applies_when}
        edgeOnly={props.step.done_when !== undefined}
        onChange={(applies_when) => set({ applies_when })}
      />
      <ConditionEditor
        label={t("flow.step.doneWhen")}
        value={props.step.done_when}
        edgeOnly
        onChange={(done_when) => set({ done_when })}
      />
      <div class="flex flex-wrap gap-2">
        <Button variant="secondary" size="sm" onClick={() => props.onChange(moveStep(props.flow, props.step.step_id, -1))}>{t("flow.step.earlier")}</Button>
        <Button variant="secondary" size="sm" onClick={() => props.onChange(moveStep(props.flow, props.step.step_id, 1))}>{t("flow.step.later")}</Button>
        <Button variant="danger-ghost" size="sm" onClick={() => props.onChange(removeStep(props.flow, props.step.step_id))}>{t("flow.step.remove")}</Button>
      </div>
    </div>
  );
}

/**
 * The editor of step `stepId` of `flow`. It stays mounted while the step changes, so a condition
 * being built keeps its unfinished clauses; the parent remounts it for another step.
 */
export function StepEditor(props: { flow: Flow; stepId: string; onChange: (flow: Flow) => void; onRenamed: (id: string) => void }) {
  const step = () => props.flow.steps.find((candidate) => candidate.step_id === props.stepId);
  return (
    <Show when={step()}>
      {(current) => <StepFields flow={props.flow} step={current()} onChange={props.onChange} onRenamed={props.onRenamed} />}
    </Show>
  );
}
