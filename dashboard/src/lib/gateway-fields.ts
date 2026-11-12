// The gateway endpoint form's two text inputs (ADR-0198): the fields a request is built from, or a
// response is read into, one per line as `name kind` with an optional `required`; and secret
// headers, one per line as `Name: value`. The cloud checks names and kinds again; this only turns
// the text into the request's shape and says which line it could not read.

import type { GatewayField } from "../api/schemas";

const KINDS: Readonly<Record<string, GatewayField["kind"]>> = {
  text: "GATEWAY_FIELD_KIND_STRING",
  number: "GATEWAY_FIELD_KIND_INTEGER",
  "yes-no": "GATEWAY_FIELD_KIND_BOOLEAN",
};

/** The fields `text` declares, or the 1-based line it could not read. */
export function fieldsFromText(text: string): { fields: GatewayField[] } | { badLine: number } {
  const fields: GatewayField[] = [];
  const lines = text.split("\n");
  for (const [index, raw] of lines.entries()) {
    const words = raw.trim().split(/\s+/).filter((word) => word !== "");
    if (words.length === 0) {
      continue;
    }
    const [name, kindWord, flag, ...rest] = words;
    const kind = kindWord === undefined ? undefined : KINDS[kindWord];
    if (name === undefined || kind === undefined || rest.length > 0 || (flag !== undefined && flag !== "required")) {
      return { badLine: index + 1 };
    }
    fields.push({ name, kind, required: flag === "required" });
  }
  return { fields };
}

/** The text a field list is edited as. */
export function fieldsToText(fields: readonly GatewayField[]): string {
  const word = (kind: GatewayField["kind"]) => Object.entries(KINDS).find(([, value]) => value === kind)?.[0] ?? "text";
  return fields.map((field) => `${field.name} ${word(field.kind)}${field.required === true ? " required" : ""}`).join("\n");
}

/** The secret headers `text` sets, or the 1-based line it could not read. */
export function headersFromText(text: string): { headers: Record<string, string> } | { badLine: number } {
  const headers: Record<string, string> = {};
  for (const [index, raw] of text.split("\n").entries()) {
    const line = raw.trim();
    if (line === "") {
      continue;
    }
    const colon = line.indexOf(":");
    const name = colon > 0 ? line.slice(0, colon).trim() : "";
    const value = colon > 0 ? line.slice(colon + 1).trim() : "";
    if (name === "" || value === "") {
      return { badLine: index + 1 };
    }
    headers[name] = value;
  }
  return { headers };
}
