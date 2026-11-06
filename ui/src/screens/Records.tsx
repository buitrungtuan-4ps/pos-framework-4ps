// A package's custom records (ADR-0194, ADR-0206): its object types, each one's records, and the form
// that enters or changes one.
//
// The edge re-checks every value against the running package and is the authority; this form only
// offers what the package declares. A personal value is entered here and never shown again: after a
// save the field is empty, the record says that a value is held, and showing it takes
// `data.personal.read`, one subject at a time, every read audited by the edge. A personal value is
// sent only with its consent ticked, and the consent's wording is the tenant's label for its purpose.

import { For, Show, createEffect, createSignal, on } from "solid-js";

import { ApiError, api } from "../api/client";
import type {
  CustomDataResponse,
  CustomField,
  CustomFieldValue,
  ObjectType,
  ValueRequest,
} from "../api/schemas";
import { ReadFailed } from "../components/StoreData";
import { t, type MessageKey } from "../i18n";
import { RegionsContext, type Regions } from "../package/regions";
import { packageLabel } from "../package/render";
import { NO_ACTS, PackageScreen } from "../package/screen";
import { customData } from "../state/custom-data";
import { can, holdsOwn } from "../state/permissions";
import { createScreenRead } from "../state/screen-read";

/** What the form holds for one field: text for typed values, a flag for yes or no. */
type Draft = Readonly<Record<string, string | boolean>>;

const MILLI = 1000;

/** A quantity typed in units, to thousandths, without floating point: `1.5` is 1500. */
function milli(typed: string): number | null {
  const match = /^(-?)(\d+)(?:\.(\d{1,3}))?$/.exec(typed.trim());
  if (match === null) {
    return null;
  }
  const whole = Number(match[2]) * MILLI + Number((match[3] ?? "").padEnd(3, "0"));
  return match[1] === "-" ? -whole : whole;
}

/** A field's value as the edge sent it, as the form holds it. */
function drafted(field: CustomField, value: CustomFieldValue | undefined): string | boolean {
  if (field.field_type === "CUSTOM_FIELD_TYPE_BOOL") {
    return value?.boolean ?? false;
  }
  if (value === undefined) {
    return "";
  }
  const quantity = value.quantity?.milli;
  return (
    value.integer?.toString() ??
    value.list_entry_id ??
    value.calendar_date ??
    (quantity === undefined ? "" : `${Math.trunc(quantity / MILLI)}.${String(Math.abs(quantity % MILLI)).padStart(3, "0")}`)
  );
}

/** The value the edge takes for one field's draft, `"invalid"` for one it could not, or `null` to clear it. */
function wire(field: CustomField, draft: string | boolean): ValueRequest | "invalid" {
  const base: ValueRequest = { custom_field_id: field.custom_field_id };
  if (typeof draft === "boolean") {
    return { ...base, boolean: draft };
  }
  const typed = draft.trim();
  if (typed === "") {
    return base;
  }
  switch (field.field_type) {
    case "CUSTOM_FIELD_TYPE_INT":
      return /^-?\d+$/.test(typed) ? { ...base, integer: Number(typed) } : "invalid";
    case "CUSTOM_FIELD_TYPE_CHOICE":
      return { ...base, list_entry_id: typed };
    case "CUSTOM_FIELD_TYPE_DATE":
      return /^\d{4}-\d{2}-\d{2}$/.test(typed) ? { ...base, calendar_date: typed } : "invalid";
    case "CUSTOM_FIELD_TYPE_QUANTITY": {
      const amount = milli(typed);
      return amount === null ? "invalid" : { ...base, quantity: { milli: amount } };
    }
    case "CUSTOM_FIELD_TYPE_TEXT":
      return { ...base, text: draft };
    default:
      return "invalid";
  }
}

/** Why a save was refused, as the operator reads it. */
function refusalKey(caught: unknown): MessageKey {
  if (caught instanceof ApiError && caught.reason === "CUSTOM_ENTRY_REFUSED") {
    return "records.refused_entry";
  }
  if (caught instanceof ApiError && caught.reason === "PERMISSION_DENIED") {
    return "records.refused_permission";
  }
  return "records.save_failed";
}

