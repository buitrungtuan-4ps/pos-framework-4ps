// A package's custom records (ADR-0194, ADR-0206): its object types, each one's records, and the form
// that enters or changes one.
//
// The edge re-checks every value against the running package and is the authority; this form only
// offers what the package declares. A personal value is entered here and never shown again: after a
// save the field is empty, the record says that a value is held, and showing it takes
// `data.personal.read`, one subject at a time, every read audited by the edge. A personal value is
// sent only with its consent ticked, and the consent's wording is the tenant's label for its purpose.

import { For, Show, createEffect, createSignal, on, type JSX } from "solid-js";

import { ApiError, api } from "../api/client";
import type { CustomDataResponse, CustomField, ObjectType, ValueRequest } from "../api/schemas";
import { ReadFailed } from "../components/StoreData";
import { t, type MessageKey } from "../i18n";
import { drafted, wire, type Drafted } from "../lib/custom-values";
import { RegionsContext, type Regions } from "../package/regions";
import { packageLabel } from "../package/render";
import { NO_ACTS, PackageScreen } from "../package/screen";
import { customData } from "../state/custom-data";
import { can, holdsOwn } from "../state/permissions";
import { createScreenRead } from "../state/screen-read";

const INPUT = "mt-1 block min-h-touch w-full rounded-token border border-line bg-surface px-3";

/** Why a save or a read was refused, as the operator reads it. */
function refusalKey(caught: unknown): MessageKey {
  const reason = caught instanceof ApiError ? caught.reason : null;
  if (reason === "CUSTOM_ENTRY_REFUSED") {
    return "records.refused_entry";
  }
  return reason === "PERMISSION_DENIED" ? "records.refused_permission" : "records.save_failed";
}

/** The current entries of the list a `choice` field draws from. */
function entriesOf(field: CustomField) {
  const list = (customData().lists ?? []).find((held) => held.custom_list_id === field.custom_list_id);
  return (list?.entries ?? []).filter((entry) => entry.deprecated !== true);
}

type InputProps = { id: string; field: CustomField; value: Drafted; onInput: (value: Drafted) => void };

function ChoiceInput(props: InputProps) {
  return (
    <select id={props.id} class={INPUT} value={String(props.value)} onChange={(event) => props.onInput(event.currentTarget.value)}>
      <option value="">{t("records.none_chosen")}</option>
      <For each={entriesOf(props.field)}>
        {(entry) => <option value={entry.list_entry_id}>{packageLabel(entry.label_key)}</option>}
      </For>
    </select>
  );
}

function TypedInput(props: InputProps) {
  const date = () => props.field.field_type === "CUSTOM_FIELD_TYPE_DATE";
  return (
    <input
      id={props.id}
      type={date() ? "date" : "text"}
      inputMode={date() ? undefined : "decimal"}
      autocomplete="off"
      class={INPUT}
      value={String(props.value)}
      onInput={(event) => props.onInput(event.currentTarget.value)}
    />
  );
}

const INPUTS: Readonly<Record<string, (props: InputProps) => JSX.Element>> = {
  CUSTOM_FIELD_TYPE_BOOL: (props) => (
    <input
      id={props.id}
      type="checkbox"
      class="ml-3 h-6 w-6 align-middle"
      checked={props.value === true}
      onChange={(event) => props.onInput(event.currentTarget.checked)}
    />
  ),
  CUSTOM_FIELD_TYPE_CHOICE: ChoiceInput,
  CUSTOM_FIELD_TYPE_TEXT: (props) => (
    <textarea
      id={props.id}
      class="mt-1 block w-full rounded-token border border-line bg-surface p-3"
      maxLength={1024}
      autocomplete="off"
      value={String(props.value)}
      onInput={(event) => props.onInput(event.currentTarget.value)}
    />
  ),
};

/** One field's label and input, in the form its type takes. */
function FieldInput(props: { field: CustomField; value: Drafted; onInput: (value: Drafted) => void }) {
  const id = () => `field-${props.field.custom_field_id}`;
  const Input = INPUTS[props.field.field_type] ?? TypedInput;
  return (
    <label class="mt-3 block text-sm" for={id()}>
      <span class="font-semibold">{packageLabel(props.field.label_key)}</span>
      <Show when={props.field.required === true}>
        <span class="text-ink-muted"> {t("records.required")}</span>
      </Show>
      <Input id={id()} field={props.field} value={props.value} onInput={props.onInput} />
    </label>
  );
}

/** The consents the personal values typed need, one box each, labelled with the tenant's purpose. */
function ConsentBoxes(props: { purposes: string[]; given: ReadonlySet<string>; onTick: (purpose: string, ticked: boolean) => void }) {
  return (
    <For each={props.purposes}>
      {(purpose) => (
        <label class="mt-3 flex min-h-touch items-center gap-3 text-sm">
          <input
            type="checkbox"
            class="h-6 w-6"
            checked={props.given.has(purpose)}
            onChange={(event) => props.onTick(purpose, event.currentTarget.checked)}
          />
          <span>{t("records.consent", { purpose: packageLabel(purpose) })}</span>
        </label>
      )}
    </For>
  );
}

