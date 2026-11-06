// A custom field's value between the form that types it and the edge that keeps it (ADR-0194): what
// the form holds for each type, and the value the edge takes. No floating point: a quantity is typed
// in units and sent in thousandths, parsed from its digits.

import type { CustomField, CustomFieldValue, ValueRequest } from "../api/schemas";

/** What the form holds for one field: text for typed values, a flag for yes or no. */
export type Drafted = string | boolean;

const MILLI = 1000;

/** A quantity typed in units, to thousandths, without floating point: `1.5` is 1500. */
export function milli(typed: string): number | null {
  const match = /^(-?)(\d+)(?:\.(\d{1,3}))?$/.exec(typed.trim());
  if (match === null) {
    return null;
  }
  const whole = Number(match[2]) * MILLI + Number((match[3] ?? "").padEnd(3, "0"));
  return match[1] === "-" ? -whole : whole;
}

/** A quantity in thousandths, as it is typed: `1500` is `1.500`. */
function units(quantity: number): string {
  const sign = quantity < 0 ? "-" : "";
  const absolute = Math.abs(quantity);
  return `${sign}${Math.trunc(absolute / MILLI)}.${String(absolute % MILLI).padStart(3, "0")}`;
}

/** A field's value as the edge sent it, as the form holds it. */
export function drafted(field: CustomField, value: CustomFieldValue | undefined): Drafted {
  if (field.field_type === "CUSTOM_FIELD_TYPE_BOOL") {
    return value?.boolean ?? false;
  }
  const quantity = value?.quantity?.milli;
  if (quantity !== undefined) {
    return units(quantity);
  }
  return value?.integer?.toString() ?? value?.list_entry_id ?? value?.calendar_date ?? "";
}

/** How each typed field's text becomes the value the edge takes, or `undefined` when it cannot. */
const TYPED: Readonly<Record<string, (typed: string, raw: string) => Partial<ValueRequest> | undefined>> = {
  CUSTOM_FIELD_TYPE_INT: (typed) => (/^-?\d+$/.test(typed) ? { integer: Number(typed) } : undefined),
  CUSTOM_FIELD_TYPE_CHOICE: (typed) => ({ list_entry_id: typed }),
  CUSTOM_FIELD_TYPE_DATE: (typed) => (/^\d{4}-\d{2}-\d{2}$/.test(typed) ? { calendar_date: typed } : undefined),
  CUSTOM_FIELD_TYPE_QUANTITY: (typed) => {
    const amount = milli(typed);
    return amount === null ? undefined : { quantity: { milli: amount } };
  },
  CUSTOM_FIELD_TYPE_TEXT: (_typed, raw) => ({ text: raw }),
};

/** The value the edge takes for one field's draft, or `"invalid"` for one it could not. Empty clears it. */
export function wire(field: CustomField, draft: Drafted): ValueRequest | "invalid" {
  const base: ValueRequest = { custom_field_id: field.custom_field_id };
  if (typeof draft === "boolean") {
    return { ...base, boolean: draft };
  }
  const typed = draft.trim();
  if (typed === "") {
    return base;
  }
  const value = TYPED[field.field_type]?.(typed, draft);
  return value === undefined ? "invalid" : { ...base, ...value };
}