/** One field's input, in the form its type takes. */
function FieldInput(props: {
  field: CustomField;
  value: string | boolean;
  onInput: (value: string | boolean) => void;
}) {
  const id = () => `field-${props.field.custom_field_id}`;
  const entries = () =>
    (customData().lists ?? [])
      .find((list) => list.custom_list_id === props.field.custom_list_id)
      ?.entries.filter((entry) => entry.deprecated !== true) ?? [];
  const label = () => packageLabel(props.field.label_key);
  return (
    <label class="mt-3 block text-sm" for={id()}>
      <span class="font-semibold">{label()}</span>
      <Show when={props.field.required === true}>
        <span class="text-ink-muted"> {t("records.required")}</span>
      </Show>
      {(() => {
        switch (props.field.field_type) {
          case "CUSTOM_FIELD_TYPE_BOOL":
            return (
              <input
                id={id()}
                type="checkbox"
                class="ml-3 h-6 w-6 align-middle"
                checked={props.value === true}
                onChange={(event) => props.onInput(event.currentTarget.checked)}
              />
            );
          case "CUSTOM_FIELD_TYPE_CHOICE":
            return (
              <select
                id={id()}
                class="mt-1 block min-h-touch w-full rounded-token border border-line bg-surface px-3"
                value={String(props.value)}
                onChange={(event) => props.onInput(event.currentTarget.value)}
              >
                <option value="">{t("records.none_chosen")}</option>
                <For each={entries()}>
                  {(entry) => <option value={entry.list_entry_id}>{packageLabel(entry.label_key)}</option>}
                </For>
              </select>
            );
          case "CUSTOM_FIELD_TYPE_TEXT":
            return (
              <textarea
                id={id()}
                class="mt-1 block w-full rounded-token border border-line bg-surface p-3"
                maxLength={1024}
                autocomplete="off"
                value={String(props.value)}
                onInput={(event) => props.onInput(event.currentTarget.value)}
              />
            );
          default:
            return (
              <input
                id={id()}
                type={props.field.field_type === "CUSTOM_FIELD_TYPE_DATE" ? "date" : "text"}
                inputMode={props.field.field_type === "CUSTOM_FIELD_TYPE_DATE" ? undefined : "decimal"}
                autocomplete="off"
                class="mt-1 block min-h-touch w-full rounded-token border border-line bg-surface px-3"
                value={String(props.value)}
                onInput={(event) => props.onInput(event.currentTarget.value)}
              />
            );
        }
      })()}
    </label>
  );
}

