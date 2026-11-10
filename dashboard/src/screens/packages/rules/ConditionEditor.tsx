// A condition built from choices, written as CEL for the cloud and the store (ADR-0193), so HQ never
// types CEL. The clauses are `lib/rule-builder.ts`'s; this screen only picks them.
//
// A condition the builder did not write (one typed before the builder existed) is shown as it is,
// read-only, with one way forward: replace it with a built one. Half-understanding someone's CEL and
// writing back a different meaning would be worse than not editing it at all.

import { createSignal, For, Show, untrack } from "solid-js";

import { TechnicalDetails } from "../../../components/kit";
import { Button, CheckboxField, SelectField, TextField } from "../../../components/ui";
import { t, type MessageKey } from "../../../i18n";
import { channelLabel } from "../../../lib/channels";
import {
  type Clause,
  factAt,
  fromCel,
  opsFor,
  problemWith,
  RULE_FACTS,
  type RuleCondition,
  type RuleOp,
  toCel,
} from "../../../lib/rule-builder";

const OP_LABEL: Readonly<Record<RuleOp, MessageKey>> = {
  eq: "rule.op.eq",
  ne: "rule.op.ne",
  lt: "rule.op.lt",
  le: "rule.op.le",
  gt: "rule.op.gt",
  ge: "rule.op.ge",
  in: "rule.op.in",
  set: "rule.op.set",
  unset: "rule.op.unset",
};

/** The clause a new row starts as: the first fact, its first operator, no value yet. */
function blankClause(): Clause {
  const fact = RULE_FACTS[0];
  return { path: fact?.path ?? "", op: fact === undefined ? "eq" : (opsFor(fact)[0] ?? "eq"), values: [] };
}

/** One clause's value: a number, one token, or several tokens for `in`. */
function ClauseValue(props: { clause: Clause; onChange: (values: string[]) => void }) {
  const fact = () => factAt(props.clause.path);
  const tokens = () => (fact()?.tokens ?? []).map((token) => ({ value: token, label: channelLabel(token) }));
  return (
    <Show when={props.clause.op !== "set" && props.clause.op !== "unset"}>
      <Show
        when={fact()?.kind === "token"}
        fallback={
          <TextField
            label={t("rule.value")}
            type="number"
            value={props.clause.values[0] ?? ""}
            onInput={(value) => props.onChange([value])}
          />
        }
      >
        <Show
          when={props.clause.op === "in"}
          fallback={
            <SelectField
              label={t("rule.value")}
              value={props.clause.values[0] ?? ""}
              placeholder={t("rule.pick")}
              options={tokens()}
              onChange={(value) => props.onChange([value])}
            />
          }
        >
          <fieldset class="flex flex-wrap gap-x-4">
            <legend class="text-sm font-medium text-ink">{t("rule.anyOf")}</legend>
            <For each={tokens()}>
              {(token) => (
                <CheckboxField
                  label={token.label}
                  checked={props.clause.values.includes(token.value)}
                  onChange={(on) =>
                    props.onChange(
                      on
                        ? [...props.clause.values, token.value]
                        : props.clause.values.filter((value) => value !== token.value),
                    )
                  }
                />
              )}
            </For>
          </fieldset>
        </Show>
      </Show>
    </Show>
  );
}

/** One clause: what it looks at, how it compares, and to what. */
function ClauseRow(props: {
  clause: Clause;
  edgeOnly: boolean;
  onChange: (clause: Clause) => void;
  onRemove: () => void;
}) {
  const facts = () =>
    RULE_FACTS.filter((fact) => !props.edgeOnly || fact.edge).map((fact) => ({
      value: fact.path,
      label: t(fact.labelKey),
    }));
  const ops = () => {
    const fact = factAt(props.clause.path);
    return (fact === undefined ? [] : opsFor(fact)).map((op) => ({ value: op, label: t(OP_LABEL[op]) }));
  };
  const pickFact = (path: string) => {
    const fact = factAt(path);
    props.onChange({ path, op: fact === undefined ? "eq" : (opsFor(fact)[0] ?? "eq"), values: [] });
  };
  return (
    <div class="grid items-end gap-2 sm:grid-cols-[1fr_1fr_1fr_auto]" data-rule="clause">
      <SelectField label={t("rule.fact")} value={props.clause.path} options={facts()} onChange={pickFact} />
      <SelectField
        label={t("rule.compare")}
        value={props.clause.op}
        options={ops()}
        onChange={(op) => props.onChange({ ...props.clause, op: op as RuleOp, values: [] })}
      />
      <ClauseValue clause={props.clause} onChange={(values) => props.onChange({ ...props.clause, values })} />
      <Button variant="ghost" size="sm" onClick={() => props.onRemove()}>
        {t("rule.removeClause")}
      </Button>
    </div>
  );
}