type FormProps = {
  kind: ObjectType;
  record: CustomDataResponse | null;
  onSaved: (saved: CustomDataResponse) => void;
  onCancel: () => void;
};

/** The form's state and acts: what is typed, which fields changed, the consents ticked, and saving. */
function createRecordForm(props: FormProps) {
  const fields = () => props.kind.fields.filter((field) => field.deprecated !== true);
  const valueOf = (field: CustomField) =>
    field.personal === true ? "" : drafted(field, props.record?.values.find((value) => value.custom_field_id === field.custom_field_id));
  const [draft, setDraft] = createSignal<Readonly<Record<string, Drafted>>>(
    Object.fromEntries(fields().map((field) => [field.custom_field_id, valueOf(field)])),
  );
  const [changed, setChanged] = createSignal<ReadonlySet<string>>(new Set());
  const [consented, setConsented] = createSignal<ReadonlySet<string>>(new Set());
  const [shown, setShown] = createSignal<Readonly<Record<string, string>> | null>(null);
  const [error, setError] = createSignal<MessageKey | null>(null);
  const [saving, setSaving] = createSignal(false);

  const enter = (field: CustomField, value: Drafted) => {
    setDraft((held) => ({ ...held, [field.custom_field_id]: value }));
    setChanged((held) => new Set([...held, field.custom_field_id]));
  };
  const purposes = () => [
    ...new Set(
      fields()
        .filter((field) => field.personal === true && String(draft()[field.custom_field_id] ?? "").trim() !== "")
        .flatMap((field) => (field.consent_purpose_key === undefined ? [] : [field.consent_purpose_key])),
    ),
  ];
  const tick = (purpose: string, ticked: boolean) =>
    setConsented((held) => new Set(ticked ? [...held, purpose] : [...held].filter((key) => key !== purpose)));

  const showPersonal = async () => {
    const subjectId = props.record?.subject_id;
    if (subjectId !== undefined) {
      await api.subject(subjectId).then((subject) => setShown(subject.fields), (caught: unknown) => setError(refusalKey(caught)));
    }
  };

  const valuesToSend = (): ValueRequest[] | null => {
    const sent = fields().filter((field) => props.record === null || changed().has(field.custom_field_id));
    const values = sent.map((field) => wire(field, draft()[field.custom_field_id] ?? ""));
    return values.every((value) => value !== "invalid") ? (values as ValueRequest[]) : null;
  };

  const saveRecord = async () => {
    const values = valuesToSend();
    if (values === null) {
      setError("records.invalid_value");
      return;
    }
    setError(null);
    setSaving(true);
    const body = { object_type_id: props.kind.object_type_id, values, consented_purpose_keys: purposes() };
    const recordId = props.record?.custom_record_id;
    try {
      props.onSaved(await (recordId === undefined ? api.createCustomRecord(body) : api.changeCustomRecord(recordId, body)));
    } catch (caught) {
      setError(refusalKey(caught));
    } finally {
      setSaving(false);
    }
  };

  return { fields, draft, enter, purposes, consented, tick, shown, showPersonal, error, saving, saveRecord };
}

/** The form that enters a new record of `kind`, or changes `record`. */
function RecordForm(props: FormProps) {
  const form = createRecordForm(props);
  const held = () => props.record?.subject_id !== undefined;
  return (
    <div class="mt-4 rounded-token border border-line bg-surface p-4" data-outcome="record-form">
      <For each={form.fields()}>
        {(field) => (
          <>
            <FieldInput field={field} value={form.draft()[field.custom_field_id] ?? ""} onInput={(value) => form.enter(field, value)} />
            <Show when={field.personal === true && held()}>
              <p class="mt-1 text-sm text-ink-muted" data-outcome="personal-held">
                {form.shown()?.[field.custom_field_id] ?? t("records.personal_held")}
              </p>
            </Show>
          </>
        )}
      </For>
      <Show when={held() && form.shown() === null && holdsOwn("data.personal.read")}>
        <button type="button" class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm" data-step="showPersonal" onClick={() => void form.showPersonal()}>
          {t("records.show_personal")}
        </button>
      </Show>
      <ConsentBoxes purposes={form.purposes()} given={form.consented()} onTick={form.tick} />
      <Show when={form.error()}>
        {(key) => (
          <p class="mt-3 text-danger" role="alert" data-outcome="record-refused">
            {t(key())}
          </p>
        )}
      </Show>
      <div class="mt-4 grid grid-cols-2 gap-2">
        <button
          type="button"
          class="min-h-touch rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
          disabled={form.saving() || form.purposes().some((purpose) => !form.consented().has(purpose))}
          data-step="saveRecord"
          onClick={() => void form.saveRecord()}
        >
          {t("records.save")}
        </button>
        <button type="button" class="min-h-touch rounded-token border border-line" onClick={() => props.onCancel()}>
          {t("common.cancel")}
        </button>
      </div>
    </div>
  );
}

