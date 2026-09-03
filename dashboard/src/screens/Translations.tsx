// The translation grid (ADR-0043; dynamic locales added in M4, ADR-0074): edit a tenant's message
// catalogue, keyed by message id with one column per locale. `en` is the enforced floor (ADR-0020) —
// the server rejects a save where any key has an empty `en` — so it is validated here before sending,
// too. The columns are no longer the app's own UI locales: they are the union of the content locales
// the platform can serve (`GET /admin/locales`, driven by the country modules), `en`, and any locale
// the stored grid already carries — so a `ja`/`ko` value is visible even before the UI ships that
// language. A per-locale completion % and a missing-only filter make the gaps easy to find.

import { createMemo, createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { TranslationGrid, TranslationImportReport } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import { EmptyState, Modal } from "../components/kit";
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

  // CSV import (ADR-0075): a dry-run report the operator confirms before anything is written. The file
  // is held so the confirm re-sends the exact same bytes the dry-run classified.
  const [importReport, setImportReport] = createSignal<TranslationImportReport | null>(null);
  const [importFile, setImportFile] = createSignal<File | null>(null);

  // The version the grid was read at, or `null` for a tenant that has authored none yet (ADR-0095).
  const [version, setVersion] = createSignal<string | null>(null);

  // A `412` means somebody else saved the grid while this one was open. The screen reloads rather
  // than offering a retry: retrying would re-apply the overwrite the refusal exists to prevent, and
  // the operator needs to see what actually changed before deciding again.
  const fail = async (caught: unknown) => {
    if (caught instanceof ApiError && caught.isStale) {
      const message = t("translations.stale");
      setError(message);
      toast.error(message);
      await load();
      return;
    }
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
      setGrid(loadedGrid.value);
      setVersion(loadedGrid.etag);
      setCatalogue(locales);
      setLoaded(true);
    } catch (caught) {
      await fail(caught);
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
      await api.putTranslations(tenantId(), grid(), version());
      // Re-read rather than trusting the save's own `ETag`: the next edit has to be made against
      // what is stored, and the reload is one request either way.
      await load();
      toast.ok(t("translations.saved"));
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const exportGrid = async () => {
    try {
      await api.exportTranslationsCsv(tenantId());
    } catch (caught) {
      await fail(caught);
    }
  };

  // Step 1: the operator picks a file; dry-run it and show the report. Nothing is written yet.
  const onImportFile = async (file: File) => {
    setError("");
    setBusy(true);
    try {
      const report = await api.dryRunTranslationsCsv(tenantId(), file);
      setImportFile(file);
      setImportReport(report);
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const closeImport = () => {
    setImportReport(null);
    setImportFile(null);
  };

  // Step 2: the operator confirms; re-send the same file to apply, then reload the grid.
  const applyImport = async () => {
    const file = importFile();
    if (!file) {
      return;
    }
    setBusy(true);
    try {
      const report = await api.applyTranslationsCsv(tenantId(), file);
      toast.ok(
        t("translations.importApplied", {
          created: report.create_count,
          updated: report.update_count,
        }),
      );
      closeImport();
      await load();
    } catch (caught) {
      await fail(caught);
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
            <div class="flex flex-wrap gap-2">
              <Button variant="secondary" disabled={busy()} onClick={() => void exportGrid()}>
                {t("translations.exportCsv")}
              </Button>
              <label class="inline-flex min-h-touch cursor-pointer items-center justify-center rounded-token border border-line bg-surface-raised px-4 text-base font-medium text-ink hover:brightness-95">
                {t("translations.importCsv")}
                <input
                  type="file"
                  accept=".csv,text/csv"
                  class="hidden"
                  disabled={busy()}
                  onChange={(event) => {
                    const file = event.currentTarget.files?.[0];
                    if (file) {
                      void onImportFile(file);
                    }
                    event.currentTarget.value = "";
                  }}
                />
              </label>
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            </div>
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

        <Modal
          open={importReport() !== null}
          title={t("translations.importReview")}
          closeLabel={t("action.close")}
          onClose={closeImport}
          footer={
            <>
              <Button variant="secondary" onClick={closeImport}>
                {t("action.cancel")}
              </Button>
              <Button
                disabled={busy() || (importReport()?.create_count ?? 0) + (importReport()?.update_count ?? 0) === 0}
                onClick={() => void applyImport()}
              >
                {t("translations.importApply")}
              </Button>
            </>
          }
        >
          <Show when={importReport()}>
            {(report) => (
              <div class="flex flex-col gap-3 text-sm text-ink">
                <p>
                  {t("translations.importSummary", {
                    created: report().create_count,
                    updated: report().update_count,
                    rejected: report().reject_count,
                  })}
                </p>
                <Show when={report().reject_count > 0}>
                  <div class="flex flex-col gap-1">
                    <p class="font-medium">{t("translations.importRejected")}</p>
                    <ul class="max-h-40 overflow-y-auto text-xs text-ink-muted">
                      <For each={report().rows.filter((row) => row.action === "reject")}>
                        {(row) => (
                          <li>
                            <span class="font-mono">{row.key || "—"}</span>
                            {row.reason ? ` — ${row.reason}` : ""}
                          </li>
                        )}
                      </For>
                    </ul>
                  </div>
                </Show>
              </div>
            )}
          </Show>
        </Modal>
      </RequireContext>
    </div>
  );
}
