// Choosing a store's country fills in what its country pack knows (ADR-0105, pilot U2/M3).
//
// Store settings had two selects: **Country**, which records a fact and touched nothing else, and
// **Start from a country**, which overwrote six fields from the pack. The pilot chose Vietnam in
// the first and never saw the second, so a Vietnamese store was published with no 1,000 ₫ cash
// rounding and no quick-cash notes, although the Vietnam pack carries both.
//
// So choosing the country now fills the pack's values too, but only into fields nobody has set,
// because an airport store that rounds its own way must not lose that to a select. A field counts
// as set when it holds something and that something is not merely the framework's default on a
// store that has never published its locale. Whatever is filled and whatever is kept is named, so
// nothing changes silently. "Start from a country" stays the deliberate way to take all of it.
//
// A pack also gives a new store some *settings* in its own units (ADR-0183): Vietnam's 20,000 ₫
// over/short past which a drawer close asks a reason. Those are written once the locale naming a
// new country is published, by the same rule — a setting no scope sets is filled at the store's
// own scope, one somebody set is kept, and the console says which.

import { api } from "../api/client";
import type { Country, EffectiveSetting } from "../api/schemas";
import type { Json } from "../api/types";
import { toast } from "../components/Toast";
import { type MessageKey, t, tFromServer } from "../i18n";
import { apiMessage } from "./errors";

/** The store-settings fields a country pack has a value for, as the form holds them. */
export interface PackFields {
  readonly currency: string;
  readonly displayLanguage: string;
  readonly pricesIncludeTax: boolean;
  /** The cash rounding increment as typed, in minor units; blank means "do not round". */
  readonly rounding: string;
  /** The quick-cash notes as typed, comma-separated minor units. */
  readonly notes: string;
  readonly registrationLabel: string;
}

export type PackField = keyof PackFields;

/** What the form shows for a store with no locale published: the framework's own defaults. */
export const FRAMEWORK_DEFAULTS: PackFields = {
  currency: "VND",
  displayLanguage: "",
  pricesIncludeTax: false,
  rounding: "",
  notes: "",
  registrationLabel: "",
};

/** Each field's label on the form, so a message can name what changed in the reader's words. */
export const PACK_FIELD_LABEL: Readonly<Record<PackField, MessageKey>> = {
  currency: "storeSettings.currency",
  displayLanguage: "storeSettings.language",
  pricesIncludeTax: "storeSettings.pricesIncludeTax",
  rounding: "storeSettings.cashRounding",
  notes: "storeSettings.cashNotes",
  registrationLabel: "storeSettings.registrationLabel",
};

/**
 * What each country's paper calls the seller's tax registration — a *suggestion*, overwritable.
 *
 * The values live in the country packs' own documentation; the console cannot read them from the
 * registry because a label is text somebody prints, not a fact the `CountryModule` trait carries. A
 * code with no row here leaves the field blank rather than guessing at a legal caption.
 */
const REGISTRATION_LABEL: Record<string, string> = {
  VN: "MST",
  JP: "登録番号",
  IN: "GSTIN",
};

/** A country pack's values in the form's shape. `registrationLabel` is the paper's, per country. */
export function packFields(country: Country, registrationLabel: string): PackFields {
  return {
    currency: country.currency_code,
    displayLanguage: country.default_language,
    pricesIncludeTax: country.prices_include_tax,
    rounding: country.cash_rounding_increment === null ? "" : String(country.cash_rounding_increment),
    notes: country.cash_denominations.join(", "),
    registrationLabel,
  };
}

/** What choosing a country does to the form: the new values, and which fields it filled or kept. */
export interface PackFill {
  readonly next: PackFields;
  /** Fields that took the pack's value, and now differ from before. */
  readonly filled: readonly PackField[];
  /** Fields someone had set to something other than the pack's value, left as they were. */
  readonly kept: readonly PackField[];
}

