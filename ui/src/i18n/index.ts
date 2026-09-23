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

// The language this device's operator chose, kept across reloads (F10). Until it was kept, every
// reload, every tablet that woke and every till that restarted came back in English, and a
// Vietnamese shop tapped the toggle on every device every time. Per device, because the person at a
// till is the one who reads it; wrapped because storage can be unavailable (a private window, a
// locked-down kiosk profile) and a language is not worth failing a boot over.
const CHOSEN = "pos.locale";

function chosen(): Locale | null {
  try {
    const value = globalThis.localStorage?.getItem(CHOSEN);
    return LOCALES.find((candidate) => candidate === value) ?? null;
  } catch {
    return null;
  }
}

const [locale, setLocaleSignal] = createSignal<Locale>(chosen() ?? "en");
export { locale };

function apply(next: Locale): void {
  setLocaleSignal(next);
  if (typeof document !== "undefined") {
    document.documentElement.lang = next;
  }
}
apply(locale());

// The operator picked a language: show it, and remember it on this device.
export function setLocale(next: Locale): void {
  apply(next);
  try {
    globalThis.localStorage?.setItem(CHOSEN, next);
  } catch {
    // Shown for this session; the next reload starts from the store's language again.
  }
}

// The store's own display language, as the default for a device whose operator has chosen none.
// `tag` is the BCP 47 tag the store publishes (`vi`, `vi-VN`); a language the till has no catalogue
// for leaves the current one. Never overrides a choice the operator made on this device.
export function adoptStoreLanguage(tag: string | undefined): void {
  if (tag === undefined || chosen() !== null) {
    return;
  }
  const primary = tag.toLowerCase().split("-")[0];
  const match = LOCALES.find((candidate) => candidate === primary);
  if (match !== undefined) {
    apply(match);
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
  const message = CATALOGUES[active][key] ?? en[key];
  // Fast path: a static string with no interpolation is returned as-is, skipping the
  // IntlMessageFormat parse and format. Two characters disqualify it, and both were measured
  // against the real library rather than assumed: `{` opens an argument, and `''` is ICU's escape
  // for a literal apostrophe — `It''s` formats to `It's`, so returning it raw would be wrong.
  // A lone apostrophe (`don't`, `a 'b' c`) is literal in ICU and stays on the fast path.
  if (!args && typeof message === "string" && !message.includes("{") && !message.includes("''")) {
    return message;
  }
  const formatted = formatter(active, key).format(args);
  return typeof formatted === "string" ? formatted : String(formatted);
}
