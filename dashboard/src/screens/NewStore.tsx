// The guided new-store wizard (ADR-0065, WS-C). Onboarding from zero without a ULID or a curl: name
// the store (optionally under a brand) → create it in the registry → issue the scoped API key its
// devices use to reach the cloud (shown once) → a handoff summary pointing at the next steps
// (activation, configuration). Composes the registry + API-key routes; tenant comes from the picker.

import { createSignal, For, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { api, ApiError } from "../api/client";
import type { Brand, CreateApiKeyResponse, Store } from "../api/types";
import { type MessageKey, t } from "../i18n";
import { tenantId, tenantName } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";

// Scopes offered for the store's key, each mapped to a static i18n key (a template-literal key would
// not be a MessageKey and would defeat the type check). Two are pre-selected: a store device reads its
// configuration and places the orders its channels take.
const SCOPES: readonly { wire: string; key: MessageKey }[] = [
  { wire: "read_config", key: "scope.read_config" },
  { wire: "place_orders", key: "scope.place_orders" },
  { wire: "read_rollups", key: "scope.read_rollups" },
  { wire: "manage_devices", key: "scope.manage_devices" },
];

const STEP_KEYS: readonly MessageKey[] = ["wizard.step1", "wizard.step2", "wizard.step3"];

export function NewStore() {
  const navigate = useNavigate();

  const [step, setStep] = createSignal(1);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [brands, setBrands] = createSignal<Brand[]>([]);
  const [name, setName] = createSignal("");
  const [brandId, setBrandId] = createSignal("");
  const [created, setCreated] = createSignal<Store | null>(null);

  const [scopes, setScopes] = createSignal<string[]>(["read_config", "place_orders"]);
  const [issued, setIssued] = createSignal<CreateApiKeyResponse | null>(null);

  const fail = (caught: unknown) =>
    setError(caught instanceof ApiError ? caught.message : String(caught));

  const loadBrands = async () => {
    try {
      setBrands(await api.listBrands(tenantId()));
    } catch (caught) {
      fail(caught);
    }
  };

  // Load the tenant's brands once, so step 1's brand picker is populated.
  if (tenantId()) {
    void loadBrands();
  }

  const toggleScope = (wire: string) => {
    setScopes((current) =>
      current.includes(wire) ? current.filter((s) => s !== wire) : [...current, wire],
    );
  };

  const createStore = async () => {
    const storeName = name().trim();
    if (!storeName) {
      setError(t("stores.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      const store = await api.createStore(tenantId(), storeName, brandId() || undefined);
      setCreated(store);
      setStep(2);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const issueKey = async () => {
    if (scopes().length === 0) {
      setError(t("wizard.scopesRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      setIssued(await api.createApiKey(tenantId(), scopes()));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // The store server's bootstrap file (crates/pos-edge EdgeConfig): it names WHICH store this box is
  // and nothing else — the schema is `deny_unknown_fields`, so a credential or a cloud URL here would
  // be rejected at load. The box gets its credential by activation, not this file (see the runbook).
  // Store/tenant names and the cloud origin ride along as comments, so the file documents itself
  // without adding parsed keys. Assembled client-side because the store_id is only known here.
  const [copied, setCopied] = createSignal(false);

  const configToml = () => {
    const store = created();
    if (!store) {
      return "";
    }
    return [
      "# pos_edge bootstrap configuration",
      `# Store:  ${store.name}  (${store.store_id})`,
      `# Tenant: ${tenantName() || tenantId()}  (${tenantId()})`,
      `# Cloud:  ${window.location.origin}`,
      "#",
      "# This file tells the store server WHICH store it is. It carries no credential — the box",
      "# gets its credential by activation (see the provisioning runbook), never from this file.",
      "# Save it as config.toml beside the pos_edge binary, or point POS_EDGE_CONFIG at its path.",
      "",
      `store_id = "${store.store_id}"`,
      "",
      "# Optional — the LAN IP to advertise in the pairing QR; pin it with a DHCP reservation:",
      '# advertised_ip = "192.168.1.50"',
      "",
      "# Optional — override the listen address (default 0.0.0.0:8787):",
      '# bind = "0.0.0.0:8787"',
      "",
      "# Optional — where the SQLite event store lives (default store.sqlite):",
      '# store_path = "store.sqlite"',
      "",
    ].join("\n");
  };

  const copyConfig = async () => {
    try {
      await navigator.clipboard.writeText(configToml());
      setCopied(true);
    } catch {
      // Clipboard access can be blocked; the operator can still select the text or download the file.
      setCopied(false);
    }
  };

  // A real file download, not a data: link the operator has to rename — this dashboard is served by
  // pos_cloud (same origin), so the browser saves `config.toml` directly.
  const downloadConfig = () => {
    const url = URL.createObjectURL(new Blob([configToml()], { type: "application/toml" }));
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = "config.toml";
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(url);
  };

  return (
    <div>
      <PageHeader title={t("wizard.title")} description={t("wizard.description")} />
      <Show
        when={tenantId()}
        fallback={<Banner tone="danger" message={t("context.tenantRequired")} />}
      >
        <ol class="mb-6 flex flex-wrap gap-2" aria-label={t("wizard.title")}>
          <For each={STEP_KEYS}>
            {(key, index) => (
              <li
                class={`rounded-token border px-3 py-1 text-sm ${
                  step() === index() + 1
                    ? "border-accent bg-accent text-accent-ink"
                    : step() > index() + 1
                      ? "border-line bg-surface-raised text-ink-muted"
                      : "border-line text-ink-muted"
                }`}
                aria-current={step() === index() + 1 ? "step" : undefined}
              >
                {index() + 1}. {t(key)}
              </li>
            )}
          </For>
        </ol>

        <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

        {/* Step 1 — store details */}
        <Show when={step() === 1}>
          <Card title={t("wizard.step1")}>
            <div class="flex flex-col gap-4">
              <p class="text-sm text-ink-muted">{t("wizard.step1Hint")}</p>
              <TextField
                label={t("stores.name")}
                value={name()}
                onInput={setName}
                placeholder={t("stores.namePlaceholder")}
              />
              <label class="block">
                <span class="mb-1 block text-sm font-medium text-ink">{t("stores.brand")}</span>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  value={brandId()}
                  onChange={(event) => setBrandId(event.currentTarget.value)}
                >
                  <option value="">{t("stores.noBrand")}</option>
                  <For each={brands()}>
                    {(brand) => <option value={brand.brand_id}>{brand.name}</option>}
                  </For>
                </select>
              </label>
              <div>
                <Button disabled={busy()} onClick={() => void createStore()}>
                  {t("wizard.next")}
                </Button>
              </div>
            </div>
          </Card>
        </Show>

        {/* Step 2 — API key */}
        <Show when={step() === 2}>
          <Card title={t("wizard.step2")}>
            <div class="flex flex-col gap-4">
              <Banner tone="ok" message={t("wizard.created")} />
              <p class="text-sm text-ink-muted">{t("wizard.step2Hint")}</p>
              <Show
                when={issued()}
                fallback={
                  <>
                    <fieldset class="flex flex-col gap-2">
                      <legend class="mb-1 text-sm font-medium text-ink">{t("wizard.scopes")}</legend>
                      <For each={SCOPES}>
                        {(scope) => (
                          <label class="flex items-center gap-2 text-sm text-ink">
                            <input
                              type="checkbox"
                              checked={scopes().includes(scope.wire)}
                              onChange={() => toggleScope(scope.wire)}
                            />
                            {t(scope.key)}
                          </label>
                        )}
                      </For>
                    </fieldset>
                    <div class="flex gap-2">
                      <Button variant="secondary" onClick={() => setStep(1)}>
                        {t("wizard.back")}
                      </Button>
                      <Button disabled={busy()} onClick={() => void issueKey()}>
                        {t("wizard.issueKey")}
                      </Button>
                      <Button variant="secondary" onClick={() => setStep(3)}>
                        {t("wizard.skipKey")}
                      </Button>
                    </div>
                  </>
                }
              >
                {(key) => (
                  <>
                    <Banner tone="ok" message={t("wizard.keyIssued")} />
                    <div class="break-all rounded-token border border-line bg-surface-raised p-3 font-mono text-sm text-ink">
                      {key().token}
                    </div>
                    <div>
                      <Button onClick={() => setStep(3)}>{t("wizard.next")}</Button>
                    </div>
                  </>
                )}
              </Show>
            </div>
          </Card>
        </Show>

        {/* Step 3 — handoff */}
        <Show when={step() === 3}>
          <Card title={t("wizard.doneTitle")}>
            <div class="flex flex-col gap-4">
              <table class="w-full text-left text-sm">
                <tbody>
                  <tr class="border-b border-line">
                    <th class="py-2 pr-4 font-medium text-ink-muted">{t("wizard.storeLabel")}</th>
                    <td class="py-2">
                      <div class="flex flex-col">
                        <span class="text-ink">{created()?.name}</span>
                        <span class="font-mono text-xs text-ink-muted">{created()?.store_id}</span>
                      </div>
                    </td>
                  </tr>
                  <Show when={issued()}>
                    {(key) => (
                      <tr class="border-b border-line">
                        <th class="py-2 pr-4 font-medium text-ink-muted">{t("wizard.keyLabel")}</th>
                        <td class="py-2 font-mono text-xs text-ink-muted">{key().id}</td>
                      </tr>
                    )}
                  </Show>
                </tbody>
              </table>

              {/* The store server's config.toml — the operator's one file to carry to the box. */}
              <div class="flex flex-col gap-2">
                <span class="text-sm font-medium text-ink">{t("wizard.configTitle")}</span>
                <p class="text-sm text-ink-muted">{t("wizard.configHint")}</p>
                <pre class="overflow-x-auto rounded-token border border-line bg-surface-raised p-3 font-mono text-xs text-ink">
                  {configToml()}
                </pre>
                <div class="flex gap-2">
                  <Button variant="secondary" onClick={() => void copyConfig()}>
                    {t("action.copy")}
                  </Button>
                  <Button variant="secondary" onClick={downloadConfig}>
                    {t("wizard.downloadConfig")}
                  </Button>
                </div>
                <Show when={copied()}>
                  <Banner tone="ok" message={t("wizard.configCopied")} />
                </Show>
              </div>

              <Banner tone="ok" message={t("wizard.doneHint")} />
              <div>
                <Button onClick={() => navigate("/stores", { replace: true })}>
                  {t("wizard.finish")}
                </Button>
              </div>
            </div>
          </Card>
        </Show>
      </Show>
    </div>
  );
}
