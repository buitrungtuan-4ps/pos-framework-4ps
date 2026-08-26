// The translation grid (ADR-0043): edit a tenant's message catalogue, keyed by message id with one
// column per locale. `en` is the enforced floor (ADR-0020) — the server rejects a save where any
// key has an empty `en` — so it is validated here before sending, too. Add a key, edit any cell,
// then save the whole grid.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { TranslationGrid } from "../api/types";
import { LOCALES, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";

export function Translations() {
  const [grid, setGrid] = createSignal<TranslationGrid>({});
  const [loaded, setLoaded] = createSignal(false);
  const [newKey, setNewKey] = createSignal("");
  const [error, setError] = createSignal("");
  const [ok, setOk] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const load = async () => {
    setError("");
    setOk("");
    setBusy(true);
    try {
      setGrid(await api.getTranslations(tenantId()));
      setLoaded(true);
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

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
    setOk("");
    const missing = Object.entries(grid()).filter(([, row]) => !(row.en ?? "").trim());
    if (missing.length > 0) {
      setError(t("translations.missingEn"));
      return;
    }
    setBusy(true);
    try {
      await api.putTranslations(tenantId(), grid());
      setOk(t("translations.saved"));
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  const keys = () => Object.keys(grid()).sort();

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
          <Show when={ok()}>{(message) => <Banner tone="ok" message={message()} />}</Show>
          <Show when={loaded()}>
            <div class="mb-4 overflow-x-auto">
              <Show
                when={keys().length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("translations.empty")}</p>}
              >
                <table class="w-full text-left text-sm">
                  <thead>
                    <tr class="border-b border-line text-ink-muted">
                      <th class="py-2 pr-4 font-medium">{t("translations.key")}</th>
                      <For each={LOCALES}>
                        {(code) => <th class="py-2 pr-4 font-medium">{code.toUpperCase()}</th>}
                      </For>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={keys()}>
                      {(key) => (
                        <tr class="border-b border-line">
                          <td class="py-2 pr-4 align-top font-mono text-xs text-ink">{key}</td>
                          <For each={LOCALES}>
                            {(code) => (
                              <td class="py-2 pr-4">
                                <input
                                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                  aria-label={key}
                                  value={grid()[key]?.[code] ?? ""}
                                  onInput={(event) =>
                                    setCell(key, code, event.currentTarget.value)
                                  }
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