/** The form that enters a new record of `kind`, or changes `record`. */
function RecordForm(props: {
  kind: ObjectType;
  record: CustomDataResponse | null;
  onSaved: (saved: CustomDataResponse) => void;
  onCancel: () => void;
}) {
  const fields = () => props.kind.fields.filter((field) => field.deprecated !== true);
  const initial = (): Draft =>
    Object.fromEntries(
      fields().map((field) => [
        field.custom_field_id,
        field.personal === true
          ? ""
          : drafted(field, props.record?.values.find((value) => value.custom_field_id === field.custom_field_id)),
      ]),
    );
  const [draft, setDraft] = createSignal<Draft>(initial());
  const [changed, setChanged] = createSignal<ReadonlySet<string>>(new Set());
  const [consented, setConsented] = createSignal<ReadonlySet<string>>(new Set());
  const [shown, setShown] = createSignal<Readonly<Record<string, string>> | null>(null);
  const [error, setError] = createSignal<MessageKey | null>(null);
  const [saving, setSaving] = createSignal(false);

  const enter = (field: CustomField, value: string | boolean) => {
    setDraft((held) => ({ ...held, [field.custom_field_id]: value }));
    setChanged((held) => new Set([...held, field.custom_field_id]));
  };
  // The consents the personal values typed need, each once.
  const purposes = () => [
    ...new Set(
      fields()
        .filter((field) => field.personal === true && String(draft()[field.custom_field_id] ?? "").trim() !== "")
        .map((field) => field.consent_purpose_key)
        .filter((key): key is string => key !== undefined),
    ),
  ];
  const consentGiven = () => purposes().every((key) => consented().has(key));

  const showPersonal = async () => {
    const subjectId = props.record?.subject_id;
    if (subjectId === undefined) {
      return;
    }
    try {
      setShown((await api.subject(subjectId)).fields);
    } catch (caught) {
      setError(refusalKey(caught));
    }
  };

  const saveRecord = async () => {
    const sent = fields().filter((field) => props.record === null || changed().has(field.custom_field_id));
    const values: ValueRequest[] = [];
    for (const field of sent) {
      const value = wire(field, draft()[field.custom_field_id] ?? "");
      if (value === "invalid") {
        setError("records.invalid_value");
        return;
      }
      values.push(value);
    }
    setError(null);
    setSaving(true);
    const body = {
      object_type_id: props.kind.object_type_id,
      values,
      consented_purpose_keys: purposes(),
    };
    try {
      const saved =
        props.record?.custom_record_id === undefined
          ? await api.createCustomRecord(body)
          : await api.changeCustomRecord(props.record.custom_record_id, body);
      props.onSaved(saved);
    } catch (caught) {
      setError(refusalKey(caught));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div class="mt-4 rounded-token border border-line bg-surface p-4" data-outcome="record-form">
      <For each={fields()}>
        {(field) => (
          <>
            <FieldInput
              field={field}
              value={draft()[field.custom_field_id] ?? ""}
              onInput={(value) => enter(field, value)}
            />
            <Show when={field.personal === true && props.record?.subject_id !== undefined}>
              <p class="mt-1 text-sm text-ink-muted" data-outcome="personal-held">
                {shown()?.[field.custom_field_id] ?? t("records.personal_held")}
              </p>
            </Show>
          </>
        )}
      </For>
      <Show when={props.record?.subject_id !== undefined && shown() === null && holdsOwn("data.personal.read")}>
        <button
          type="button"
          class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
          data-step="showPersonal"
          onClick={() => void showPersonal()}
        >
          {t("records.show_personal")}
        </button>
      </Show>
      <For each={purposes()}>
        {(purpose) => (
          <label class="mt-3 flex min-h-touch items-center gap-3 text-sm">
            <input
              type="checkbox"
              class="h-6 w-6"
              checked={consented().has(purpose)}
              onChange={(event) => {
                const ticked = event.currentTarget.checked;
                setConsented((held) => {
                  const next = new Set(held);
                  if (ticked) {
                    next.add(purpose);
                  } else {
                    next.delete(purpose);
                  }
                  return next;
                });
              }}
            />
            <span>{t("records.consent", { purpose: packageLabel(purpose) })}</span>
          </label>
        )}
      </For>
      <Show when={error()}>
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
          disabled={saving() || !consentGiven()}
          data-step="saveRecord"
          onClick={() => void saveRecord()}
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

/** One record in a list: its non-personal values in the type's order. */
function summary(kind: ObjectType, record: CustomDataResponse): string {
  return kind.fields
    .filter((field) => field.personal !== true)
    .map((field) => {
      const value = record.values.find((held) => held.custom_field_id === field.custom_field_id);
      if (value === undefined) {
        return null;
      }
      if (value.list_entry_id !== undefined) {
        const entry = (customData().lists ?? [])
          .flatMap((list) => list.entries)
          .find((held) => held.list_entry_id === value.list_entry_id);
        return packageLabel(entry?.label_key);
      }
      if (value.boolean !== undefined) {
        return value.boolean ? packageLabel(field.label_key) : null;
      }
      return String(drafted(field, value));
    })
    .filter((text): text is string => text !== null && text !== "")
    .join(" · ");
}

function RecordsBody() {
  const kinds = () => customData().object_types ?? [];
  const [kindId, setKindId] = createSignal<string | null>(null);
  const kind = () => kinds().find((held) => held.object_type_id === kindId()) ?? kinds()[0];
  const [records, setRecords] = createSignal<CustomDataResponse[] | null>(null);
  const [editing, setEditing] = createSignal<CustomDataResponse | "new" | null>(null);
  const [saved, setSaved] = createSignal(false);
  const read = createScreenRead(async () => {
    const shown = kind();
    return shown === undefined ? [] : (await api.customRecords(shown.object_type_id)).records;
  }, setRecords);
  createEffect(on(() => kind()?.object_type_id, () => void read.run()));

  const newRecord = () => {
    setSaved(false);
    setEditing("new");
  };
  const chooseRecord = (record: CustomDataResponse) => {
    setSaved(false);
    setEditing(record);
  };
  const onSaved = (record: CustomDataResponse) => {
    setRecords((held) => [record, ...(held ?? []).filter((other) => other.custom_record_id !== record.custom_record_id)]);
    setEditing(null);
    setSaved(true);
  };

  return (
    <Show when={kind()} fallback={<p class="text-ink-muted">{t("records.none_declared")}</p>}>
      {(shown) => (
        <>
          <div class="flex flex-wrap gap-2" role="tablist">
            <For each={kinds()}>
              {(each) => (
                <button
                  type="button"
                  role="tab"
                  aria-selected={each.object_type_id === shown().object_type_id}
                  class="min-h-touch rounded-token border border-line px-3 aria-selected:bg-primary aria-selected:text-primary-ink"
                  onClick={() => (setKindId(each.object_type_id), setEditing(null))}
                >
                  {packageLabel(each.label_key)}
                </button>
              )}
            </For>
          </div>
          <Show when={can("data.record.write") && shown().deprecated !== true && editing() === null}>
            <button
              type="button"
              class="mt-4 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink"
              data-step="newRecord"
              onClick={() => newRecord()}
            >
              {t("records.new", { kind: packageLabel(shown().label_key) })}
            </button>
          </Show>
          <Show when={saved()}>
            <p class="mt-3 text-sm" role="status" data-outcome="record-saved">
              {t("records.saved")}
            </p>
          </Show>
          <Show when={editing()}>
            {(form) => (
              <RecordForm
                kind={shown()}
                record={form() === "new" ? null : (form() as CustomDataResponse)}
                onSaved={onSaved}
                onCancel={() => setEditing(null)}
              />
            )}
          </Show>
          <ReadFailed read={read} />
          <ul class="mt-4 divide-y divide-line">
            <For each={records() ?? []}>
              {(record) => (
                <li>
                  <button
                    type="button"
                    class="min-h-touch w-full px-2 py-3 text-left"
                    data-step="chooseRecord"
                    onClick={() => chooseRecord(record)}
                  >
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
