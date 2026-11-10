// A package's data part as the data designer edits it
// ([ADR-0194](../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md),
// [ADR-0206](../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)):
// custom fields on the objects a store already has, lists a choice field draws from, and object
// types of HQ's own with their fields.
//
// Every new field says whether it is personal, and the designer never assumes: a field starts with
// the question unanswered, and `fieldProblem` holds it until it is answered, and, for a personal
// one, until its retention and consent purpose are given. Data only ever grows (ADR-0194 decision
// 6): what an earlier version declared is retired with `deprecated`, never removed or retyped, and
// the cloud refuses a draft that does otherwise.

import type { CustomField, CustomList, PackageData } from "../api/schemas";
import type { MessageKey } from "../i18n";

export type ObjectType = NonNullable<PackageData["object_types"]>[number];

/** The kinds of value a field holds, in the order a picker lists them. */
export const FIELD_TYPES: readonly { readonly value: string; readonly key: MessageKey }[] = [
  { value: "CUSTOM_FIELD_TYPE_TEXT", key: "data.type.text" },
  { value: "CUSTOM_FIELD_TYPE_INT", key: "data.type.int" },
  { value: "CUSTOM_FIELD_TYPE_BOOL", key: "data.type.bool" },
  { value: "CUSTOM_FIELD_TYPE_CHOICE", key: "data.type.choice" },
  { value: "CUSTOM_FIELD_TYPE_DATE", key: "data.type.date" },
  { value: "CUSTOM_FIELD_TYPE_QUANTITY", key: "data.type.quantity" },
];

/** The objects a store already has that a field can be added to. */
export const DATA_OBJECTS: readonly { readonly value: string; readonly key: MessageKey }[] = [
  { value: "DATA_OBJECT_ORDER", key: "data.object.order" },
  { value: "DATA_OBJECT_LINE", key: "data.object.line" },
  { value: "DATA_OBJECT_BILL", key: "data.object.bill" },
  { value: "DATA_OBJECT_TABLE", key: "data.object.table" },
  { value: "DATA_OBJECT_SHIFT", key: "data.object.shift" },
];

/** A new text field `id`, on `object` when it is a field of an existing object; personal unanswered. */
export function newField(id: string, object?: string): CustomField {
  return {
    custom_field_id: id,
    field_type: "CUSTOM_FIELD_TYPE_TEXT",
    label_key: `field.${id}`,
    ...(object === undefined ? {} : { object }),
  };
}

/** A new list `id` with no entries. */
export const newList = (id: string): CustomList => ({ custom_list_id: id, label_key: `list.${id}`, entries: [] });

/** A new object type `id` with no fields. */
export const newObjectType = (id: string): ObjectType => ({ object_type_id: id, label_key: `object.${id}`, fields: [] });

/**
 * `field` with `change`, keeping its shape consistent: a field that is not personal keeps no
 * retention or consent, a field that is not a choice no list, and one that is not a quantity no unit.
 */
export function changeField(field: CustomField, change: Partial<CustomField>): CustomField {
  const next: CustomField = { ...field, ...change };
  if (next.personal !== true) {
    delete next.retention_days;
    delete next.consent_purpose_key;
  }
  if (next.field_type !== "CUSTOM_FIELD_TYPE_CHOICE") {
    delete next.custom_list_id;
  }
  if (next.field_type !== "CUSTOM_FIELD_TYPE_QUANTITY") {
    delete next.unit_key;
  }
  return next;
}

/** Why `field` cannot be published as it stands, as an i18n key, or `null`. */
export function fieldProblem(field: CustomField): MessageKey | null {
  if (field.personal === undefined) {
    return "data.problem.personalUnanswered";
  }
  if (field.personal && (field.retention_days === undefined || field.retention_days < 1)) {
    return "data.problem.retention";
  }
  if (field.personal && (field.consent_purpose_key ?? "").trim() === "") {
    return "data.problem.consent";
  }
  if (field.field_type === "CUSTOM_FIELD_TYPE_CHOICE" && (field.custom_list_id ?? "") === "") {
    return "data.problem.list";
  }
  return null;
}
