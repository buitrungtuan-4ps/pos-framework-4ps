// The config-tree editor (ADR-0033, ADR-0060) — the missing half of config delivery: publish one
// level of a store's tree, and the store pulls it (ADR-0039). Left: the current effective (composed,
// validated) config for the tenant/store in context. Right: author one level as JSON and publish;
// the server composes + validates and either appends a new version or rejects with the violations,
// keeping the last good version current.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import { CONFIG_LEVELS, type ConfigLevel, type Json } from "../api/types";
import { type MessageKey, t } from "../i18n";

// The level names are user-visible, so each maps to a static i18n key (a template-literal key would
// not be a MessageKey and would defeat the type check).
const LEVEL_KEY: Record<ConfigLevel, MessageKey> = {
  tenant: "config.level.tenant",
  brand: "config.level.brand",
  store: "config.level.store",
  device: "config.level.device",
};
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextArea } from "../components/ui";

export function Config() {
  const [effective, setEffective] = createSignal<Json | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  const [level, setLevel] = createSignal<ConfigLevel>("store");
  const [document, setDocument] = createSignal("{\n}\n");
  const [error, setError] = createSignal("");
  const [ok, setOk] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const load = async () => {
    setError("");
    setOk("");
    setBusy(true);
    try {
      setEffective(await api.effectiveConfig(tenantId(), storeId()));
      setLoaded(true);
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

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
      <Show
        when={tenantId() && storeId()}
        fallback={<Banner tone="danger" message={t("context.required")} />}
      >
        <div class="grid gap-6 lg:grid-cols-2">
          <Card
            title={t("config.effective")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.load")}
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
      </Show>
    </div>
  );
}
