// The settings history in words, and a rollback that touches only the scope chosen
// (ADR-0246 decisions 2 to 4).
//
// Every write of a setting is audited with the person, the slot (`setting_key SCOPE scope_id`) and
// the value before and after. Those entries are the revisions: a rollback reads them, restores the
// value each slot held before the chosen change, and writes it back through the ordinary setting
// write, which validates it against the register now, publishes it and audits it as a new change.
// So a later publish keeps it, and a value the register refuses now is refused, naming the setting.

import type { AuditEntry } from "../api/schemas";
import type { Json, SettingScope } from "../api/types";

/** One change to one setting at one place. `undefined` is "not set there". */
export interface SettingChange {
  readonly id: string;
  readonly atMs: number;
  readonly actorEmail: string;
  readonly key: string;
  readonly scope: SettingScope;
  readonly scopeId: string;
  readonly before: Json | undefined;
  readonly after: Json | undefined;
}

/** What a rollback writes: a value back at its slot, or `undefined` to clear the slot. */
export interface Restore {
  readonly key: string;
  readonly scope: SettingScope;
  readonly scopeId: string;
  readonly value: Json | undefined;
}

/** Which settings a rollback takes: one setting, one group (a node such as `einvoice`), or all. */
export type What =
  | { readonly kind: "all" }
  | { readonly kind: "group"; readonly node: string }
  | { readonly kind: "setting"; readonly key: string };

/** Where: the place a value is written for, by its scope and id. */
export interface Where {
  readonly scope: SettingScope;
  readonly scopeId: string;
}

const SCOPES: readonly SettingScope[] = [
  "SETTING_SCOPE_TENANT",
  "SETTING_SCOPE_BRAND",
  "SETTING_SCOPE_STORE_GROUP",
  "SETTING_SCOPE_STORE",
];

function valueOf(side: unknown): Json | undefined {
  if (side !== null && typeof side === "object" && !Array.isArray(side) && "value" in side) {
    return (side as { value: Json }).value;
  }
  return undefined;
}

/** A `setting.set` or `setting.clear` audit entry as a change; anything else is `null`. */
export function changeOf(entry: AuditEntry): SettingChange | null {
  if (entry.entity_type !== "setting" || (entry.action !== "setting.set" && entry.action !== "setting.clear")) {
    return null;
  }
  const [key, scope, scopeId] = entry.entity_id.split(" ");
  const known = SCOPES.find((candidate) => candidate === scope);
  if (!key || !known || !scopeId) {
    return null;
  }
  return {
    id: entry.id,
    atMs: entry.at_ms,
    actorEmail: entry.actor_email,
    key,
    scope: known,
    scopeId,
    before: valueOf(entry.before),
    after: entry.action === "setting.clear" ? undefined : valueOf(entry.after),
  };
}

/** The node a setting belongs to: `einvoice` for `einvoice.correction_kind`. */
export function nodeOf(key: string): string {
  return key.split(".")[0] ?? key;
}

/** Whether `change` is one `what` takes. */
export function takes(what: What, change: SettingChange): boolean {
  switch (what.kind) {
    case "all":
      return true;
    case "group":
      return nodeOf(change.key) === what.node;
    case "setting":
      return change.key === what.key;
  }
}

/** Whether `change` was written at `where`. Ids compare without case, as the cloud's do. */
export function at(where: Where, change: SettingChange): boolean {
  return change.scope === where.scope && change.scopeId.toUpperCase() === where.scopeId.toUpperCase();
}

/**
 * What rolling back to just before `from` writes, for the settings `what` takes at `where`: each
 * slot changed at or after `from` goes back to the value it held before its earliest such change.
 * A slot whose value then is what it holds now needs no write and is left out.
 */
export function restoresFor(
  changes: readonly SettingChange[],
  from: SettingChange,
  what: What,
  where: Where,
): Restore[] {
  const after = changes
    .filter((change) => at(where, change) && takes(what, change))
    .filter(
      (change) =>
        change.atMs > from.atMs || change.id === from.id || (change.atMs === from.atMs && change.id > from.id),
    )
    .sort((a, b) => a.atMs - b.atMs || a.id.localeCompare(b.id));
  const earliest = new Map<string, SettingChange>();
  const latest = new Map<string, SettingChange>();
  for (const change of after) {
    if (!earliest.has(change.key)) {
      earliest.set(change.key, change);
    }
    latest.set(change.key, change);
  }
  const restores: Restore[] = [];
  for (const [key, first] of earliest) {
    const now = latest.get(key)?.after;
    if (JSON.stringify(now) === JSON.stringify(first.before)) {
      continue;
    }
    restores.push({
      key,
      scope: first.scope,
      scopeId: first.scopeId,
      value: first.before,
    });
  }
  return restores.sort((a, b) => a.key.localeCompare(b.key));
}
