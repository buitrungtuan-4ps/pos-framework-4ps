// The flow designer: a package's flows as graphs of steps (ADR-0192), each required or optional,
// perhaps conditional, waiting on the steps before it. A part of the package designer
// (`designer/parts.tsx`): it edits the draft's `flows` and shows the cloud's refusals beside the
// flow and the step they name.
//
// A flow's variants (ADR-0189 decision 4) are listed as they are; each is edited as a flow of its
// own, under the same id.

import { createSignal, For, Index, Show } from "solid-js";

import { Button, Card, CheckboxField, SelectField, TextField } from "../../../components/ui";
import type { Flow, FlowVariant, PackageRefusal } from "../../../api/schemas";
import { t, type MessageKey } from "../../../i18n";
import { CATALOGUE } from "../../../lib/action-catalogue";
import { addStep, FLOWS_MAX, GUARD_CHANNELS, GUARD_SCOPES, idFrom, localProblems, scopeWarnings } from "../../../lib/flow-graph";
import type { PartProps } from "../designer/parts";
import { FlowGraph } from "./FlowGraph";
import { StepEditor } from "./StepEditor";
import { actionName } from "../screens/names";

/** What a flow refusal's reason says, in the console's words. */
const FLOW_REASON: Readonly<Record<string, MessageKey>> = {
  TOO_MANY_FLOWS: "flow.refusal.tooManyFlows",
  STEP_COUNT: "flow.problem.stepCount",
  BAD_ID: "flow.refusal.badId",
  DUPLICATE_STEP: "flow.problem.stepId",
  NOT_ONE_THING: "flow.problem.oneThing",
  UNKNOWN_KIND: "flow.refusal.unknownKind",
  NO_OFFLINE_BRANCH: "flow.refusal.noOfflineBranch",
  BAD_OFFLINE_BRANCH: "flow.refusal.badOfflineBranch",
  UNKNOWN_ACTION: "flow.refusal.unknownAction",
  UNKNOWN_SCREEN: "flow.refusal.unknownScreen",
  UNKNOWN_PREREQUISITE: "flow.problem.prerequisite",
  CYCLE: "flow.problem.cycle",
  CONDITION: "flow.refusal.condition",
};

/** A flow refusal in words, naming the step and what it names. */
export function flowRefusalText(refusal: PackageRefusal): string {
  const key = FLOW_REASON[refusal.reason] ?? "designer.refusal.other";
  const where = refusal.step_id === undefined ? "" : t("flow.refusal.atStep", { step: refusal.step_id });
  return `${where}${t(key, { name: refusal.name ?? "", field: refusal.field })}`;
}

/** Which orders the flow's guards apply to (ADR-0244), and what Check warns of in that choice. */
function GuardScope(props: { flow: Flow; onChange: (flow: Flow) => void }) {
  const scope = () => props.flow.guard_scope ?? "FLOW_GUARD_SCOPE_ENTERED";
  const channels = () => props.flow.guard_channels ?? [];
  const toggle = (channel: string, on: boolean) => {
    const rest = channels().filter((named) => named !== channel);
    props.onChange({ ...props.flow, guard_channels: on ? [...rest, channel] : rest });
  };
  return (
    <div class="flex flex-col gap-1" data-flow-scope={scope()}>
      <SelectField
        label={t("flow.scope.label")}
        hint={t("flow.scope.hint")}
        value={scope()}
        options={GUARD_SCOPES.map((value) => ({ value, label: t(`flow.scope.${value}`) }))}
        onChange={(guard_scope) => props.onChange({ ...props.flow, guard_scope })}
      />
      <Show when={scope() === "FLOW_GUARD_SCOPE_CHANNELS"}>
        <div class="flex flex-wrap gap-x-4">
          <For each={GUARD_CHANNELS}>
            {(channel) => (
              <CheckboxField
                label={t(channel.label)}
                checked={channels().includes(channel.value)}
                onChange={(on) => toggle(channel.value, on)}
              />
            )}
          </For>
        </div>
      </Show>
      <Show when={scopeWarnings(props.flow).length > 0}>
        <ul class="flex flex-col gap-1 text-sm font-medium text-ink" role="note" data-outcome="flow-scope-warning">
          <For each={scopeWarnings(props.flow)}>{(key) => <li>{t(key)}</li>}</For>
        </ul>
      </Show>
    </div>
  );
}

