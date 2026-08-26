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

// A language's own name, shown in the switcher. An endonym is written in that language and is never
// itself translated — "Tiếng Việt" reads the same whatever the active locale — so it lives here, not
// in the message catalogues.
const ENDONYMS: Record<Locale, string> = { en: "English", vi: "Tiếng Việt" };

/** The language's own name, for the locale switcher. */
export function localeName(code: Locale): string {
  return ENDONYMS[code];
}

const LOCALE_KEY = "pos.dashboard.locale";

// The starting locale: the operator's last choice (per browser), else the browser's own preference
// when it is one we ship, else the enforced `en` floor.
function loadLocale(): Locale {
  try {
    const stored = localStorage.getItem(LOCALE_KEY);
    if (stored === "en" || stored === "vi") {
      return stored;
    }
  } catch {
    // Private windows and blocked site-data throw on access; fall through to the browser hint.
  }
  try {
    if (navigator.language.toLowerCase().startsWith("vi")) {
      return "vi";
    }
  } catch {
    // No navigator (non-browser context): the en floor.
  }
  return "en";
}

const [locale, setLocaleSignal] = createSignal<Locale>(loadLocale());
export { locale };

export function setLocale(next: Locale): void {
  setLocaleSignal(next);
  document.documentElement.lang = next;
  try {
    localStorage.setItem(LOCALE_KEY, next);
  } catch {
    // Persistence is a convenience; a failure to store is not an error worth surfacing.
  }
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
