// The i18n runtime (ADR-0020): ICU MessageFormat over the platform `Intl`, with `en` the enforced
// floor. `t(key, args)` reads the active locale (a signal, so a locale change re-renders every
// caller), formats the message with intl-messageformat, and falls back to the English catalogue for
// any key a translation is missing — never a blank, never a raw key on a merged build. `en.json` is
// the canonical key list; `MessageKey` makes a typo a type error.

import { createSignal } from "solid-js";
import { IntlMessageFormat } from "intl-messageformat";

import en from "./en.json";
import vi from "./vi.json";

export type MessageKey = keyof typeof en;
export type Locale = "en" | "vi";

export const LOCALES: readonly Locale[] = ["en", "vi"];

const CATALOGUES: Record<Locale, Record<string, string>> = { en, vi };

const [locale, setLocaleSignal] = createSignal<Locale>("en");
export { locale };

export function setLocale(next: Locale): void {
  setLocaleSignal(next);
  document.documentElement.lang = next;
}

// Compiled formatters are cached by locale+key: intl-messageformat parses the ICU pattern once.
const cache = new Map<string, IntlMessageFormat>();

function formatter(active: Locale, key: MessageKey): IntlMessageFormat {
  const cacheKey = `${active}:${key}`;
  const cached = cache.get(cacheKey);
  if (cached !== undefined) {
    return cached;
  }
  // en is the floor: a key missing from the active catalogue resolves to English.
  const message = CATALOGUES[active][key] ?? en[key];
  const compiled = new IntlMessageFormat(message, active);
  cache.set(cacheKey, compiled);
  return compiled;
}

export function t(key: MessageKey, args?: Record<string, string | number>): string {
  const active = locale();
  const formatted = formatter(active, key).format(args);
  return typeof formatted === "string" ? formatted : String(formatted);
}
