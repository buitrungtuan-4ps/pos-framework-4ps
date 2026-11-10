// The rule builder's reactions: "when … if … then …" (ADR-0193 decision 5). When a trigger fires
// on the till, if a condition holds, run one act: a UI act or an edge write, never money, with the
// parameters fixed here. The condition is built in `ConditionEditor`, so nobody types CEL.
//
// A part of the package designer (`designer/parts.tsx`): it edits the draft's `reactions` and shows
// the cloud's refusals beside the reaction they name.

import { createSignal, For, Index, Show } from "solid-js";

import { Button, Card, SelectField, TextField } from "../../../components/ui";
import type { Flow, PackageRefusal, Reaction, ReactionVariant } from "../../../api/schemas";
import { t, type MessageKey } from "../../../i18n";
import { actionAt, fixedParams, reactionActs } from "../../../lib/action-catalogue";
import { idFrom } from "../../../lib/flow-graph";
import type { PartProps } from "../designer/parts";
import { ConditionEditor } from "../rules/ConditionEditor";

const TRIGGERS: readonly { readonly value: string; readonly key: MessageKey }[] = [
  { value: "REACTION_TRIGGER_ORDER_OPENED", key: "reaction.when.orderOpened" },
  { value: "REACTION_TRIGGER_LINE_ADDED", key: "reaction.when.lineAdded" },
  { value: "REACTION_TRIGGER_BILL_OPENED", key: "reaction.when.billOpened" },
  { value: "REACTION_TRIGGER_SCREEN_ENTERED", key: "reaction.when.screenEntered" },
  { value: "REACTION_TRIGGER_STEP_COMPLETED", key: "reaction.when.stepCompleted" },
];

const REACTION_REASON: Readonly<Record<string, MessageKey>> = {
  BAD_ID: "flow.refusal.badId",
  UNKNOWN_TRIGGER: "reaction.refusal.unknownTrigger",
  QUALIFIER: "reaction.refusal.qualifier",
  UNKNOWN_TARGET: "reaction.refusal.unknownTarget",
  TOO_MANY_FOR_TRIGGER: "reaction.refusal.tooMany",
  UNKNOWN_ACTION: "flow.refusal.unknownAction",
  ACT_NOT_ALLOWED: "reaction.refusal.actNotAllowed",
  PARAM: "reaction.refusal.param",
  CONDITION: "reaction.refusal.condition",
};

/** A reaction refusal in words. */
export const reactionRefusalText = (refusal: PackageRefusal): string =>
  t(REACTION_REASON[refusal.reason] ?? "designer.refusal.other", { name: refusal.name ?? "", field: refusal.field });

/** The trigger with what it is for: a screen, or a step of a flow; the other fields are dropped. */
function withTrigger(reaction: Reaction, when: string): Reaction {
  const { screen_id: _screen, flow_id: _flow, step_id: _step, ...rest } = reaction;
  return { ...rest, when };
}

/** What fires it: the trigger, and the screen or the flow's step it names. */
function Trigger(props: { reaction: Reaction; flows: readonly Flow[]; onChange: (reaction: Reaction) => void }) {
  const flow = () => props.flows.find((candidate) => candidate.flow_id === props.reaction.flow_id);
  return (
    <div class="grid gap-2 sm:grid-cols-3">
      <SelectField
        label={t("reaction.when")}
        value={props.reaction.when}
        options={TRIGGERS.map((trigger) => ({ value: trigger.value, label: t(trigger.key) }))}
        onChange={(when) => props.onChange(withTrigger(props.reaction, when))}
      />
      <Show when={props.reaction.when === "REACTION_TRIGGER_SCREEN_ENTERED"}>
        <TextField label={t("flow.step.screen")} value={props.reaction.screen_id ?? ""} onInput={(screen_id) => props.onChange({ ...props.reaction, screen_id })} />
      </Show>
      <Show when={props.reaction.when === "REACTION_TRIGGER_STEP_COMPLETED"}>
        <SelectField
          label={t("reaction.flow")}
          value={props.reaction.flow_id ?? ""}
          placeholder={t("rule.pick")}
          options={props.flows.map((candidate) => ({ value: candidate.flow_id, label: candidate.flow_id }))}
          onChange={(flow_id) => props.onChange({ ...props.reaction, flow_id, step_id: "" })}
        />
        <SelectField
          label={t("reaction.step")}
          value={props.reaction.step_id ?? ""}
          placeholder={t("rule.pick")}
          options={(flow()?.steps ?? []).map((step) => ({ value: step.step_id, label: step.step_id }))}
          onChange={(step_id) => props.onChange({ ...props.reaction, step_id })}
        />
      </Show>
    </div>
  );
}

