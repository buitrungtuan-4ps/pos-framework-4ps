// Store settings — locale (ADR-0074, Track M4): the store's currency, IANA timezone, and
// business-date cutoff, published as the `locale` config node the edge applies (ADR-0014). Currency
// and timezone offer the platform's known values as suggestions (from the country registry and a
// short list of common zones) but accept any value the server validates, so a fork's market works
// before its country module ships. A live preview shows the store's business date under the chosen
// timezone and cutoff, so the operator sees the effect before publishing. Store-scoped: publishing is
// per-store.
//
// It also carries the till's money (ADR-0105): whether prices are quoted with tax already in them,
// what the total rounds to in cash, and which notes the pay pad offers. Those are country facts, so
// **Start from a country** fills all of them in from the compiled country pack — the affordance the
// whole country-pack idea exists for. Each stays editable afterwards, because a store in an airport
// may round differently from the country it is in.

import { createMemo, createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Country } from "../api/types";
import { LOCALES, localeName, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import { FormField } from "../components/kit";
import { toast } from "../components/Toast";

/** A short list of common IANA zones offered as suggestions; any valid zone is accepted server-side. */
const COMMON_TIMEZONES = [
  "Asia/Ho_Chi_Minh",
  "Asia/Bangkok",
  "Asia/Tokyo",
  "Asia/Singapore",
  "Asia/Seoul",
  "Asia/Jakarta",
  "UTC",
];

/**
 * The store's business date under `timezone` and `cutoffHour`, as `YYYY-MM-DD`, or `null` if the
 * timezone is not one the browser knows. Before the cutoff hour the trading day is still the previous
 * calendar day (ADR-0014), so this mirrors the edge's `derive_business_date` for the preview.
 */
function businessDatePreview(timezone: string, cutoffHour: number): string | null {
  try {
    const now = new Date();
    const parts = new Intl.DateTimeFormat("en-CA", {
      timeZone: timezone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      hour12: false,
    }).formatToParts(now);
    const field = (type: string) => parts.find((part) => part.type === type)?.value ?? "";
    const year = Number(field("year"));
    const month = Number(field("month"));
    const day = Number(field("day"));
    // Intl renders midnight as "24" in some engines; normalise to 0 so the comparison holds.
    const hour = Number(field("hour")) % 24;
    if (!Number.isFinite(year) || !Number.isFinite(month) || !Number.isFinite(day)) {
      return null;
    }
    const date = new Date(Date.UTC(year, month - 1, day));
    if (hour < cutoffHour) {
      date.setUTCDate(date.getUTCDate() - 1);
    }
    return date.toISOString().slice(0, 10);
  } catch {
    return null;
  }
}

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

/** A textarea's lines as printed lines, trimmed, with the blanks dropped. */
function printedLines(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

export function StoreSettings() {
  const [countries, setCountries] = createSignal<Country[]>([]);
  const [currency, setCurrency] = createSignal("VND");
  const [timezone, setTimezone] = createSignal("Asia/Ho_Chi_Minh");
  const [cutoffHour, setCutoffHour] = createSignal(4);
  // The store's display language, which selects a compiled item's per-locale name at the edge
  // (ADR-0074). Blank means each item shows its default name.
  const [displayLanguage, setDisplayLanguage] = createSignal("");
  // The till's money (ADR-0105). `roundingText` and `notesText` are the raw fields rather than parsed
  // numbers, so a half-typed value is not silently reinterpreted while the operator is typing; both
  // are parsed once, on publish.
  const [pricesIncludeTax, setPricesIncludeTax] = createSignal(false);
  // Who this store legally is (ADR-0106). Held as raw text, published as a whole; the multi-line
  // fields are one printed line per input line, which is how an address is actually written.
  const [country, setCountry] = createSignal("");
  const [legalName, setLegalName] = createSignal("");
  const [tradingName, setTradingName] = createSignal("");
  const [addressText, setAddressText] = createSignal("");
  const [registrationNumber, setRegistrationNumber] = createSignal("");
  const [registrationLabel, setRegistrationLabel] = createSignal("");
  const [contactText, setContactText] = createSignal("");
  const [footerText, setFooterText] = createSignal("");
  const [roundingText, setRoundingText] = createSignal("");
  const [notesText, setNotesText] = createSignal("");
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setCountries(await api.listCountries());
      setLoaded(true);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // The country registry is not store-scoped, but loading it on the store gate keeps one load path.
  onScopedContext("store", () => void load());

  // The currencies the platform's country modules declare, deduped, as datalist suggestions.
  const currencyOptions = createMemo(() => {
    const set = new Set(countries().map((country) => country.currency_code));
    return [...set].sort();
  });

  const preview = createMemo(() => businessDatePreview(timezone(), cutoffHour()));

  /** The rounding increment as the server wants it: a positive number, or `null` for no rounding. */
  const roundingValue = createMemo(() => {
    const text = roundingText().trim();
    if (text === "") {
      return null;
    }
    const value = Number(text);
    return Number.isFinite(value) && value > 0 ? value : null;
  });

  /** The notes as the server wants them: positive minor units, ascending, de-duplicated. */
  const noteValues = createMemo(() => {
    const parsed = notesText()
      .split(",")
      .map((part) => Number(part.trim()))
      .filter((value) => Number.isFinite(value) && value > 0);
    return [...new Set(parsed)].sort((left, right) => left - right);
  });

  /** Whether what was typed into either field is not what will be published. */
  const tillMoneyRejected = createMemo(
    () =>
      (roundingText().trim() !== "" && roundingValue() === null) ||
      notesText().split(",").filter((part) => part.trim() !== "").length !== noteValues().length,
  );

  // Fill the form from a compiled country pack (ADR-0105). This is the affordance country packs
  // exist for: the operator picks Japan and every field a Japanese store needs is already right,
  // rather than being told to remember that Japanese prices include their tax.
  const applyCountry = (code: string) => {
    const country = countries().find((candidate) => candidate.code === code);
    if (!country) {
      return;
    }
    setCurrency(country.currency_code);
    setDisplayLanguage(country.default_language);
    setPricesIncludeTax(country.prices_include_tax);
    setRoundingText(
      country.cash_rounding_increment === null ? "" : String(country.cash_rounding_increment),
    );
    setNotesText(country.cash_denominations.join(", "));
    setCountry(country.code);
    // The label the paper uses is the country's, and the operator can overwrite it — a closed set
    // here would make the fourth country a code change (ADR-0106).
    if (registrationLabel().trim() === "") {
      setRegistrationLabel(REGISTRATION_LABEL[country.code] ?? "");
    }
    toast.ok(t("storeSettings.filledFrom", { country: country.display_name }));
  };

  const publishProfile = async () => {
    setError("");
    setBusy(true);
    try {
      await api.publishStoreProfile(tenantId(), storeId(), {
        legal_name: legalName().trim(),
        trading_name: tradingName().trim() || undefined,
        address_lines: printedLines(addressText()),
        tax_registration_number: registrationNumber().trim() || undefined,
        tax_registration_label: registrationLabel().trim() || undefined,
        contact_lines: printedLines(contactText()),
        footer_lines: printedLines(footerText()),
        country_code: country().trim() || undefined,
      });
      toast.ok(t("storeSettings.profilePublished", { store: storeName() }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const publish = async () => {
    setError("");
    setBusy(true);
    try {
      await api.publishLocale(tenantId(), storeId(), {
        currency_code: currency().trim().toUpperCase(),
        timezone: timezone().trim(),
        cutoff_hour: cutoffHour(),
        display_language: displayLanguage().trim() || undefined,
        prices_include_tax: pricesIncludeTax(),
        cash_rounding_increment: roundingValue(),
        cash_denominations: noteValues(),
      });
      toast.ok(t("storeSettings.published", { store: storeName() }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("storeSettings.title")} description={t("storeSettings.description")} />
      <RequireContext need="store">
        <Card title={t("storeSettings.locale")}>
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={loaded()}>
            <div class="grid max-w-xl gap-4">
              <FormField label={t("storeSettings.fromCountry")}>
                <select
                  class="min-h-touch w-72 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
                  value=""
                  onChange={(event) => {
                    applyCountry(event.currentTarget.value);
                    event.currentTarget.value = "";
                  }}
                >
                  <option value="">{t("storeSettings.fromCountryNone")}</option>
                  <For each={countries()}>
                    {(country) => <option value={country.code}>{country.display_name}</option>}
                  </For>
                </select>
                <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.fromCountryHint")}</p>
              </FormField>

              <FormField label={t("storeSettings.currency")}>
                <input
                  class="min-h-touch w-40 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink uppercase"
                  list="currency-options"
                  maxLength={3}
                  value={currency()}
                  onInput={(event) => setCurrency(event.currentTarget.value)}
                />
                <datalist id="currency-options">
                  <For each={currencyOptions()}>{(code) => <option value={code} />}</For>
                </datalist>
              </FormField>

              <FormField label={t("storeSettings.timezone")}>
                <input
                  class="min-h-touch w-72 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
                  list="timezone-options"
                  value={timezone()}
                  onInput={(event) => setTimezone(event.currentTarget.value)}
                />
                <datalist id="timezone-options">
                  <For each={COMMON_TIMEZONES}>{(zone) => <option value={zone} />}</For>
                </datalist>
              </FormField>

              <FormField label={t("storeSettings.cutoff")}>
                <input
                  type="number"
                  min={0}
                  max={23}
                  class="min-h-touch w-24 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
                  value={cutoffHour()}
                  onInput={(event) =>
                    setCutoffHour(Math.max(0, Math.min(23, Number(event.currentTarget.value) || 0)))
                  }
                />
                <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.cutoffHint")}</p>
              </FormField>

              <FormField label={t("storeSettings.language")}>
                <input
                  class="min-h-touch w-40 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
                  list="language-options"
                  value={displayLanguage()}
                  onInput={(event) => setDisplayLanguage(event.currentTarget.value)}
                />
                <datalist id="language-options">
                  <For each={LOCALES}>
                    {(code) => <option value={code}>{localeName(code)}</option>}
                  </For>
                </datalist>
                <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.languageHint")}</p>
              </FormField>

              <FormField label={t("storeSettings.pricesIncludeTax")}>
                <label class="flex min-h-touch items-center gap-2 text-sm text-ink">
                  <input
                    type="checkbox"
                    class="size-4"
                    checked={pricesIncludeTax()}
                    onChange={(event) => setPricesIncludeTax(event.currentTarget.checked)}
                  />
                  {t("storeSettings.pricesIncludeTaxLabel")}
                </label>
                <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.pricesIncludeTaxHint")}</p>
              </FormField>

              <FormField label={t("storeSettings.cashRounding")}>
                <input
                  inputMode="numeric"
                  class="min-h-touch w-40 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink tabular-nums"
                  value={roundingText()}
                  onInput={(event) => setRoundingText(event.currentTarget.value)}
                />
                <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.cashRoundingHint")}</p>
              </FormField>

              <FormField label={t("storeSettings.cashNotes")}>
                <input
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-sm text-ink tabular-nums"
                  value={notesText()}
                  onInput={(event) => setNotesText(event.currentTarget.value)}
                />
                <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.cashNotesHint")}</p>
              </FormField>

              <Show when={tillMoneyRejected()}>
                <Banner tone="danger" message={t("storeSettings.tillMoneyRejected")} />
              </Show>

              <div class="rounded-token border border-line bg-surface-raised p-3 text-sm">
                <span class="text-ink-muted">{t("storeSettings.businessDate")}</span>{" "}
                <Show when={preview()} fallback={<span class="text-danger">{t("storeSettings.badTimezone")}</span>}>
                  {(date) => <span class="font-medium text-ink">{date()}</span>}
                </Show>
              </div>

              <div>
                <Button disabled={busy()} onClick={() => void publish()}>
                  {t("storeSettings.publish")}
                </Button>
              </div>
            </div>
          </Show>
        </Card>
        <Card title={t("storeSettings.identity")}>
          <p class="mb-4 max-w-2xl text-sm text-ink-muted">
            {t("storeSettings.identityHint")}
          </p>
          <div class="grid max-w-xl gap-4">
            <FormField label={t("storeSettings.legalName")}>
              <input
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
                value={legalName()}
                onInput={(event) => setLegalName(event.currentTarget.value)}
              />
            </FormField>

            <FormField label={t("storeSettings.tradingName")}>
              <input
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
                value={tradingName()}
                onInput={(event) => setTradingName(event.currentTarget.value)}
              />
              <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.tradingNameHint")}</p>
            </FormField>

            <FormField label={t("storeSettings.address")}>
              <textarea
                rows={3}
                class="w-full rounded-token border border-line bg-surface-raised px-3 py-2 text-sm text-ink"
                value={addressText()}
                onInput={(event) => setAddressText(event.currentTarget.value)}
              />
              <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.linesHint")}</p>
            </FormField>

            <FormField label={t("storeSettings.registrationLabel")}>
              <input
                class="min-h-touch w-40 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
                value={registrationLabel()}
                onInput={(event) => setRegistrationLabel(event.currentTarget.value)}
              />
            </FormField>

            <FormField label={t("storeSettings.registrationNumber")}>
              <input
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
                value={registrationNumber()}
                onInput={(event) => setRegistrationNumber(event.currentTarget.value)}
              />
              <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.registrationHint")}</p>
            </FormField>

            <FormField label={t("storeSettings.contact")}>
              <textarea
                rows={2}
                class="w-full rounded-token border border-line bg-surface-raised px-3 py-2 text-sm text-ink"
                value={contactText()}
                onInput={(event) => setContactText(event.currentTarget.value)}
              />
            </FormField>

            <FormField label={t("storeSettings.footer")}>
              <textarea
                rows={2}
                class="w-full rounded-token border border-line bg-surface-raised px-3 py-2 text-sm text-ink"
                value={footerText()}
                onInput={(event) => setFooterText(event.currentTarget.value)}
              />
            </FormField>

            <div>
              <Button disabled={busy()} onClick={() => void publishProfile()}>
                {t("storeSettings.publishProfile")}
              </Button>
            </div>
          </div>
        </Card>
      </RequireContext>
    </div>
  );
}