/** One flow: its start, its graph, the step being edited, and its refusals. */
function FlowCard(props: {
  variant: FlowVariant;
  refusals: readonly PackageRefusal[];
  onChange: (flow: Flow) => void;
  onRemove: () => void;
}) {
  const flow = () => props.variant.body;
  const [picked, setPicked] = createSignal<string | null>(null);
  const [stepName, setStepName] = createSignal("");
  const add = () => {
    const name = stepName().trim();
    if (name === "") {
      return;
    }
    const next = addStep(flow(), name, { action_id: CATALOGUE[0]?.action_id ?? "" });
    props.onChange(next);
    setPicked(next.steps[next.steps.length - 1]?.step_id ?? null);
    setStepName("");
  };
  const problems = () => localProblems(flow());
  return (
    <Card title={flow().flow_id} actions={<Button variant="danger-ghost" size="sm" onClick={() => props.onRemove()}>{t("flow.remove")}</Button>}>
      <div class="flex flex-col gap-3" data-flow={flow().flow_id}>
        <Show when={props.variant.selector !== undefined && Object.keys(props.variant.selector).length > 0}>
          <p class="text-sm text-ink-muted">{t("flow.variant")}</p>
        </Show>
        <SelectField
          label={t("flow.startsWith")}
          value={flow().start_action_id}
          options={CATALOGUE.map((action) => ({ value: action.action_id, label: actionName(action.action_id) }))}
          onChange={(start_action_id) => props.onChange({ ...flow(), start_action_id })}
        />
        <GuardScope flow={flow()} onChange={props.onChange} />
        <FlowGraph flow={flow()} picked={picked()} refusals={props.refusals} onPick={setPicked} />
        <div class="flex items-end gap-2">
          <TextField label={t("flow.step.newName")} value={stepName()} onInput={setStepName} />
          <Button variant="secondary" onClick={add}>{t("flow.step.add")}</Button>
        </div>
        <Show when={picked()} keyed>
          {(id) => <StepEditor flow={flow()} stepId={id} onChange={props.onChange} onRenamed={setPicked} />}
        </Show>
        <Show when={problems().length > 0 || props.refusals.length > 0}>
          <ul role="alert" class="flex flex-col gap-1 text-sm text-danger" data-outcome="flow-refused">
            <For each={problems()}>
              {(problem) => (
                <li>{problem.stepId === undefined ? "" : t("flow.refusal.atStep", { step: problem.stepId })}{t(problem.key)}</li>
              )}
            </For>
            <For each={props.refusals}>{(refusal) => <li>{flowRefusalText(refusal)}</li>}</For>
          </ul>
        </Show>
      </div>
    </Card>
  );
}

/** The panel: every flow, a way to add one, and refusals about the flows together. */
export function FlowsPanel(props: PartProps) {
  const flows = () => props.draft().flows ?? [];
  const [name, setName] = createSignal("");
  const set = (next: readonly FlowVariant[]) => props.change((draft) => ({ ...draft, flows: [...next] }));
  const add = () => {
    const trimmed = name().trim();
    if (trimmed === "" || flows().length >= FLOWS_MAX) {
      return;
    }
    const flow_id = idFrom(trimmed, flows().map((variant) => variant.body.flow_id), "flow");
    set([...flows(), { body: { flow_id, start_action_id: CATALOGUE[0]?.action_id ?? "", steps: [] } }]);
    setName("");
  };
  const whole = () => props.refusals.filter((refusal) => refusal.flow_index === undefined);
  return (
    <div class="flex flex-col gap-4">
      <p class="text-sm text-ink-muted">{t("flow.intro")}</p>
      <Index each={flows()}>
        {(variant, index) => (
          <FlowCard
            variant={variant()}
            refusals={props.refusals.filter((refusal) => refusal.flow_index === index)}
            onChange={(body) => set(flows().map((old, at) => (at === index ? { ...old, body } : old)))}
            onRemove={() => set(flows().filter((_, at) => at !== index))}
          />
        )}
      </Index>
      <div class="flex items-end gap-2">
        <TextField label={t("flow.newName")} value={name()} onInput={setName} />
        <Button onClick={add} disabled={flows().length >= FLOWS_MAX}>{t("flow.add")}</Button>
      </div>
      <Show when={whole().length > 0}>
        <ul role="alert" class="flex flex-col gap-1 text-sm text-danger">
          <For each={whole()}>{(refusal) => <li>{flowRefusalText(refusal)}</li>}</For>
        </ul>
      </Show>
    </div>
  );
}
