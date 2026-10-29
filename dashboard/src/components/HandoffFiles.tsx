// The store's files in the Stores screen's replacement drawer: the listen port, the Windows setup
// file (ADR-0141) and the four generated files from `lib/handoff.ts`. Split out of `Stores.tsx` so
// that screen's function stays within its size ceiling (ADR-0172), and so the one condition they
// share sits beside them: none is offered from a console on plain http (pilot F1), because each
// names this console's origin as the store's cloud and a store dials its cloud over https only
// (`consoleIsHttps`).

import { createSignal, For, Show } from "solid-js";

import type { Store } from "../api/schemas";
import { t } from "../i18n";
import { DEFAULT_BIND_PORT } from "../installers.mjs";
import type { InstallerValues } from "../installers.d.mts";
import { consoleIsHttps, downloadHandoff, HANDOFF_FILES } from "../lib/handoff";
import { tenantId } from "../state/session";
import { SetupFile } from "./SetupFile";
import { Banner, Button, TextField } from "./ui";

export function HandoffFiles(props: {
  /** The store whose files these are. */
  readonly store: Store;
  /** The tenant's name, for the installers' own messages. */
  readonly tenantLabel: string;
  /** A key issued in the drawer, baked into the files; `null` when the operator went on without. */
  readonly keyToken: string | null;
}) {
  // A property of the *machine*, not of the store: it shapes the generated config and never reaches
  // the cloud, so it is asked for here rather than stored on the row. Blank takes the edge's default.
  const [port, setPort] = createSignal("");

  /**
   * The values every generated artifact is rendered from.
   *
   * `cloudUrl` and `cloudHost` come from the browser's own location rather than from configuration,
   * for the same reason the wizard does it: the console is served by `pos_cloud`, so the origin the
   * operator is looking at *is* the origin the store must dial. A configured value could disagree
   * with it, and the disagreement would only show up as a box that boots LAN-only.
   */
  const values = (): InstallerValues => ({
    storeName: props.store.name,
    storeId: props.store.store_id,
    tenantLabel: props.tenantLabel,
    tenantId: tenantId(),
    cloudUrl: window.location.origin,
    cloudHost: window.location.hostname,
    bindPort: port(),
    key: props.keyToken,
  });

  return (
    <Show
      when={consoleIsHttps()}
      fallback={<Banner tone="danger" message={t("handoff.needsHttps")} />}
    >
      <TextField
        label={t("wizard.bindPort")}
        value={port()}
        onInput={setPort}
        placeholder={DEFAULT_BIND_PORT}
      />
      <p class="text-sm text-ink-muted">{t("wizard.bindPortHint")}</p>
      <Show when={props.keyToken !== null}>
        <Banner tone="danger" message={t("handoff.secretWarning")} />
      </Show>
      {/* The setup file first: on Windows it is the whole install in one
          double-click (ADR-0141), and the four files below stay for what it cannot
          cover — a Linux box, a hand-managed host, a technician who wants to read
          the script before it runs on a shop's only till. */}
      <SetupFile
        tenantId={tenantId()}
        storeId={props.store.store_id}
        hasKey={props.keyToken !== null}
      />
      <For each={HANDOFF_FILES}>
        {(file) => (
          <div class="flex flex-col gap-1">
            <Button
              data-step="downloadHandoff"
              variant="secondary"
              onClick={() => downloadHandoff(file, values())}
            >
              {t(file.labelKey)}
            </Button>
            <p class="text-sm text-ink-muted">{t(file.hintKey)}</p>
          </div>
        )}
      </For>
    </Show>
  );
}
