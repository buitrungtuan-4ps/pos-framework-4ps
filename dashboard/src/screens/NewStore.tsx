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
import { screenHref } from "../state/screens";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
// The four generated artifacts live in one module because a CI gate renders and parses them; see
// `installers.mjs` and `scripts/installer-syntax.mjs`. This screen supplies the values and nothing
// else, which is also what lets a Windows installer exist at all (issue #182).
import {
  DEFAULT_BIND_PORT,
  configToml as renderConfigToml,
  envFile as renderEnvFile,
  linuxInstaller,
  windowsInstaller,
} from "../installers.mjs";
import type { InstallerValues } from "../installers.d.mts";

// Scopes offered for the store's key, each mapped to a static i18n key (a template-literal key would
// not be a MessageKey and would defeat the type check).
//
// `read_config` and `relay_orders` are pre-selected together because a store box needs **both** or it
// is half-connected in a way that is hard to see: with only `read_config` it pulls its configuration
// happily while the order relay retries a `403` forever, so cloud-placed orders never reach the
// kitchen and the only symptom is a log line. That pairing is why the relay scope is offered here at
// all — it was missing from this list, which meant no operator could grant it from the console
// (roadmap-v3 E6).
const SCOPES: readonly { wire: string; key: MessageKey }[] = [
  { wire: "read_config", key: "scope.read_config" },
  { wire: "relay_orders", key: "scope.relay_orders" },
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

  const [scopes, setScopes] = createSignal<string[]>([
    "read_config",
    "relay_orders",
    "place_orders",
  ]);
  const [issued, setIssued] = createSignal<CreateApiKeyResponse | null>(null);

  // A box fact, not a store fact: the listen port never reaches the registry, it only shapes the
  // generated `config.toml`. Empty means "leave it out and take the edge's default", which is why the
  // field lives on the handoff step beside the file rather than on step 1 beside the store's name.
  const [bindPort, setBindPort] = createSignal("");

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
    // Idempotency (F0): the wizard mints the store exactly once. If it already did — the operator
    // stepped back to step 1 and forward again — advance without creating a second, orphaned store.
    if (created()) {
      setStep(2);
      return;
    }
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
    const store = created();
    if (!store) {
      return;
    }
    setError("");
    setBusy(true);
    try {
      // Bound to the store step 1 just created (S1). The store sync routes refuse a key that names
      // another store or none, so a wizard that issued a tenant-wide key here would hand the
      // operator a credential the box cannot use.
      setIssued(await api.createApiKey(tenantId(), scopes(), store.store_id));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // The store server's bootstrap file (crates/pos-edge `EdgeConfig`): which store this box is, which
  // cloud it dials, and which stream it publishes into. It still carries no credential — that comes
  // from the environment file below or from the OS keyring — but it MUST carry `cloud_url`, because
  // `serve()` only composes the cloud surface `if let Some(cloud_url)`. A file without it produces a
  // box that boots LAN-only: no config-pull, no heartbeat, no relay, and `/api/activation` answering
  // 404 so the `/setup` screen cannot even be reached.
  //
  // The `[nats]` table has the same character and was missing for the same reason (roadmap **E3**):
  // `spawn_event_publish` returns early when the section is absent, logging that the outbox is not
  // being published. Every store this wizard has ever provisioned therefore committed its sales
  // locally and shipped none of them — the cloud's rollups, reports and reconciliation all read a
  // stream nothing published to. Absent config is not a safe default here; it is silence.
  //
  // This generator used to omit it, on the belief — stated in this very comment — that
  // `deny_unknown_fields` would reject a cloud URL here. That was true when the wizard was written and
  // stopped being true when E1 added the field; nobody revisited the comment, so the omission looked
  // deliberate for four merged slices. Anything asserted about the edge's schema in a comment here is
  // a claim about another crate, and it goes stale silently.
  const [copied, setCopied] = createSignal(false);

  // Every artifact this screen hands over comes from `src/installers.mjs`, which exists so that a CI
  // gate can render and parse them — `sh -n` for the shell script, PowerShell's own parser for the
  // `.ps1` on the Windows runner. Before that gate, nothing in the tree checked a generated
  // installer at all, and the Windows one could not be written for want of a way to check it
  // (issue #182). The screen's only job is to supply the values.
  const values = (): InstallerValues | null => {
    const store = created();
    if (!store) {
      return null;
    }
    return {
      storeName: store.name,
      storeId: store.store_id,
      tenantLabel: tenantName() || tenantId(),
      tenantId: tenantId(),
      cloudUrl: window.location.origin,
      cloudHost: window.location.hostname,
      bindPort: bindPort(),
      key: issued()?.token ?? null,
    };
  };

  const configToml = () => {
    const v = values();
    return v ? renderConfigToml(v) : "";
  };

  const envFile = () => {
    const v = values();
    return v ? renderEnvFile(v) : "";
  };

  const installer = () => {
    const v = values();
    return v ? linuxInstaller(v) : "";
  };

  const windowsScript = () => {
    const v = values();
    return v ? windowsInstaller(v) : "";
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
  // pos_cloud (same origin), so the browser saves the file under the name given here.
  const download = (filename: string, body: string, mime: string) => {
    const url = URL.createObjectURL(new Blob([body], { type: mime }));
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = filename;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(url);
  };

  const downloadConfig = () => download("config.toml", configToml(), "application/toml");
  const downloadEnv = () => download("env", envFile(), "text/plain");
  const downloadInstaller = () =>
    download("install-pos-edge.sh", installer(), "application/x-shellscript");
  const downloadWindowsInstaller = () =>
    download("install-pos-edge.ps1", windowsScript(), "text/plain");

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

              {/* The two files the operator carries to the box: config.toml and the env file. */}
              <div class="flex flex-col gap-2">
                <span class="text-sm font-medium text-ink">{t("wizard.configTitle")}</span>
                <p class="text-sm text-ink-muted">{t("wizard.configHint")}</p>
                <TextField
                  label={t("wizard.bindPort")}
                  value={bindPort()}
                  onInput={setBindPort}
                  placeholder={DEFAULT_BIND_PORT}
                />
                <p class="text-sm text-ink-muted">{t("wizard.bindPortHint")}</p>
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

              {/* The environment file — the store's one secret, installed root-owned and mode 0600. */}
              <div class="flex flex-col gap-2">
                <span class="text-sm font-medium text-ink">{t("wizard.envTitle")}</span>
                <p class="text-sm text-ink-muted">{t("wizard.envHint")}</p>
                <Show when={!issued()}>
                  <Banner tone="danger" message={t("wizard.envNoKey")} />
                </Show>
                <pre class="overflow-x-auto rounded-token border border-line bg-surface-raised p-3 font-mono text-xs text-ink">
                  {envFile()}
                </pre>
                <div class="flex gap-2">
                  <Button variant="secondary" onClick={downloadEnv}>
                    {t("wizard.downloadEnv")}
                  </Button>
                </div>
              </div>

              {/* The installer (R3). Last of the three because it *contains* the two above — an
                  operator who takes only this file has everything, and the two separate downloads
                  stay for the cases the script cannot cover: a Windows box, a hand-managed host, or
                  a technician who wants to read the config before it is written. */}
              <div class="flex flex-col gap-2">
                <span class="text-sm font-medium text-ink">{t("wizard.installerTitle")}</span>
                <p class="text-sm text-ink-muted">{t("wizard.installerHint")}</p>
                <Banner tone="danger" message={t("wizard.installerSecret")} />
                <div class="flex gap-2">
                  <Button onClick={downloadInstaller}>{t("wizard.downloadInstaller")}</Button>
                </div>
              </div>

              {/* The same handoff for a Windows store (R4, issue #182). Windows used to get the two
                  files and a README, so the install was five sc.exe lines typed by hand — and the
                  one easiest to skip decides whether the box comes back from an update. */}
              <div class="flex flex-col gap-2">
                <span class="text-sm font-medium text-ink">{t("wizard.installerWindowsTitle")}</span>
                <p class="text-sm text-ink-muted">{t("wizard.installerWindowsHint")}</p>
                <Banner tone="danger" message={t("wizard.installerSecret")} />
                <div class="flex gap-2">
                  <Button onClick={downloadWindowsInstaller}>
                    {t("wizard.downloadWindowsInstaller")}
                  </Button>
                </div>
              </div>

              <Banner tone="ok" message={t("wizard.doneHint")} />
              <div>
                <Button onClick={() => navigate(screenHref("stores", tenantId(), ""), { replace: true })}>
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