/** One record in a list: its non-personal values in the type's order, labelled from the grid. */
function summary(kind: ObjectType, record: CustomDataResponse): string {
  const entryLabel = (entryId: string) =>
    packageLabel((customData().lists ?? []).flatMap((list) => list.entries).find((entry) => entry.list_entry_id === entryId)?.label_key);
  const text = (field: CustomField): string => {
    const value = record.values.find((held) => held.custom_field_id === field.custom_field_id);
    if (value?.list_entry_id !== undefined) {
      return entryLabel(value.list_entry_id);
    }
    if (value?.boolean !== undefined) {
      return value.boolean ? packageLabel(field.label_key) : "";
    }
    return value === undefined ? "" : String(drafted(field, value));
  };
  return kind.fields
    .filter((field) => field.personal !== true)
    .map(text)
    .filter((shown) => shown !== "")
    .join(" · ");
}

/** The object types, as tabs. */
function KindTabs(props: { shown: ObjectType; onChoose: (objectTypeId: string) => void }) {
  return (
    <div class="flex flex-wrap gap-2" role="tablist">
      <For each={customData().object_types ?? []}>
        {(each) => (
          <button
            type="button"
            role="tab"
            aria-selected={each.object_type_id === props.shown.object_type_id}
            class="min-h-touch rounded-token border border-line px-3 aria-selected:bg-primary aria-selected:text-primary-ink"
            onClick={() => props.onChoose(each.object_type_id)}
          >
            {packageLabel(each.label_key)}
          </button>
        )}
      </For>
    </div>
  );
}

function RecordsBody() {
  const [kindId, setKindId] = createSignal<string | null>(null);
  const kind = () => {
    const kinds = customData().object_types ?? [];
    return kinds.find((held) => held.object_type_id === kindId()) ?? kinds[0];
  };
  const [records, setRecords] = createSignal<CustomDataResponse[] | null>(null);
  const [editing, setEditing] = createSignal<CustomDataResponse | "new" | null>(null);
  const [saved, setSaved] = createSignal(false);
  const read = createScreenRead(async () => {
    const shown = kind();
    return shown === undefined ? [] : (await api.customRecords(shown.object_type_id)).records;
  }, setRecords);
  createEffect(on(() => kind()?.object_type_id, () => void read.run()));

  const newRecord = () => (setSaved(false), setEditing("new"));
  const chooseRecord = (record: CustomDataResponse) => (setSaved(false), setEditing(record));
  const onSaved = (record: CustomDataResponse) => {
    setRecords((held) => [record, ...(held ?? []).filter((other) => other.custom_record_id !== record.custom_record_id)]);
    setEditing(null);
    setSaved(true);
  };

  return (
    <Show when={kind()} fallback={<p class="text-ink-muted">{t("records.none_declared")}</p>}>
      {(shown) => (
        <>
          <KindTabs shown={shown()} onChoose={(id) => (setKindId(id), setEditing(null))} />
          <Show when={can("data.record.write") && shown().deprecated !== true && editing() === null}>
            <button type="button" class="mt-4 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink" data-step="newRecord" onClick={() => newRecord()}>
              {t("records.new", { kind: packageLabel(shown().label_key) })}
            </button>
          </Show>
          <Show when={saved()}>
            <p class="mt-3 text-sm" role="status" data-outcome="record-saved">
              {t("records.saved")}
            </p>
          </Show>
          <Show when={editing()} keyed>
            {(form) => <RecordForm kind={shown()} record={form === "new" ? null : form} onSaved={onSaved} onCancel={() => setEditing(null)} />}
          </Show>
          <ReadFailed read={read} />
          <ul class="mt-4 divide-y divide-line">
            <For each={records() ?? []}>
              {(record) => (
                <li>
                  <button type="button" class="min-h-touch w-full px-2 py-3 text-left" data-step="chooseRecord" onClick={() => chooseRecord(record)}>
                    {summary(shown(), record) || t("records.no_values")}
                  </button>
                </li>
              )}
            </For>
          </ul>
        </>
      )}
    </Show>
  );
}

export function Records() {
  // The region the default package's `records` screen arranges (`../package/regions.tsx`).
  const regions: Regions = { records_body: () => <RecordsBody /> };
  return (
    <RegionsContext.Provider value={regions}>
      <PackageScreen screenId="records" host={NO_ACTS} />
    </RegionsContext.Provider>
  );
}
