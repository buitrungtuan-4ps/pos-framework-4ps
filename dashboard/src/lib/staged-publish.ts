// Stage a change to one group of settings, then publish it to the place chosen (ADR-0246 decision 3):
// one store, a store group, a brand or every store. Nothing reaches a store while it is staged; the
// publish writes each staged value at that one place through the ordinary setting write, which checks
// it against the register, publishes it to the stores that place reaches and no other, and audits it.

import type { SettingDefinition } from "../api/schemas";
import type { Json, SettingScope } from "../api/types";

/** One value staged for a setting of the group. */
export interface Staged {
  readonly key: string;
  readonly value: Json;
}

/** Where the staged values are published. */
export interface Target {
  readonly scope: SettingScope;
  readonly scopeId: string;
}

/** The settings of the register's `node`, in the register's order. */
export function groupSettings(catalogue: readonly SettingDefinition[], node: string): SettingDefinition[] {
  return catalogue.filter((setting) => setting.node === node);
}

/** The groups the register has, each once, in the register's order. */
export function groupsOf(catalogue: readonly SettingDefinition[]): string[] {
  return [...new Set(catalogue.map((setting) => setting.node))];
}

/**
 * The values a choice may be staged with here: every one but a value that needs a recorded
 * confirmation, which the Settings screen asks for (ADR-0169).
 */
export function stageableValues(setting: SettingDefinition): string[] {
  return (setting.values ?? []).filter((value) => value !== setting.confirm?.value);
}

/**
 * The writes a publish makes: each staged value whose setting may be written at the target's scope.
 * A staged value of a setting the scope does not take is answered as skipped, by key, rather than
 * written somewhere else.
 */
export function writesFor(
  catalogue: readonly SettingDefinition[],
  staged: readonly Staged[],
  target: Target,
): { writes: Staged[]; skipped: string[] } {
  const writes: Staged[] = [];
  const skipped: string[] = [];
  for (const entry of staged) {
    const setting = catalogue.find((candidate) => candidate.setting_key === entry.key);
    if (setting && setting.scopes.includes(target.scope)) {
      writes.push(entry);
    } else {
      skipped.push(entry.key);
    }
  }
  return { writes, skipped };
}

/** `staged` with `key` set to `value`, or taken out when `value` is `undefined` (left unchanged). */
export function withStaged(
  staged: ReadonlyMap<string, Json>,
  key: string,
  value: Json | undefined,
): ReadonlyMap<string, Json> {
  const next = new Map(staged);
  if (value === undefined) {
    next.delete(key);
  } else {
    next.set(key, value);
  }
  return next;
}
