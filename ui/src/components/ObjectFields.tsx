// The custom fields a package adds to an existing object, typed at the till (ADR-0194, ADR-0206;
// phase 10 open item O7): an order's, a bill's, a table's or a shift's "Details". The package declares
// the fields; the edge re-checks every value and is the authority. A personal value is typed here, sent
// only with its consent ticked, and never shown again: the edge answers that one is held.

import { For, Show, createResource, createSignal } from "solid-js";

import { api } from "../api/client";
import type { CustomField } from "../api/schemas";
import { t, type MessageKey } from "../i18n";
import { drafted, wire, type Drafted } from "../lib/custom-values";
import { ConsentBoxes, FieldInput, refusalKey } from "../screens/Records";
import { customData } from "../state/custom-data";

/** The fields the running package declares on `object`, not retired. */
export function fieldsOn(fields: readonly CustomField[], object: string): CustomField[] {
  return fields.filter((field) => field.object === object && field.deprecated !== true);
}

/** The consents the typed personal values need, each purpose once. */
export function purposesFor(fields: readonly CustomField[], draft: Readonly<Record<string, Drafted>>): string[] {
  const typed = fields.filter((field) => field.personal === true && String(draft[field.custom_field_id] ?? "").trim() !== "");
  return [...new Set(typed.flatMap((field) => (field.consent_purpose_key === undefined ? [] : [field.consent_purpose_key])))];
}

/** The object's Details: its fields, typed and saved. Draws nothing where the package declares none. */
export function ObjectFields(props: { object: string; objectId: string | undefined }) {
  const fields = () => fieldsOn(customData().custom_fields ?? [], props.object);
  const [held] = createResource(
    () => (fields().length > 0 && props.objectId ? props.objectId : undefined),
    (objectId) => api.objectCustomFields(props.object, objectId).catch(() => null),
  );
  const [draft, setDraft] = createSignal<Readonly<Record<string, Drafted>>>({});
  const [consented, setConsented] = createSignal<ReadonlySet<string>>(new Set());
  const [outcome, setOutcome] = createSignal<MessageKey | null>(null);
  const valueOf = (field: CustomField) =>
    draft()[field.custom_field_id] ?? (field.personal === true ? "" : drafted(field, held()?.values.find((value) => value.custom_field_id === field.custom_field_id)));
  const save = async () => {
    const typed = fields().filter((field) => draft()[field.custom_field_id] !== undefined);
    const values = typed.map((field) => wire(field, draft()[field.custom_field_id] ?? ""));
    if (props.objectId === undefined || values.some((value) => value === "invalid")) return setOutcome("records.invalid_value");
    const body = { object: props.object, object_id: props.objectId, values: values.filter((value) => value !== "invalid"), consented_purpose_keys: purposesFor(fields(), draft()) };
    await api.setCustomFields(body).then(() => (setDraft({}), setOutcome("details.saved")), (caught: unknown) => setOutcome(refusalKey(caught)));
  };
  return (
    <Show when={fields().length > 0 && props.objectId}>
      <details class="mt-2 rounded-token border border-line px-3 py-2" data-outcome="object-fields">
        <summary class="min-h-touch cursor-pointer font-semibold">{t("details.title")}</summary>
        <For each={fields()}>{(field) => <FieldInput field={field} value={valueOf(field)} onInput={(value) => setDraft((was) => ({ ...was, [field.custom_field_id]: value }))} />}</For>
        <ConsentBoxes purposes={purposesFor(fields(), draft())} given={consented()} onTick={(purpose, ticked) => setConsented((was) => new Set(ticked ? [...was, purpose] : [...was].filter((key) => key !== purpose)))} />
        <Show when={outcome()}>{(key) => <p class="mt-2 text-sm" role="status" data-outcome="object-fields-outcome">{t(key())}</p>}</Show>
        <button type="button" class="mt-3 min-h-touch rounded-token bg-primary px-4 font-semibold text-primary-ink disabled:opacity-50" data-step="saveDetails"
          disabled={Object.keys(draft()).length === 0 || purposesFor(fields(), draft()).some((purpose) => !consented().has(purpose))} onClick={() => void save()}>
          {t("records.save")}
        </button>
      </details>
    </Show>
  );
}
