// Where a value written at a scope came from, as Shared settings says it under the value: the
// country pack that gave it (ADR-0183), or when a person set it there.

import type { WrittenSetting } from "../api/schemas";
import { locale, t } from "../i18n";
import { formatInstant } from "./format";

/** A country's name in the console's language, from its two-letter code; the code where none. */
export function countryName(code: string): string {
  try {
    return new Intl.DisplayNames([locale()], { type: "region" }).of(code) ?? code;
  } catch {
    return code;
  }
}

/**
 * "From the Vietnam pack." for a value a country pack gave, "Set here on …" for one a person
 * wrote, and nothing for none or an unreadable time.
 */
export function writtenNote(written: WrittenSetting | undefined): string | undefined {
  const pack = written?.pack_country_code;
  if (pack) {
    return t("settings.fromPack", { country: countryName(pack) });
  }
  const at = Date.parse(written?.update_time ?? "");
  return Number.isFinite(at) ? t("settings.setOn", { when: formatInstant(at) }) : undefined;
}
