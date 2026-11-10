// One custom field, edited in place (ADR-0194, ADR-0206): its kind of value, its label key, whether
// an entry must give it, and whether it is personal data. The personal question has no default and
// is asked as a question; a personal field then needs how long its value is kept and the purpose a
// person consents to, or it cannot be published. A field is retired, never removed, once a version
// that declares it has been published.

import { Show } from "solid-js";

import type { CustomField, CustomList } from "../../../api/schemas";
import { Button, CheckboxField, NumberField, SelectField, TextField } from "../../../components/ui";
import { t } from "../../../i18n";
import { changeField, DATA_OBJECTS, FIELD_TYPES, fieldProblem } from "../../../lib/data-draft";

/** Whether the value is personal data: unanswered until someone answers it. */
function PersonalQuestion(props: { field: CustomField; onChange: (change: Partial<CustomField>) => void }) {
  const answer = () => (props.field.personal === undefined ? "" : props.field.personal ? "yes" : "no");
  return (
    <div class="flex flex-col gap-2">
      <SelectField
        label={t("data.personal")}
        hint={t("data.personalHint")}
        value={answer()}
        placeholder={t("data.personalUnanswered")}
        options={[
          { value: "no", label: t("data.personalNo") },
          { value: "yes", label: t("data.personalYes") },
        ]}
        onChange={(value) => props.onChange({ personal: value === "" ? undefined : value === "yes" })}
      />
      <Show when={props.field.personal === true}>
        <div class="grid gap-2 sm:grid-cols-2">
          <NumberField
            label={t("data.retentionDays")}
            min={1}
            value={props.field.retention_days ?? null}
            onChange={(days) => props.onChange({ retention_days: days ?? undefined })}
          />
          <TextField
            label={t("data.consentPurposeKey")}
            hint={t("data.keyHint")}
            value={props.field.consent_purpose_key ?? ""}
            onInput={(consent_purpose_key) => props.onChange({ consent_purpose_key })}
          />
        </div>
      </Show>
    </div>
  );
}

/** The field's editor. `object` offers where the field sits, for a field of an existing object. */
export function FieldEditor(props: {
  field: CustomField;
  lists: readonly CustomList[];
  withObject: boolean;
  onChange: (field: CustomField) => void;
  onRemove: () => void;
}) {
  const set = (change: Partial<CustomField>) => props.onChange(changeField(props.field, change));
  const problem = () => fieldProblem(props.field);
  return (
    <div class="flex flex-col gap-2 rounded-token border border-line p-3" data-custom-field={props.field.custom_field_id}>
      <p class="font-medium text-ink">{props.field.custom_field_id}</p>
      <div class="grid gap-2 sm:grid-cols-3">
        <SelectField label={t("data.fieldType")} value={props.field.field_type} options={FIELD_TYPES.map((type) => ({ value: type.value, label: t(type.key) }))} onChange={(field_type) => set({ field_type })} />
        <Show when={props.withObject}>
          <SelectField label={t("data.object")} value={props.field.object ?? ""} options={DATA_OBJECTS.map((object) => ({ value: object.value, label: t(object.key) }))} onChange={(object) => set({ object })} />
        </Show>
        <TextField label={t("data.labelKey")} hint={t("data.keyHint")} value={props.field.label_key} onInput={(label_key) => set({ label_key })} />
      </div>
      <Show when={props.field.field_type === "CUSTOM_FIELD_TYPE_CHOICE"}>
        <SelectField label={t("data.list")} value={props.field.custom_list_id ?? ""} placeholder={t("rule.pick")} options={props.lists.map((list) => ({ value: list.custom_list_id, label: list.custom_list_id }))} onChange={(custom_list_id) => set({ custom_list_id })} />
      </Show>
      <Show when={props.field.field_type === "CUSTOM_FIELD_TYPE_QUANTITY"}>
        <TextField label={t("data.unitKey")} hint={t("data.keyHint")} value={props.field.unit_key ?? ""} onInput={(unit_key) => set({ unit_key })} />
      </Show>
      <CheckboxField label={t("data.required")} checked={props.field.required === true} onChange={(required) => set({ required })} />
      <PersonalQuestion field={props.field} onChange={set} />
      <Show when={problem()}>{(key) => <p class="text-sm text-danger">{t(key())}</p>}</Show>
      <div class="flex flex-wrap gap-2">
        <CheckboxField label={t("data.retired")} checked={props.field.deprecated === true} onChange={(deprecated) => set({ deprecated })} />
        <Button variant="danger-ghost" size="sm" onClick={() => props.onRemove()}>{t("data.remove")}</Button>
      </div>
    </div>
  );
}
