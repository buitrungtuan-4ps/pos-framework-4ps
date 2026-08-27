// The config-tree editor (ADR-0033, ADR-0060) — the missing half of config delivery: publish one
// level of a store's tree, and the store pulls it (ADR-0039). Left: the current effective (composed,
// validated) config for the tenant/store in context. Right: author one level as JSON and publish;
// the server composes + validates and either appends a new version or rejects with the violations,
// keeping the last good version current.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import { CONFIG_LEVELS, type ConfigLevel, type ConfigVersion, type Json } from "../api/types";
import { locale, type MessageKey, t } from "../i18n";

// The level names are user-visible, so each maps to a static i18n key (a template-literal key would
// not be a MessageKey and would defeat the type check).
const LEVEL_KEY: Record<ConfigLevel, MessageKey> = {
  tenant: "config.level.tenant",
  brand: "config.level.brand",
  store: "config.level.store",
  device: "config.level.device",
};
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextArea } from "../components/ui";
import { ConfirmDialog, EmptyState, StatusBadge } from "../components/kit";
import { toast } from "../components/Toast";

export function Config() {
  const [effective, setEffective] = createSignal<Json | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  const [level, setLevel] = createSignal<ConfigLevel>("store");
  const [document, setDocument] = createSignal("{\n}\n");
  const [error, setError] = createSignal("");
  const [ok, setOk] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [versions, setVersions] = createSignal<ConfigVersion[]>([]);
  const [viewing, setViewing] = createSignal<string | null>(null);
  const [viewingDoc, setViewingDoc] = createSignal<Json | null>(null);
  const [compare, setCompare] = createSignal(false);
  const [rollbackTo, setRollbackTo] = createSignal<string | null>(null);

  const loadVersions = async () => {
    try {
      setVersions(await api.configVersions(tenantId(), storeId()));
    } catch {
      // A store with no published config has no versions yet; leave the list empty rather than erroring.
      setVersions([]);
    }
  };

  const load = async () => {
    setError("");
    setOk("");
    setBusy(true);
    setViewing(null);
    setViewingDoc(null);
    try {
      setEffective(await api.effectiveConfig(tenantId(), storeId()));
      setLoaded(true);
      await loadVersions();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  const viewVersion = async (versionId: string) => {
    setError("");
    try {
      setViewingDoc(await api.configVersionEffective(tenantId(), storeId(), versionId));
      setViewing(versionId);
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    }
  };

  const rollback = async () => {
    const versionId = rollbackTo();
    setRollbackTo(null);
    if (versionId === null) {
      return;
    }
    setBusy(true);
    try {
      const result = await api.rollbackConfig(tenantId(), storeId(), versionId);
      toast.ok(t("config.rolledBack", { version: result.config_version_id }));
      await load();
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const versionTime = (version: ConfigVersion) => new Date(version.at_ms).toLocaleString(locale());

  // A crude but honest line diff for stable-key config JSON: pretty-print both and mark each line of
  // `doc` that differs from `other` at the same index. Enough to show what a version changed.
  const diffLines = (doc: Json | null, other: Json | null) => {
    const otherLines = JSON.stringify(other, null, 2).split("\n");
    return JSON.stringify(doc, null, 2)
      .split("\n")
      .map((line, index) => ({ line, changed: line !== (otherLines[index] ?? "") }));
  };

  // Load on open and whenever the tenant/store changes — never with an empty context (F0).
  onScopedContext("store", () => void load());

  const publish = async () => {
    setError("");
    setOk("");
    let parsed: Json;
    try {
      parsed = JSON.parse(document()) as Json;
    } catch {
      setError(t("config.invalidJson"));
      return;
    }
    setBusy(true);
    try {
      const result = await api.publishConfig(tenantId(), storeId(), level(), parsed);
      setOk(t("config.published", { version: result.config_version_id }));
      await load();
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("config.title")} description={t("config.description")} />
      <RequireContext need="store">
        <div class="grid gap-6 lg:grid-cols-2">
          <Card
            title={t("config.effective")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show
              when={loaded()}
              fallback={<p class="text-sm text-ink-muted">{t("config.loadHint")}</p>}
            >
              <Show
                when={effective() !== null}
                fallback={<p class="text-sm text-ink-muted">{t("config.effectiveEmpty")}</p>}
              >
                <pre class="max-h-96 overflow-auto rounded-token border border-line bg-surface-raised p-3 text-xs text-ink">
                  {JSON.stringify(effective(), null, 2)}
                </pre>
              </Show>
            </Show>
          </Card>

          <Card title={t("config.publish")}>
            <div class="flex flex-col gap-4">
              <label class="block">
                <span class="mb-1 block text-sm font-medium text-ink">{t("config.level")}</span>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  value={level()}
                  onChange={(event) => setLevel(event.currentTarget.value as ConfigLevel)}
                >
                  <For each={CONFIG_LEVELS}>
                    {(name) => <option value={name}>{t(LEVEL_KEY[name])}</option>}
                  </For>
                </select>
              </label>
              <TextArea
                label={t("config.document")}
                value={document()}
                onInput={setDocument}
                rows={14}
              />
              <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
              <Show when={ok()}>{(message) => <Banner tone="ok" message={message()} />}</Show>
              <Button disabled={busy()} onClick={() => void publish()}>
                {t("action.publish")}
              </Button>
            </div>
          </Card>
        </div>

        <div class="mt-6">
          <Card title={t("config.history")}>
            <Show
              when={versions().length > 0}
              fallback={<EmptyState title={t("config.noVersions")} description={t("config.noVersionsHint")} />}
            >
              <ul class="flex flex-col gap-2">
                <For each={versions()}>
                  {(version) => (
                    <li class="flex flex-wrap items-center justify-between gap-2 rounded-token border border-line px-3 py-2">
                      <div class="flex flex-wrap items-center gap-2">
                        <span class="text-sm text-ink">{versionTime(version)}</span>
                        <Show when={version.current}>
                          <StatusBadge tone="active" label={t("config.current")} />
                        </Show>
                      </div>
                      <div class="flex flex-wrap items-center gap-2">
                        <Button
                          variant="secondary"
                          onClick={() => {
                            setCompare(false);
                            void viewVersion(version.version_id);
                          }}
                        >
                          {t("config.view")}
                        </Button>
                        <Show when={!version.current}>
                          <Button
                            variant="secondary"
                            disabled={busy()}
                            onClick={() => setRollbackTo(version.version_id)}
                          >
                            {t("config.rollback")}
                          </Button>
                        </Show>
                      </div>
                    </li>
                  )}
                </For>
              </ul>

              <Show when={viewing()}>
                {(versionId) => (
                  <div class="mt-4">
                    <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
                      <span class="text-sm font-medium text-ink">
                        {t("config.viewingVersion", { version: versionId() })}
                      </span>
                      <label class="flex items-center gap-2 text-sm text-ink-muted">
                        <input
                          type="checkbox"
                          checked={compare()}
                          onChange={(event) => setCompare(event.currentTarget.checked)}
                        />
                        {t("config.compareCurrent")}
                      </label>
                    </div>
                    <Show
                      when={compare()}
                      fallback={
                        <pre class="max-h-96 overflow-auto rounded-token border border-line bg-surface-raised p-3 text-xs text-ink">
                          {JSON.stringify(viewingDoc(), null, 2)}
                        </pre>
                      }
                    >
                      <div class="grid gap-3 lg:grid-cols-2">
                        <div>
                          <span class="mb-1 block text-xs font-medium text-ink-muted">
                            {t("config.thisVersion")}
                          </span>
                          <pre class="max-h-96 overflow-auto rounded-token border border-line bg-surface-raised p-3 text-xs text-ink">
                            <For each={diffLines(viewingDoc(), effective())}>
                              {(row) => (
                                <div class={row.changed ? "bg-danger/15 text-ink" : "text-ink"}>
                                  {row.line || " "}
                                </div>
                              )}
                            </For>
                          </pre>
                        </div>
                        <div>
                          <span class="mb-1 block text-xs font-medium text-ink-muted">
                            {t("config.currentVersion")}
                          </span>
                          <pre class="max-h-96 overflow-auto rounded-token border border-line bg-surface-raised p-3 text-xs text-ink">
                            <For each={diffLines(effective(), viewingDoc())}>
                              {(row) => (
                                <div class={row.changed ? "bg-accent/15 text-ink" : "text-ink"}>
                                  {row.line || " "}
                                </div>
                              )}
                            </For>
                          </pre>
                        </div>
                      </div>
                    </Show>
                  </div>
                )}
              </Show>
            </Show>
          </Card>
        </div>
      </RequireContext>

      <ConfirmDialog
        open={rollbackTo() !== null}
        title={t("config.rollbackConfirmTitle")}
        message={t("config.rollbackConfirmBody")}
        confirmLabel={t("config.rollback")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        busy={busy()}
        onConfirm={() => void rollback()}
        onCancel={() => setRollbackTo(null)}
      />
    </div>
  );
}