/**
 * Fills `pack` into the fields of `current` nobody has set.
 *
 * `localePublished` is whether the store has a published locale. Without one, a field still at
 * the framework default was not chosen by anybody, so the pack replaces it; with one, only a blank
 * field is open, because a published value is the store's answer even when it equals the default.
 */
export function fillFromPack(
  current: PackFields,
  pack: PackFields,
  localePublished: boolean,
): PackFill {
  const next: Record<PackField, string | boolean> = { ...current };
  const filled: PackField[] = [];
  const kept: PackField[] = [];
  for (const field of Object.keys(PACK_FIELD_LABEL) as PackField[]) {
    const value = current[field];
    if (value === pack[field]) {
      continue;
    }
    const blank = typeof value === "string" && value.trim() === "";
    if (blank || (!localePublished && value === FRAMEWORK_DEFAULTS[field])) {
      next[field] = pack[field];
      filled.push(field);
    } else {
      kept.push(field);
    }
  }
  return { next: next as unknown as PackFields, filled, kept };
}

/**
 * What to tell the operator about a fill: the fields it filled, then the ones it kept. Nothing when
 * the pack changed nothing and found nothing to disagree with.
 */
export function packFillMessages(country: string, fill: PackFill): string[] {
  const names = (fields: readonly PackField[]) =>
    fields.map((field) => t(PACK_FIELD_LABEL[field])).join(", ");
  const messages: string[] = [];
  if (fill.filled.length > 0) {
    messages.push(t("storeSettings.countryFilled", { country, fields: names(fill.filled) }));
  }
  if (fill.kept.length > 0) {
    messages.push(t("storeSettings.countryKept", { country, fields: names(fill.kept) }));
  }
  return messages;
}

/** A setting a country pack gives a new store (ADR-0183), as `GET /admin/countries` lists it. */
export type SettingDefault = Country["setting_defaults"][number];

/** Which of a pack's settings to write, and which the store already has set to something else. */
export interface SettingsFill {
  readonly filled: readonly SettingDefault[];
  readonly kept: readonly SettingDefault[];
}

/**
 * A pack's settings against what the store runs. A setting no scope sets — the store runs the
 * register's default — is filled; one the tenant, a brand, a group or the store sets to another
 * value is kept, because a tenant-wide 50,000 is somebody's answer just as a typed field is.
 */
export function fillSettings(
  defaults: readonly SettingDefault[],
  effective: readonly EffectiveSetting[],
): SettingsFill {
  const filled: SettingDefault[] = [];
  const kept: SettingDefault[] = [];
  for (const seeded of defaults) {
    const running = effective.find((setting) => setting.setting_key === seeded.setting_key);
    if (running?.scope === undefined) {
      filled.push(seeded);
    } else if (running.value !== seeded.value) {
      kept.push(seeded);
    }
  }
  return { filled, kept };
}

/** A setting's title as Shared settings shows it, or its key when this console has none. */
const settingName = (seeded: SettingDefault) =>
  tFromServer(`settings.item.${seeded.setting_key}.title`, seeded.setting_key);

/**
 * Writes `country`'s settings into `store` by {@link fillSettings}, each at the store's own scope
 * through the same call Shared settings makes, and says what it filled and kept.
 */
export async function applyPackSettings(tenant: string, store: string, country: Country) {
  const fill = fillSettings(country.setting_defaults, await api.effectiveSettings(tenant, store));
  for (const seeded of fill.filled) {
    await api.putSetting(tenant, seeded.setting_key, "SETTING_SCOPE_STORE", store, seeded.value);
  }
  const names = (seeds: readonly SettingDefault[], withValue: boolean) =>
    seeds
      .map((seeded) => (withValue ? `${settingName(seeded)} = ${String(seeded.value)}` : settingName(seeded)))
      .join(", ");
  const name = country.display_name;
  if (fill.filled.length > 0) {
    toast.ok(t("storeSettings.countrySettingsFilled", { country: name, settings: names(fill.filled, true) }));
  }
  if (fill.kept.length > 0) {
    toast.ok(t("storeSettings.countrySettingsKept", { country: name, settings: names(fill.kept, false) }));
  }
}

