// The tenant's labels for the translation keys the running package names (ADR-0043): what
// `GET /api/package` answers as `running.labels`, `key → { locale → text }`, every key with `en`.
// A package's label shows from the till's own catalogue where it has the key, then from these in the
// till's language, then in English, and otherwise draws nothing: never the raw key.

import { createSignal } from "solid-js";

import { locale } from "../i18n";

type Labels = Readonly<Record<string, Readonly<Record<string, string>>>>;

const [labels, setLabels] = createSignal<Labels>({});

/** Takes the labels a `GET /api/package` answer carries, or none. */
export function applyLabels(body: unknown): void {
  const running = (body as { running?: { labels?: Labels } | null } | null)?.running;
  setLabels(running?.labels ?? {});
}

/** `key`'s tenant label in the till's language, else in English, else `undefined`. */
export function tenantLabel(key: string): string | undefined {
  const texts = labels()[key];
  if (texts === undefined) {
    return undefined;
  }
  const text = texts[locale()] ?? texts["en"];
  return text === "" ? undefined : text;
}