/** The built condition: its clauses, how they join, and the CEL it writes. */
function BuiltCondition(props: { condition: RuleCondition; edgeOnly: boolean; onChange: (next: RuleCondition) => void }) {
  const set = (index: number, clause: Clause) =>
    props.onChange({ ...props.condition, clauses: props.condition.clauses.map((old, at) => (at === index ? clause : old)) });
  const remove = (index: number) =>
    props.onChange({ ...props.condition, clauses: props.condition.clauses.filter((_, at) => at !== index) });
  const problem = () => problemWith(props.condition);
  return (
    <div class="flex flex-col gap-2">
      <Show when={props.condition.clauses.length > 1}>
        <SelectField
          label={t("rule.join")}
          value={props.condition.join}
          options={[
            { value: "all", label: t("rule.join.all") },
            { value: "any", label: t("rule.join.any") },
          ]}
          onChange={(join) => props.onChange({ ...props.condition, join: join === "any" ? "any" : "all" })}
        />
      </Show>
      <For each={props.condition.clauses}>
        {(clause, index) => (
          <ClauseRow clause={clause} edgeOnly={props.edgeOnly} onChange={(next) => set(index(), next)} onRemove={() => remove(index())} />
        )}
      </For>
      <div>
        <Button variant="secondary" size="sm" onClick={() => props.onChange({ ...props.condition, clauses: [...props.condition.clauses, blankClause()] })}>
          {t("rule.addClause")}
        </Button>
      </div>
      <Show when={problem()}>{(key) => <p class="text-sm text-danger">{t(key())}</p>}</Show>
      <Show when={problem() === null}>
        <TechnicalDetails label={t("rule.asCel")}>{toCel(props.condition)}</TechnicalDetails>
      </Show>
    </div>
  );
}

/**
 * Edits one condition. `value` is the CEL a part holds, or `undefined` for none; `onChange` gets the
 * CEL to hold, or `undefined` to drop it. A clause still being filled in is kept here and not
 * written: the part keeps what it held until the condition can be written. `edgeOnly` offers only
 * the facts the edge holds, for an edge guard. The parent keys this by what it edits, so a new
 * step starts from its own condition.
 */
export function ConditionEditor(props: {
  label: string;
  value: string | undefined;
  onChange: (cel: string | undefined) => void;
  edgeOnly?: boolean;
}) {
  // Read once: the editor holds the clauses while they are edited, the part holds the CEL.
  const initial = untrack(() => props.value);
  const [editing, setEditing] = createSignal<RuleCondition | null>(
    initial === undefined ? null : fromCel(initial),
  );
  const handWritten = () => props.value !== undefined && editing() === null;
  const write = (next: RuleCondition) => {
    setEditing(next.clauses.length === 0 ? null : next);
    if (next.clauses.length === 0) {
      props.onChange(undefined);
    } else if (problemWith(next) === null) {
      props.onChange(toCel(next));
    }
  };
  return (
    <fieldset class="flex flex-col gap-2 rounded-token border border-line p-3" data-rule="condition">
      <legend class="px-1 text-sm font-medium text-ink">{props.label}</legend>
      <Show when={handWritten()}>
        <p class="text-sm text-ink-muted">{t("rule.handWritten")}</p>
        <code class="break-all text-xs text-ink">{props.value}</code>
        <div>
          <Button variant="secondary" size="sm" onClick={() => props.onChange(undefined)}>
            {t("rule.replace")}
          </Button>
        </div>
      </Show>
      <Show when={!handWritten() && editing() === null}>
        <p class="text-sm text-ink-muted">{t("rule.always")}</p>
        <div>
          <Button variant="secondary" size="sm" onClick={() => setEditing({ join: "all", clauses: [blankClause()] })}>
            {t("rule.addClause")}
          </Button>
        </div>
      </Show>
      <Show when={editing()}>
        {(condition) => <BuiltCondition condition={condition()} edgeOnly={props.edgeOnly === true} onChange={write} />}
      </Show>
    </fieldset>
  );
}
