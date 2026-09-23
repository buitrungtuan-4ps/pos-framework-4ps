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

import { createEffect, createMemo, createSignal, Show } from "solid-js";

import { api } from "../api/client";
import type { Json } from "../api/types";
import { LOCALES, localeName, t } from "../i18n";
import { formatInstant } from "../lib/format";
import { RequireContext } from "../lib/scoped";
import { createAdminResource, failureOf } from "../lib/resource";
import { usePublishedNodes } from "../lib/published";
import { storeId, storeName, tenantId } from "../state/session";
import {
  Banner,
  Card,
  CheckboxField,
  NumberField,
  PageHeader,
  SelectField,
  TextArea,
  TextField,
} from "../components/ui";
import { FormSection, PublishBar, type PublishState, StickyActions } from "../components/kit";
import { describePublish } from "../lib/publish-copy";
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

/** One node of the store's effective config tree, or `null` when the tree does not carry it. */
function node(effective: Json | null, key: string): Record<string, Json> | null {
  if (effective === null || typeof effective !== "object" || Array.isArray(effective)) {
    return null;
  }
  const value = (effective as Record<string, Json>)[key];
  if (value === undefined || value === null || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  return value as Record<string, Json>;
}

/** A string field of a published node, or `null` — a node written by a fork may hold anything. */
function readString(from: Record<string, Json> | null, key: string): string | null {
  const value = from?.[key];
  return typeof value === "string" ? value : null;
}

/** A finite-number field of a published node, or `null`. */
function readNumber(from: Record<string, Json> | null, key: string): number | null {
  const value = from?.[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** A boolean field of a published node, or `null`. */
function readBoolean(from: Record<string, Json> | null, key: string): boolean | null {
  const value = from?.[key];
  return typeof value === "boolean" ? value : null;
}

/** A string-array field of a published node as textarea lines; missing or malformed reads as "". */
function readLines(from: Record<string, Json> | null, key: string): string {
  const value = from?.[key];
  if (!Array.isArray(value)) {
    return "";
  }
  return value.filter((entry): entry is string => typeof entry === "string").join("\n");
}

/** A textarea's lines as printed lines, trimmed, with the blanks dropped. */
function printedLines(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

export function StoreSettings() {
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
  // A write's refusal. The read's own refusal lives in the resource below; the two are kept apart
  // because a form that could not be read and a publish that was rejected want different words.
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  /**
   * The three reads this screen opens on: the country registry for the pickers, the effective tree
   * for the values, and the per-node publish dates for the two bars.
   *
   * One resource rather than three, because the form is primed from the tree and labelled from the
   * registry — a half-arrived read would show a country code where a country name belongs.
   */
  const read = createAdminResource(
    async (tenant, store) => {
      if (!store) {
        return null;
      }
      const [countryList, effective] = await Promise.all([
        api.listCountries(),
        api.effectiveConfig(tenant, store),
      ]);
      return { countries: countryList, effective };
    },
    { scope: "tenant" },
  );

  /**
   * When each of this screen's two nodes was last published.
   *
   * Replaces a read of the *tree* version this screen used to make. The tree moves whenever any
   * node is published, so publishing tax rates made this screen claim the store's locale had just
   * been published — a true date attached to the wrong fact.
   */
  const published = usePublishedNodes();

  const fail = (caught: unknown) => {
    const message = apiMessage(caught);
    setError(message);
    toast.error(message);
  };

  /**
   * Back to the framework's own defaults.
   *
   * Called before every hydration, and it is the half that makes switching stores safe: without it
   * a field the *previous* store published and this one does not would survive the switch, and this
   * screen publishes whatever the fields hold.
   */
  const resetToDefaults = () => {
    setCurrency("VND");
    setTimezone("Asia/Ho_Chi_Minh");
    setCutoffHour(4);
    setDisplayLanguage("");
    setPricesIncludeTax(false);
    setRoundingText("");
    setNotesText("");
    setCountry("");
    setFilledFrom("");
    setLegalName("");
    setTradingName("");
    setAddressText("");
    setRegistrationNumber("");
    setRegistrationLabel("");
    setContactText("");
    setFooterText("");
  };

  /**
   * Fill the form from what the store is running (V15).
   *
   * Until this existed the screen opened on framework defaults for every store — VND,
   * Asia/Ho_Chi_Minh, cutoff 04:00 — whatever that store had published. Two failures, and the
   * second is the serious one: the operator was told the wrong thing, and then a Save of any single
   * field republished the whole node from those defaults, so opening this screen to fix a typo in
   * the address could move a Tokyo store's currency to VND. A publish here rebuilds the node
   * wholesale (`admin_publish_locale`), so the form has to start from the node, not from nothing.
   *
   * Each field is read defensively: a node written by a fork, or by an older build, is a store this
   * console still has to draw. A value of the wrong type leaves the framework default in place
   * rather than throwing.
   */
  const hydrate = (effective: Json | null) => {
    const locale = node(effective, "locale");
    const profile = node(effective, "store_profile");

    const currencyCode = readString(locale, "currency_code");
    if (currencyCode !== null) {
      setCurrency(currencyCode.toUpperCase());
    }
    const zone = readString(locale, "timezone");
    if (zone !== null) {
      setTimezone(zone);
    }
    const cutoff = readNumber(locale, "cutoff_hour");
    if (cutoff !== null) {
      setCutoffHour(Math.max(0, Math.min(23, Math.trunc(cutoff))));
    }
    const language = readString(locale, "display_language");
    if (language !== null) {
      setDisplayLanguage(language);
    }
    const inclusive = readBoolean(locale, "prices_include_tax");
    if (inclusive !== null) {
      setPricesIncludeTax(inclusive);
    }
    // `cash_rounding_increment` is nullable *by design* — `null` means "do not round" — so an
    // explicit null is a published value and leaves the field blank, which is what blank means here.
    if (locale !== null && "cash_rounding_increment" in locale) {
      const increment = readNumber(locale, "cash_rounding_increment");
      setRoundingText(increment === null ? "" : String(increment));
    }
    const denominations = locale?.["cash_denominations"];
    if (Array.isArray(denominations)) {
      setNotesText(
        denominations
          .filter((value): value is number => typeof value === "number" && Number.isFinite(value))
          .join(", "),
      );
    }
    // The country is on both nodes and they can disagree — `store_profile` carries the country the
    // paper claims, `locale` the one the store trades under (ADR-0114). The picker edits the locale
    // one, so that is the one it shows; the profile's is the fallback for a store that published a
    // profile before `country_code` was required on the locale publish.
    const countryCode = readString(locale, "country_code") ?? readString(profile, "country_code");
    if (countryCode !== null) {
      setCountry(countryCode.toUpperCase());
    }

    setLegalName(readString(profile, "legal_name") ?? "");
    setTradingName(readString(profile, "trading_name") ?? "");
    setAddressText(readLines(profile, "address_lines"));
    setRegistrationNumber(readString(profile, "tax_registration_number") ?? "");
    setRegistrationLabel(readString(profile, "tax_registration_label") ?? "");
    setContactText(readLines(profile, "contact_lines"));
    setFooterText(readLines(profile, "footer_lines"));
  };

  /** The country registry, or an empty list while the read is in flight or after it failed. */
  const countries = () => read.value()?.countries ?? [];

  /**
   * Prime the form from what the store is running, every time the read lands.
   *
   * The reset is what makes switching stores safe, and it stays: without it a field the *previous*
   * store published and this one does not would survive the switch, and this screen publishes
   * whatever the fields hold.
   */
  createEffect(() => {
    const value = read.value();
    if (value === null) {
      return;
    }
    resetToDefaults();
    hydrate(value.effective);
  });

  /**
   * How the locale bar describes where its node stands — this screen's words, not the shared ones.
   *
   * "Never published" is the important one, and it is why the shared copy will not do: a locale
   * this store has never published means the form is showing VND, Asia/Ho_Chi_Minh and a 04:00
   * cutoff because those are the *framework's* defaults, not because the store chose them. An
   * operator who reads a plain "not published yet" and then edits one field would republish all
   * five from values nobody picked. The identity bar below has no such trap and uses the shared
   * wording.
   */
  const describeLocale = (state: PublishState, at: number | null): string => {
    if (state === "never") {
      return t("storeSettings.nothingPublished");
    }
    return at === null
      ? t("storeSettings.runningNow")
      : t("storeSettings.runningNowAt", {
          when: formatInstant(at),
        });
  };

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
      // Re-read the node dates, not the whole form: the fields hold exactly what was just
      // published, and a failed read here should not blank a form the operator is still using.
      await published.refresh();
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
      await published.refresh();
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
          {/* Two banners, because they answer different questions: the read could not be made, or
              the publish was refused. A screen that shows one message for both leaves an operator
              guessing whether to press the button again. */}
          <Show when={failureOf(read)}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={read.value() !== null}>
            <div class="grid max-w-xl gap-4">
              <FormSection title={t("storeSettings.sectionWhere")}>
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

              </FormSection>

              <FormSection title={t("storeSettings.sectionClock")}>
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

              </FormSection>

              <FormSection title={t("storeSettings.sectionMoney")}>
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
              </FormSection>

              <div class="rounded-token border border-line bg-surface-raised p-3 text-sm">
                <span class="text-ink-muted">{t("storeSettings.businessDate")}</span>{" "}
                <Show when={preview()} fallback={<span class="text-danger">{t("storeSettings.badTimezone")}</span>}>
                  {(date) => <span class="font-medium text-ink">{date()}</span>}
                </Show>
              </div>

              {/* Whose values these are, and whether the store is running them. A form that
                  silently shows defaults for a store running something else is worse than an
                  empty one, because it reads as an answer. */}
              <StickyActions>
              <PublishBar
                label={t("storeSettings.locale")}
                publishedAtMs={published.publishedAtMs("locale")}
                describe={describeLocale}
                publishLabel={t("storeSettings.publish")}
                busy={busy()}
                disabled={country().trim() === ""}
                disabledReason={t("storeSettings.countryRequired")}
                onPublish={() => void publish()}
              />
              </StickyActions>
            </div>
          </Show>
        </Card>
        <Card title={t("storeSettings.identity")}>
          <p class="mb-4 max-w-2xl text-sm text-ink-muted">
            {t("storeSettings.identityHint")}
          </p>
          <div class="grid max-w-xl gap-4">
            <FormSection title={t("storeSettings.sectionEntity")}>
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

            </FormSection>

            <FormSection title={t("storeSettings.sectionReceipt")}>
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

            </FormSection>

            <StickyActions>
            <PublishBar
              label={t("storeSettings.identity")}
              publishedAtMs={published.publishedAtMs("store_profile")}
              describe={describePublish}
              publishLabel={t("storeSettings.publishProfile")}
              busy={busy()}
              onPublish={() => void publishProfile()}
            />
            </StickyActions>
          </div>
        </Card>
      </RequireContext>
    </div>
  );
}
