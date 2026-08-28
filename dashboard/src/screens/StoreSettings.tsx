// Store settings — locale (ADR-0074, Track M4): the store's currency, IANA timezone, and
// business-date cutoff, published as the `locale` config node the edge applies (ADR-0014). Currency
// and timezone offer the platform's known values as suggestions (from the country registry and a
// short list of common zones) but accept any value the server validates, so a fork's market works
// before its country module ships. A live preview shows the store's business date under the chosen
// timezone and cutoff, so the operator sees the effect before publishing. Store-scoped: publishing is
// per-store.

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

export function StoreSettings() {
  const [countries, setCountries] = createSignal<Country[]>([]);
  const [currency, setCurrency] = createSignal("VND");
  const [timezone, setTimezone] = createSignal("Asia/Ho_Chi_Minh");
  const [cutoffHour, setCutoffHour] = createSignal(4);
  // The store's display language, which selects a compiled item's per-locale name at the edge
  // (ADR-0074). Blank means each item shows its default name.
  const [displayLanguage, setDisplayLanguage] = createSignal("");
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

  const publish = async () => {
    setError("");
    setBusy(true);
    try {
      await api.publishLocale(tenantId(), storeId(), {
        currency_code: currency().trim().toUpperCase(),
        timezone: timezone().trim(),
        cutoff_hour: cutoffHour(),
        display_language: displayLanguage().trim() || undefined,
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
      </RequireContext>
    </div>
  );
}
