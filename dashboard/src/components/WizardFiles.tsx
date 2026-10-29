// The files the new-store wizard hands over on its last step: `config.toml`, the env file and
// both installers, plus the Windows setup file (ADR-0141). Split out of `NewStore.tsx` so the
// wizard's own function stays within its size ceiling (ADR-0172), and so the one condition all of
// them share lives in one place: none is offered from a console on plain http (pilot F1), because
// each names this console's origin as the store's cloud and a store dials its cloud over https only
// (`consoleIsHttps` in `lib/handoff.ts`).

import { createSignal, Show } from "solid-js";

import { t } from "../i18n";
// The four generated artifacts live in one module because a CI gate renders and parses them; see
// `installers.mjs` and `scripts/installer-syntax.mjs`. This component supplies the values and
// nothing else, which is also what lets a Windows installer exist at all (issue #182).
import {
  DEFAULT_BIND_PORT,
  configToml as renderConfigToml,
  envFile as renderEnvFile,
  linuxInstaller,
  windowsInstaller,
} from "../installers.mjs";
import type { InstallerValues } from "../installers.d.mts";
import { consoleIsHttps, downloadFile } from "../lib/handoff";
import { tenantId, tenantName } from "../state/session";
import type { Store } from "../api/schemas";
import { SetupFile } from "./SetupFile";
import { Banner, Button, TextField } from "./ui";

export function WizardFiles(props: {
  /** The store the wizard just created. */
  readonly store: Store;
  /** The key issued for it, shown once; `null` when the operator skipped it. */
  readonly keyToken: string | null;
}) {
  // A box fact, not a store fact: the listen port never reaches the registry, it only shapes the
  // generated `config.toml`. Empty means "leave it out and take the edge's default", which is why the
  // field lives on the handoff step beside the file rather than on step 1 beside the store's name.
  const [bindPort, setBindPort] = createSignal("");

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
  // (issue #182). This component's only job is to supply the values.
  const values = (): InstallerValues => ({
    storeName: props.store.name,
    storeId: props.store.store_id,
    tenantLabel: tenantName() || tenantId(),
    tenantId: tenantId(),
    cloudUrl: window.location.origin,
    cloudHost: window.location.hostname,
    bindPort: bindPort(),
    key: props.keyToken,
  });

  const configToml = () => renderConfigToml(values());

  const envFile = () => renderEnvFile(values());

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
  // pos_cloud (same origin), so the browser saves the file under the name given here. The helper
  // lives in `lib/handoff.ts` because the Stores screen hands the same four files over again for a
  // store this wizard is no longer holding, which is how a shop replaces a machine that died.
  const downloadConfig = () => downloadFile("config.toml", configToml(), "application/toml");
  const downloadEnv = () => downloadFile("env", envFile(), "text/plain");

  return (
    <Show
      when={consoleIsHttps()}
      fallback={<Banner tone="danger" message={t("handoff.needsHttps")} />}
    >
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
        <Show when={props.keyToken === null}>
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

      <Installers
        values={values}
        storeId={props.store.store_id}
        hasKey={props.keyToken !== null}
      />
    </Show>
  );
}

/** The two installers and the Windows setup file: the step's outcome, and the files it ends on. */
function Installers(props: {
  readonly values: () => InstallerValues;
  readonly storeId: string;
  readonly hasKey: boolean;
}) {
  const downloadInstaller = () =>
    downloadFile(
      "install-pos-edge.sh",
      linuxInstaller(props.values()),
      "application/x-shellscript",
    );
  const downloadWindowsInstaller = () =>
    downloadFile("install-pos-edge.ps1", windowsInstaller(props.values()), "text/plain");

  return (
    <>
      {/* The installer (R3). Last of the three because it *contains* the two above — an
          operator who takes only this file has everything, and the two separate downloads
          stay for the cases the script cannot cover: a Windows box, a hand-managed host, or
          a technician who wants to read the config before it is written. */}
      {/* The wizard's outcome: the installer block exists only at the last step, with a
          store created and its key decided, so waiting on it is waiting for a handoff that
          can actually bring a shop online. */}
      <div data-outcome="installer-ready" class="flex flex-col gap-2">
        <span class="text-sm font-medium text-ink">{t("wizard.installerTitle")}</span>
        <p class="text-sm text-ink-muted">{t("wizard.installerHint")}</p>
        <Banner tone="danger" message={t("wizard.installerSecret")} />
        <div class="flex gap-2">
          <Button data-step="downloadInstaller" onClick={downloadInstaller}>
            {t("wizard.downloadInstaller")}
          </Button>
        </div>
      </div>

      {/* The Windows setup file (ADR-0141): the release's own executable, named so a
          double-click installs this store — the shortest way a Windows till comes up. */}
      <SetupFile tenantId={tenantId()} storeId={props.storeId} hasKey={props.hasKey} />

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
    </>
  );
}