/** What it runs: an act a reaction may run, and the parameters that act takes, fixed here. */
function Act(props: { reaction: Reaction; onChange: (reaction: Reaction) => void }) {
  const action = () => actionAt(props.reaction.then.action_id);
  const setParam = (name: string, value: string) =>
    props.onChange({ ...props.reaction, then: { ...props.reaction.then, params: { ...props.reaction.then.params, [name]: value } } });
  return (
    <div class="flex flex-col gap-2">
      <SelectField
        label={t("reaction.then")}
        value={props.reaction.then.action_id}
        options={reactionActs().map((act) => ({ value: act.action_id, label: act.action_id }))}
        onChange={(action_id) => props.onChange({ ...props.reaction, then: { action_id, params: {} } })}
      />
      <For each={action() === undefined ? [] : fixedParams(action()!)}>
        {(param) => (
          <TextField
            label={param.required ? param.name : t("reaction.optionalParam", { name: param.name })}
            value={props.reaction.then.params?.[param.name] ?? ""}
            onInput={(value) => setParam(param.name, value)}
          />
        )}
      </For>
    </div>
  );
}

/** One reaction: when, if, then, and its refusals. */
function ReactionCard(props: {
  variant: ReactionVariant;
  flows: readonly Flow[];
  refusals: readonly PackageRefusal[];
  onChange: (reaction: Reaction) => void;
  onRemove: () => void;
}) {
  const reaction = () => props.variant.body;
  return (
    <Card title={reaction().reaction_id} actions={<Button variant="danger-ghost" size="sm" onClick={() => props.onRemove()}>{t("reaction.remove")}</Button>}>
      <div class="flex flex-col gap-3" data-reaction={reaction().reaction_id}>
        <Trigger reaction={reaction()} flows={props.flows} onChange={props.onChange} />
        <ConditionEditor label={t("reaction.if")} value={reaction().if} onChange={(condition) => {
          const { if: _old, ...rest } = reaction();
          props.onChange(condition === undefined ? rest : { ...rest, if: condition });
        }} />
        <Act reaction={reaction()} onChange={props.onChange} />
        <Show when={props.refusals.length > 0}>
          <ul role="alert" class="flex flex-col gap-1 text-sm text-danger" data-outcome="reaction-refused">
            <For each={props.refusals}>{(refusal) => <li>{reactionRefusalText(refusal)}</li>}</For>
          </ul>
        </Show>
      </div>
    </Card>
  );
}

/** The panel: every reaction, and a way to add one. */
export function ReactionsPanel(props: PartProps) {
  const reactions = () => props.draft().reactions ?? [];
  const flows = () => (props.draft().flows ?? []).map((variant) => variant.body);
  const [name, setName] = createSignal("");
  const set = (next: readonly ReactionVariant[]) => props.change((draft) => ({ ...draft, reactions: [...next] }));
  const add = () => {
    const trimmed = name().trim();
    if (trimmed === "") {
      return;
    }
    const reaction_id = idFrom(trimmed, reactions().map((variant) => variant.body.reaction_id), "reaction");
    const first = reactionActs()[0]?.action_id ?? "";
    set([...reactions(), { body: { reaction_id, when: "REACTION_TRIGGER_ORDER_OPENED", then: { action_id: first, params: {} } } }]);
    setName("");
  };
  const whole = () => props.refusals.filter((refusal) => refusal.reaction_index === undefined);
  return (
    <div class="flex flex-col gap-4">
      <p class="text-sm text-ink-muted">{t("reaction.intro")}</p>
      <Index each={reactions()}>
        {(variant, index) => (
          <ReactionCard
            variant={variant()}
            flows={flows()}
            refusals={props.refusals.filter((refusal) => refusal.reaction_index === index)}
            onChange={(body) => set(reactions().map((old, at) => (at === index ? { ...old, body } : old)))}
            onRemove={() => set(reactions().filter((_, at) => at !== index))}
          />
        )}
      </Index>
      <div class="flex items-end gap-2">
        <TextField label={t("reaction.newName")} value={name()} onInput={setName} />
        <Button onClick={add}>{t("reaction.add")}</Button>
      </div>
      <Show when={whole().length > 0}>
        <ul role="alert" class="flex flex-col gap-1 text-sm text-danger">
          <For each={whole()}>{(refusal) => <li>{reactionRefusalText(refusal)}</li>}</For>
        </ul>
      </Show>
    </div>
  );
}
