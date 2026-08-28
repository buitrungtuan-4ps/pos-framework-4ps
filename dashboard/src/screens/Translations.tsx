// The translation grid (ADR-0043; dynamic locales added in M4, ADR-0074): edit a tenant's message
// catalogue, keyed by message id with one column per locale. `en` is the enforced floor (ADR-0020) —
// the server rejects a save where any key has an empty `en` — so it is validated here before sending,
// too. The columns are no longer the app's own UI locales: they are the union of the content locales
// the platform can serve (`GET /admin/locales`, driven by the country modules), `en`, and any locale
// the stored grid already carries — so a `ja`/`ko` value is visible even before the UI ships that
// language. A per-locale completion % and a missing-only filter make the gaps easy to find.

import { createMemo, createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { TranslationGrid } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import { EmptyState } from "../components/kit";
import { toast } from "../components/Toast";

/** The enforced fallback locale (ADR-0020) — always a column, even if the catalogue omits it. */
const FALLBACK_LOCALE = "en";

export function Translations() {
  const [grid, setGrid] = createSignal<TranslationGrid>({});
  const [catalogue, setCatalogue] = createSignal<string[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [newKey, setNewKey] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [missingOnly, setMissingOnly] = createSignal(false);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedGrid, locales] = await Promise.all([
        api.getTranslations(tenantId()),
        api.listLocales(),
      ]);
      setGrid(loadedGrid);
      setCatalogue(locales);
      setLoaded(true);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  // The column set: the platform's content locales, the enforced fallback, and any locale the stored
  // grid already carries — so a value authored in a locale the catalogue does not (yet) list stays
  // visible rather than being silently hidden.
  const locales = createMemo(() => {
    const set = new Set<string>([FALLBACK_LOCALE, ...catalogue()]);
    for (const row of Object.values(grid())) {
      for (const code of Object.keys(row)) {
        set.add(code);
      }
    }
    return [...set].sort();
  });

  const keys = () => Object.keys(grid()).sort();

  const cell = (key: string, locale: string): string => grid()[key]?.[locale] ?? "";

  // Whether a key is missing any displayed locale — the missing-only filter's predicate.
  const incomplete = (key: string): boolean =>
    locales().some((locale) => cell(key, locale).trim() === "");

  const shownKeys = () => (missingOnly() ? keys().filter(incomplete) : keys());

  // Per-locale completion: non-empty cells over the number of keys, as a whole percent.
  const completion = (locale: string): number => {
    const all = keys();
    if (all.length === 0) {
      return 100;
    }
    const filled = all.filter((key) => cell(key, locale).trim() !== "").length;
    return Math.round((filled / all.length) * 100);
  };

  const setCell = (key: string, locale: string, value: string) => {
    const current = grid();
    setGrid({ ...current, [key]: { ...current[key], [locale]: value } });
  };

  const addKey = () => {
    const key = newKey().trim();
    if (key === "" || grid()[key] !== undefined) {
      return;
    }
    setGrid({ ...grid(), [key]: {} });
    setNewKey("");
  };

  const save = async () => {
    setError("");
    const missing = Object.entries(grid()).filter(([, row]) => !(row.en ?? "").trim());
    if (missing.length > 0) {
      setError(t("translations.missingEn"));
      return;
    }
    setBusy(true);
    try {
      await api.putTranslations(tenantId(), grid());
      toast.ok(t("translations.saved"));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("translations.title")} description={t("translations.description")} />
      <RequireContext need="tenant">
        <Card
          title={t("translations.grid")}
          actions={
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          }
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={loaded()}>
            <label class="mb-3 flex items-center gap-2 text-sm text-ink">
              <input
                type="checkbox"
                checked={missingOnly()}
                onChange={(event) => setMissingOnly(event.currentTarget.checked)}
              />
              {t("translations.missingOnly")}
            </label>
            <div class="mb-4 overflow-x-auto">
              <Show when={keys().length > 0} fallback={<EmptyState title={t("translations.empty")} />}>
                <table class="w-full text-left text-sm">
                  <thead>
                    <tr class="border-b border-line text-ink-muted">
                      <th class="py-2 pr-4 font-medium">{t("translations.key")}</th>
                      <For each={locales()}>
                        {(code) => (
                          <th class="py-2 pr-4 font-medium">
                            {`${code.toUpperCase()} · ${completion(code)}%`}
                          </th>
                        )}
                      </For>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={shownKeys()}>
                      {(key) => (
                        <tr class="border-b border-line">
                          <td class="py-2 pr-4 align-top font-mono text-xs text-ink">{key}</td>
                          <For each={locales()}>
                            {(code) => (
                              <td class="py-2 pr-4">
                                <input
                                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                  aria-label={`${key} ${code}`}
                                  value={cell(key, code)}
                                  onInput={(event) => setCell(key, code, event.currentTarget.value)}
                                />
                              </td>
                            )}
                          </For>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </Show>
            </div>
            <div class="flex flex-wrap items-end gap-3">
              <div class="w-64">
                <TextField
                  label={t("translations.addKey")}
                  placeholder={t("translations.newKeyPlaceholder")}
                  value={newKey()}
                  onInput={setNewKey}
                />
              </div>
              <Button variant="secondary" onClick={addKey}>
                {t("action.add")}
              </Button>
              <Button disabled={busy()} onClick={() => void save()}>
                {t("action.save")}
              </Button>
            </div>
          </Show>
        </Card>
      </RequireContext>
    </div>
  );
}
