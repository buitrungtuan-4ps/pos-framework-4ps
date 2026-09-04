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

/// The bind port the edge defaults to when `config.toml` names none (`pos_edge`'s `DEFAULT_BIND`).
const DEFAULT_BIND_PORT = "8787";

/// The JetStream stream and subject every store publishes its committed events into
/// (ADR-0087 Amendment 1). **Fleet-wide, and identical on every box** — `pos_cloud` binds one
/// durable consumer to one named stream, so a per-store stream would be ingested one store deep, and
/// a per-store subject inside a shared stream would be captured for the first box to connect and
/// refused for every one after it (the edge's handshake is a create-or-get, which does not add a
/// subject to a stream that already exists). They must match `cloud.toml`'s `[nats] stream` and
/// `filter_subject` on the cloud box, which `bootstrap.sh` documents with these same two values.
const FLEET_STREAM = "POS_FLEET";
const FLEET_SUBJECT = "pos.fleet.events";

/// The client port the broker publishes under `TLS_MODE`'s certificate-bearing postures
/// ([ADR-0089](../../../docs/adr/0089-edge-event-bus-transport.md)).
const NATS_CLIENT_PORT = "4222";

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

  const configToml = () => {
    const store = created();
    if (!store) {
      return "";
    }
    const port = bindPort().trim();
    return [
      "# pos_edge bootstrap configuration",
      `# Store:  ${store.name}  (${store.store_id})`,
      `# Tenant: ${tenantName() || tenantId()}  (${tenantId()})`,
      "#",
      "# This file tells the store server WHICH store it is and WHICH cloud to dial. It carries no",
      "# credential — that lives in the environment file (or the OS keyring), never here.",
      "# Save it as config.toml beside the pos_edge binary, or point POS_EDGE_CONFIG at its path.",
      "",
      `store_id = "${store.store_id}"`,
      `cloud_url = "${window.location.origin}"`,
      ...(port && port !== DEFAULT_BIND_PORT
        ? ["", `bind = "0.0.0.0:${port}"`]
        : ["", `# Optional — override the listen address (default 0.0.0.0:${DEFAULT_BIND_PORT}):`, `# bind = "0.0.0.0:${DEFAULT_BIND_PORT}"`]),
      "",
      "# Optional — the LAN IP to advertise in the pairing QR; pin it with a DHCP reservation:",
      '# advertised_ip = "192.168.1.50"',
      "",
      "# Optional — where the SQLite event store lives (default store.sqlite):",
      '# store_path = "store.sqlite"',
      "",
      "# Where this store publishes its committed events. Both values are the whole fleet's, not this",
      "# store's, and they must match the [nats] section of cloud.toml on the cloud box — which is why",
      "# they are generated rather than typed. The server URL is NOT here: it carries the broker token,",
      "# so it lives in the env file below.",
      "#",
      "# Keep this table LAST. Everything above it is a top-level key, and a commented line moved below",
      "# this header would be read as part of [nats] and refused at load.",
      "[nats]",
      `stream = "${FLEET_STREAM}"`,
      `subject = "${FLEET_SUBJECT}"`,
      "",
    ].join("\n");
  };

  // The second file the operator carries: the box's environment, holding the one real secret. Kept
  // apart from config.toml on purpose — this one is mode-0600 and root-owned, that one is not.
  const envFile = () => {
    const key = issued();
    return [
      "# pos_edge environment — the store's secrets. Install it as root:",
      "#   sudo install -o root -g root -m 0600 env /etc/pos-edge/env",
      "# The service unit reads it via EnvironmentFile=-/etc/pos-edge/env.",
      "",
      "# The scoped store key (read_config + relay_orders). Shown once at issuance and not",
      "# recoverable — revoke and re-issue in the console if this file is lost.",
      key ? `POS_EDGE_SYNC_KEY=${key.token}` : "POS_EDGE_SYNC_KEY=  # issue a key in step 2, or paste one here",
      "",
      "# Where this store publishes its committed events. config.toml already names the stream and the",
      "# subject; this is the one part left, and it carries the broker token — which is why it is here.",
      "#",
      "# The console cannot fill it in. Unlike the store key above, the NATS token is ONE secret shared",
      "# by the whole fleet, held on the cloud box, so putting it in a browser would spread it across",
      "# every machine in the estate. Recover it on the cloud box and uncomment this line:",
      "#",
      "#   sudo sed -n 's/  token: //p' deploy/secrets/nats.conf",
      "#",
      `# POS_EDGE_NATS_URL=tls://:<that token>@${window.location.hostname}:${NATS_CLIENT_PORT}`,
      "#",
      "# The tls:// scheme is what makes the client require TLS; nats:// connects in plaintext and the",
      "# broker refuses it. The token goes in the userinfo exactly as shown. Until this line is live the",
      "# edge logs that POS_EDGE_NATS_URL is unset and the outbox holds — the store trades either way.",
      "",
    ].join("\n");
  };

  // The third artifact, and the one a technician actually runs (roadmap-v3 **R3**): a single script
  // that lays the box out correctly instead of asking someone to follow a README at 7am in a
  // restaurant. It embeds the two files above as heredocs and then does exactly what
  // `deploy/edge/pos-edge.service`'s install block documents — no more, no less, so there is one
  // definition of the layout rather than two that drift.
  //
  // The **slot layout** is the reason this is worth generating rather than typing. Since ADR-0055
  // Amendment 1 the unit's `ExecStart` is `/var/lib/pos-edge/bin/current`, a symlink the edge
  // retargets to install its own updates; a box laid out the old way (the binary at
  // `/usr/local/bin/pos-edge`) trades perfectly well and silently **never self-updates**. That is
  // exactly the kind of mistake a hand-typed install makes and nobody notices for a release or two.
  //
  // It is deliberately not a `curl | sh`: the operator downloads it, can read every line, and runs
  // it with `sudo`. Nothing in it reaches the network.
  const installer = () => {
    const store = created();
    if (!store) {
      return "";
    }
    const key = issued();
    return [
      "#!/bin/sh",
      "# pos_edge installer — generated by the new-store wizard for one specific store.",
      `# Store:  ${store.name}  (${store.store_id})`,
      `# Tenant: ${tenantName() || tenantId()}  (${tenantId()})`,
      "#",
      "# WHAT IT DOES, in order: creates the service user and the state directory, puts the binary in",
      "# the first update slot and points `current` at it, writes the bootstrap config and the",
      "# environment file (mode 0600, root-owned), installs the systemd unit, then enables and starts",
      "# the service. Idempotent: running it twice is safe and re-applies the same layout.",
      "#",
      "# THIS FILE CONTAINS THE STORE'S KEY. Treat it as you would a password, and delete it once the",
      "# box is up. Revoke and re-issue in the console if it leaks.",
      "#",
      "# RUN IT AS:  sudo sh install-pos-edge.sh /path/to/pos-edge /path/to/pos-edge.service",
      "",
      "set -eu",
      "",
      'BINARY="${1:?usage: install-pos-edge.sh <pos-edge binary> <pos-edge.service unit>}"',
      'UNIT="${2:?the unit file ships in deploy/edge/pos-edge.service}"',
      'STATE=/var/lib/pos-edge',
      "",
      '[ "$(id -u)" -eq 0 ] || { echo "run me as root (sudo)" >&2; exit 1; }',
      '[ -f "$BINARY" ] || { echo "no such binary: $BINARY" >&2; exit 1; }',
      '[ -f "$UNIT" ] || { echo "no such unit file: $UNIT" >&2; exit 1; }',
      "",
      "# The service account. No login shell and no home: it only ever runs one program.",
      'id -u pos >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin pos',
      "",
      "# The update slot layout (ADR-0055 Amendment 1). `current` is what the unit starts and what the",
      "# edge retargets on a successful update; without it the box never self-updates.",
      "#",
      "# A box that already has `current` is one the edge is managing: it may be running slot-b after",
      "# an over-the-air update, and re-laying slot-a would point `current` back at whatever binary",
      "# this installer was handed — a silent downgrade of a shop that had updated itself. So the",
      "# slots are laid out once, and a re-run refreshes the config, the unit and the rescue copy",
      "# without touching the running binary. That is what makes running this twice safe.",
      'install -d -o pos -g pos "$STATE" "$STATE/bin"',
      'if [ -e "$STATE/bin/current" ]; then',
      '  echo "bin/current exists — leaving the installed binary alone (the edge manages its own updates)"',
      "else",
      '  install -o pos -g pos -m 0755 "$BINARY" "$STATE/bin/slot-a"',
      '  ln -sfn slot-a "$STATE/bin/current"',
      '  chown -h pos:pos "$STATE/bin/current"',
      "fi",
      "",
      "# The operator's rescue copy, and what `pos-edge --self-test` is run from by hand. Not what the",
      "# service runs.",
      'install -o root -g root -m 0755 "$BINARY" /usr/local/bin/pos-edge',
      "",
      "# The bootstrap config: which store this is and which cloud to dial. No credential in it.",
      `cat > "$STATE/config.toml" <<'POS_EDGE_CONFIG'`,
      configToml(),
      "POS_EDGE_CONFIG",
      'chown pos:pos "$STATE/config.toml"',
      'chmod 0644 "$STATE/config.toml"',
      "",
      "# The environment file: the one real secret. Root-owned, mode 0600, never world-readable.",
      "install -d -o root -g root -m 0755 /etc/pos-edge",
      "cat > /etc/pos-edge/env <<'POS_EDGE_ENV'",
      envFile(),
      "POS_EDGE_ENV",
      "chown root:root /etc/pos-edge/env",
      "chmod 0600 /etc/pos-edge/env",
      "",
      "# The service.",
      'install -o root -g root -m 0644 "$UNIT" /etc/systemd/system/pos-edge.service',
      "systemctl daemon-reload",
      "systemctl enable --now pos-edge",
      "",
      "systemctl --no-pager --lines=0 status pos-edge || true",
      "echo",
      `echo "pos_edge installed for ${store.name} (${store.store_id})."`,
      'echo "Next: open http://<this box>:' + (bindPort().trim() || DEFAULT_BIND_PORT) + '/ on a device on the shop LAN and pair it."',
      ...(key
        ? ['echo "Now DELETE this installer — it contains the store key."']
        : ['echo "WARNING: no key was issued, so /etc/pos-edge/env has no credential. Config sync and the order relay will not work until one is installed."']),
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
