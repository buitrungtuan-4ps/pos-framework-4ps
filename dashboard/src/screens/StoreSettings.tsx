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

import { createMemo, createSignal, Show } from "solid-js";

import { api } from "../api/client";
import type { Country } from "../api/types";
import { LOCALES, localeName, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  NumberField,
  PageHeader,
  SelectField,
  TextArea,
  TextField,
} from "../components/ui";
import { toast } from "../components/Toast";
import { apiMessage } from "../lib/errors";

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
  // Which country's defaults were last applied by the filler below. Display only — the publish
  // sends `country`, which is a separate fact (ADR-0114).
  const [filledFrom, setFilledFrom] = createSignal("");
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
    const message = apiMessage(caught);
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
        country_code: country().trim().toUpperCase(),
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
              {/* Which country this store is in (ADR-0114). Required: the publish refuses without
                  it, because it is the value a hosted store's region is compared against — and a
                  store whose country nobody recorded reads as "country not recorded" on the fleet
                  console rather than silently as agreeing. Distinct from the filler below, which
                  overwrites six other fields; this one records a fact and touches nothing else, so
                  an airport store can keep rounding its own way. */}
              <SelectField
                label={t("storeSettings.country")}
                value={country()}
                options={countries().map((option) => ({
                  value: option.code,
                  label: option.display_name,
                }))}
                onChange={setCountry}
                placeholder={t("storeSettings.countryNone")}
                hint={t("storeSettings.countryHint")}
              />

              {/* This picker keeps what was last applied rather than snapping back to blank. The
                  hand-rolled version reset itself in the change handler — `currentTarget.value = ""`
                  — which a controlled component cannot do: the bound value never changed, so no
                  re-render would put it back. Showing the applied country is also the more useful
                  answer to "where did these six values come from". */}
              <SelectField
                label={t("storeSettings.fromCountry")}
                value={filledFrom()}
                options={countries().map((option) => ({
                  value: option.code,
                  label: option.display_name,
                }))}
                onChange={(code) => {
                  setFilledFrom(code);
                  applyCountry(code);
                }}
                placeholder={t("storeSettings.fromCountryNone")}
                hint={t("storeSettings.fromCountryHint")}
              />

              {/* Upper-cased on the way in, not by CSS. The hand-rolled field had `class="uppercase"`,
                  which changes what an operator sees and not what gets sent: a typed `vnd` looked
                  right and published lower-case. */}
              <TextField
                label={t("storeSettings.currency")}
                value={currency()}
                onInput={(value) => setCurrency(value.toUpperCase())}
                suggestions={currencyOptions().map((code) => ({ value: code }))}
              />

              <TextField
                label={t("storeSettings.timezone")}
                value={timezone()}
                onInput={setTimezone}
                suggestions={COMMON_TIMEZONES.map((zone) => ({ value: zone }))}
              />

              <NumberField
                label={t("storeSettings.cutoff")}
                value={cutoffHour()}
                // Clamped here as well as advertised on the control: `min`/`max` bound the spinner
                // and not what can be typed, and an hour of 47 is a business date nobody can read.
                onChange={(hour) => setCutoffHour(Math.max(0, Math.min(23, hour ?? 0)))}
                min={0}
                max={23}
                hint={t("storeSettings.cutoffHint")}
              />

              <TextField
                label={t("storeSettings.language")}
                value={displayLanguage()}
                onInput={setDisplayLanguage}
                suggestions={LOCALES.map((code) => ({ value: code, label: localeName(code) }))}
                hint={t("storeSettings.languageHint")}
              />

              <CheckboxField
                label={t("storeSettings.pricesIncludeTaxLabel")}
                checked={pricesIncludeTax()}
                onChange={setPricesIncludeTax}
                hint={t("storeSettings.pricesIncludeTaxHint")}
              />

              <TextField
                label={t("storeSettings.cashRounding")}
                value={roundingText()}
                onInput={setRoundingText}
                hint={t("storeSettings.cashRoundingHint")}
              />

              <TextField
                label={t("storeSettings.cashNotes")}
                value={notesText()}
                onInput={setNotesText}
                hint={t("storeSettings.cashNotesHint")}
              />

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
                <Button disabled={busy() || country().trim() === ""} onClick={() => void publish()}>
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
            <TextField
              label={t("storeSettings.legalName")}
              value={legalName()}
              onInput={setLegalName}
            />

            <TextField
              label={t("storeSettings.tradingName")}
              value={tradingName()}
              onInput={setTradingName}
              hint={t("storeSettings.tradingNameHint")}
            />

            <div>
              <TextArea
                label={t("storeSettings.address")}
                value={addressText()}
                onInput={setAddressText}
                rows={3}
              />
              <p class="mt-1 text-xs text-ink-muted">{t("storeSettings.linesHint")}</p>
            </div>

            <TextField
              label={t("storeSettings.registrationLabel")}
              value={registrationLabel()}
              onInput={setRegistrationLabel}
            />

            <TextField
              label={t("storeSettings.registrationNumber")}
              value={registrationNumber()}
              onInput={setRegistrationNumber}
              hint={t("storeSettings.registrationHint")}
            />

            <TextArea
              label={t("storeSettings.contact")}
              value={contactText()}
              onInput={setContactText}
              rows={2}
            />

            <TextArea
              label={t("storeSettings.footer")}
              value={footerText()}
              onInput={setFooterText}
              rows={2}
            />

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