/** One form field's signal pair. */
type FieldSignal<T> = readonly [() => T, (value: T) => void];

/** The form's signals for every field a pack fills. */
export type PackSignals = { readonly [K in PackField]: FieldSignal<PackFields[K]> };

/**
 * The two ways a country pack reaches the store-settings form.
 *
 * `choose` is the Country select: it records the country and fills the pack into the fields nobody
 * has set ([`fillFromPack`]), saying what it filled and kept. `applyAll` is "Start from a country":
 * the whole pack over what the form holds, except a registration label someone typed.
 * `published` follows a locale publish: when it names a country other than the one the store
 * had, the pack's settings are written ([`applyPackSettings`]) — so a store the new-store wizard
 * created before it had a country gets them with its first locale.
 */
export function useCountryPack(options: {
  readonly fields: PackSignals;
  readonly countries: () => readonly Country[];
  /** The store's published locale node as the screen read it, or `null` when it has none. */
  readonly publishedLocale: () => Readonly<Record<string, Json>> | null;
  readonly setCountry: (code: string) => void;
}) {
  const { fields } = options;
  const read = (): PackFields => ({
    currency: fields.currency[0](),
    displayLanguage: fields.displayLanguage[0](),
    pricesIncludeTax: fields.pricesIncludeTax[0](),
    rounding: fields.rounding[0](),
    notes: fields.notes[0](),
    registrationLabel: fields.registrationLabel[0](),
  });
  const write = (values: PackFields) => {
    fields.currency[1](values.currency);
    fields.displayLanguage[1](values.displayLanguage);
    fields.pricesIncludeTax[1](values.pricesIncludeTax);
    fields.rounding[1](values.rounding);
    fields.notes[1](values.notes);
    fields.registrationLabel[1](values.registrationLabel);
  };
  // The label the paper uses is the country's, and the operator can overwrite it — a closed set
  // here would make the fourth country a code change (ADR-0106).
  const packOf = (code: string) => {
    const country = options.countries().find((candidate) => candidate.code === code);
    return country && { country, pack: packFields(country, REGISTRATION_LABEL[country.code] ?? "") };
  };

  const choose = (code: string) => {
    options.setCountry(code);
    const found = packOf(code);
    if (found) {
      const fill = fillFromPack(read(), found.pack, options.publishedLocale() !== null);
      write(fill.next);
      packFillMessages(found.country.display_name, fill).forEach((message) => toast.ok(message));
    }
  };

  const applyAll = (code: string) => {
    const found = packOf(code);
    if (!found) {
      return;
    }
    const label = read().registrationLabel.trim();
    write({ ...found.pack, registrationLabel: label || found.pack.registrationLabel });
    options.setCountry(found.country.code);
    toast.ok(t("storeSettings.filledFrom", { country: found.country.display_name }));
  };

  // What this screen last published, per store: the read it opened on goes stale after a publish.
  let sent: { readonly store: string; readonly code: string } | null = null;
  const published = async (tenant: string, store: string, country: string) => {
    const code = country.trim().toUpperCase();
    const read = options.publishedLocale()?.["country_code"];
    const before = sent?.store === store ? sent.code : typeof read === "string" ? read : null;
    sent = { store, code };
    const found = code === before ? undefined : packOf(code);
    if (!found || found.country.setting_defaults.length === 0) {
      return;
    }
    try {
      await applyPackSettings(tenant, store, found.country);
    } catch (caught) {
      toast.error(
        t("storeSettings.countrySettingsFailed", {
          country: found.country.display_name,
          reason: apiMessage(caught),
        }),
      );
    }
  };

  return { choose, applyAll, published };
}
