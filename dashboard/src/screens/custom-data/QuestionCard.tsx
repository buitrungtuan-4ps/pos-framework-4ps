// The question the Custom data screen asks: what to count, what to group by, and an optional
// filter. Only non-personal fields are offered (ADR-0194 decision 7).

import { Show } from "solid-js";

import { Button, Card, SelectField, TextField } from "../../components/ui";
import { t } from "../../i18n";
import { filterable } from "../../lib/custom-data";
import type { CustomDataState } from "./state";

/** The optional filter: a field, and the value it must hold. */
function FilterFields(props: {
  state: CustomDataState;
  options: readonly { readonly value: string; readonly label: string }[];
}) {
  const q = props.state.question;
  return (
    <>
      <SelectField
        label={t("customData.filterField")}
        value={q.filterFieldId()}
        placeholder={t("customData.noFilter")}
        disabled={!props.state.target()}
        options={props.options}
        onChange={(value) => {
          q.setFilterFieldId(value);
          q.setFilterValue("");
        }}
      />
      <Show when={q.filterFieldId()}>
        <TextField
          label={t("customData.filterValue")}
          hint={t("customData.filterValueHint")}
          value={q.filterValue()}
          onInput={q.setFilterValue}
        />
      </Show>
    </>
  );
}

export function QuestionCard(props: { state: CustomDataState }) {
  const state = props.state;
  const q = state.question;
  const fields = () => state.target()?.fields ?? [];
  const options = (filter: boolean) =>
    fields()
      .filter((field) => !filter || filterable(field))
      .map((field) => ({ value: field.custom_field_id, label: state.label(field.label_key) }));
  const chooseTarget = (value: string) => {
    q.setTargetId(value);
    q.setFieldId("");
    q.setFilterFieldId("");
    state.setSummary(null);
  };
  return (
    <Card title={t("customData.question")}>
      <div class="grid gap-3 sm:grid-cols-2">
        <SelectField
          label={t("customData.target")}
          value={q.targetId()}
          placeholder={t("customData.choose")}
          options={state.targets().map((each) => ({ value: each.id, label: state.targetLabel(each) }))}
          onChange={chooseTarget}
        />
        <SelectField
          label={t("customData.field")}
          value={q.fieldId()}
          placeholder={t("customData.choose")}
          disabled={!state.target()}
          options={options(false)}
          onChange={q.setFieldId}
        />
        <FilterFields state={state} options={options(true)} />
      </div>
      <div class="mt-3">
        <Button onClick={() => void state.run()} disabled={state.busy() || !q.fieldId()}>
          {t("customData.run")}
        </Button>
      </div>
    </Card>
  );
}
