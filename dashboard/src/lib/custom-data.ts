// What the custom-data report may be asked, read from the tenant's package versions (ADR-0194):
// the objects and object types, the fields a report may group or filter by, and their labels.
//
// A field any version declares personal, or leaves undeclared, is never offered: a report groups
// and filters by non-personal fields only (decision 7), and the server refuses one anyway.

import type {
  CustomField,
  CustomFieldValue,
  CustomList,
  PackageVersionRecord,
  TranslationGrid,
} from "../api/schemas";
import type { MessageKey } from "../i18n";

/** What a summary counts: an existing object's kind, or a package's object type. */
export type ReportTarget = {
  readonly id: string;
  /** The console's own key for an existing object's kind; absent for an object type. */
  readonly messageKey?: MessageKey;
  /** The tenant's key for an object type's name; absent for an existing object's kind. */
  readonly labelKey?: string;
  readonly object?: string;
  readonly objectTypeId?: string;
  readonly fields: readonly CustomField[];
};

/** The field types a filter can take a value for. */
const FILTERABLE = new Set(["CUSTOM_FIELD_TYPE_CHOICE", "CUSTOM_FIELD_TYPE_BOOL", "CUSTOM_FIELD_TYPE_INT"]);

/** The existing objects' label keys, in the console's own catalogue. */
const OBJECT_KEYS: Readonly<Record<string, MessageKey>> = {
  DATA_OBJECT_ORDER: "customData.objectOrder",
  DATA_OBJECT_LINE: "customData.objectLine",
  DATA_OBJECT_BILL: "customData.objectBill",
  DATA_OBJECT_TABLE: "customData.objectTable",
  DATA_OBJECT_SHIFT: "customData.objectShift",
};

/** The ids of every field any version declares other than plainly not personal. */
function personalIds(versions: readonly PackageVersionRecord[]): Set<string> {
  const ids = new Set<string>();
  for (const field of allFields(versions)) {
    if (field.personal !== false) {
      ids.add(field.custom_field_id);
    }
  }
  return ids;
}

function allFields(versions: readonly PackageVersionRecord[]): CustomField[] {
  return versions.flatMap((version) => [
    ...(version.content.data?.custom_fields ?? []),
    ...(version.content.data?.object_types ?? []).flatMap((kind) => kind.fields),
  ]);
}

/** Adds `field` to `into` unless a field of that id is there, so the latest version's wins. */
function keepLatest(into: Map<string, CustomField>, field: CustomField): void {
  into.set(field.custom_field_id, field);
}

type Versions = readonly PackageVersionRecord[];

/** The non-personal fields on each existing object's kind, the latest version's winning. */
function objectFields(versions: Versions, personal: Set<string>): Map<string, Map<string, CustomField>> {
  const objects = new Map<string, Map<string, CustomField>>();
  const fields = versions.flatMap((version) => version.content.data?.custom_fields ?? []);
  for (const field of fields) {
    const object = field.object ?? "";
    if (!personal.has(field.custom_field_id) && OBJECT_KEYS[object]) {
      const kept = objects.get(object) ?? new Map<string, CustomField>();
      keepLatest(kept, field);
      objects.set(object, kept);
    }
  }
  return objects;
}

type TypeEntry = { labelKey: string; fields: Map<string, CustomField> };

/** The non-personal fields of each object type, with its latest name. */
function typeFields(versions: Versions, personal: Set<string>): Map<string, TypeEntry> {
  const types = new Map<string, TypeEntry>();
  const kinds = versions.flatMap((version) => version.content.data?.object_types ?? []);
  for (const kind of kinds) {
    const entry = types.get(kind.object_type_id) ?? { labelKey: kind.label_key, fields: new Map() };
    entry.labelKey = kind.label_key;
    kind.fields
      .filter((field) => !personal.has(field.custom_field_id))
      .forEach((field) => keepLatest(entry.fields, field));
    types.set(kind.object_type_id, entry);
  }
  return types;
}

/** The targets a report may count, each with the non-personal fields it may group by. */
export function reportTargets(versions: Versions): ReportTarget[] {
  const personal = personalIds(versions);
  const objects = [...objectFields(versions, personal)].map(([object, fields]) => ({
    id: object,
    messageKey: OBJECT_KEYS[object],
    object,
    fields: [...fields.values()],
  }));
  const types = [...typeFields(versions, personal)].map(([objectTypeId, entry]) => ({
    id: objectTypeId,
    labelKey: entry.labelKey,
    objectTypeId,
    fields: [...entry.fields.values()],
  }));
  return [...objects, ...types].filter((target) => target.fields.length > 0);
}

/** Whether a filter can take a value for `field`. */
export function filterable(field: CustomField): boolean {
  return FILTERABLE.has(field.field_type);
}

/** Every list any version declares, by id, the latest version's entries winning. */
export function listsById(versions: readonly PackageVersionRecord[]): Map<string, CustomList> {
  const lists = new Map<string, CustomList>();
  for (const version of versions) {
    for (const list of version.content.data?.lists ?? []) {
      lists.set(list.custom_list_id, list);
    }
  }
  return lists;
}

/** A tenant label from the translation grid in `locale`, then in English, then the key itself. */
export function gridLabel(grid: TranslationGrid, key: string, locale: string): string {
  const row = grid[key];
  return row?.[locale] || row?.en || key;
}

/** A quantity in thousandths, written as a decimal without floating point: `1500` is `1.5`. */
export function formatMilli(milli: number): string {
  const sign = milli < 0 ? "-" : "";
  const whole = Math.trunc(Math.abs(milli) / 1000);
  const fraction = String(Math.abs(milli) % 1000).padStart(3, "0").replace(/0+$/, "");
  return fraction ? `${sign}${whole}.${fraction}` : `${sign}${whole}`;
}

/** What a value's words are drawn from. */
export type ValueWords = {
  readonly lists: ReadonlyMap<string, CustomList>;
  readonly fields: readonly CustomField[];
  readonly label: (key: string) => string;
  readonly yes: string;
  readonly no: string;
};

/** A list entry's label, or its id for an entry no version declares. */
function entryText(value: CustomFieldValue, words: ValueWords, entryId: string): string {
  const field = words.fields.find((each) => each.custom_field_id === value.custom_field_id);
  const list = words.lists.get(field?.custom_list_id ?? "");
  const entry = list?.entries.find((each) => each.list_entry_id === entryId);
  return entry ? words.label(entry.label_key) : entryId;
}

/** A value in words: a list entry's label, yes or no, a quantity as a decimal, or the number or date. */
export function valueText(value: CustomFieldValue, words: ValueWords): string {
  if (value.list_entry_id) {
    return entryText(value, words, value.list_entry_id);
  }
  if (value.boolean !== undefined) {
    return value.boolean ? words.yes : words.no;
  }
  if (value.quantity !== undefined) {
    return formatMilli(value.quantity.milli);
  }
  return String(value.integer ?? value.calendar_date ?? "");
}
