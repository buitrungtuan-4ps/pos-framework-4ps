// The Windows setup file (ADR-0140, ADR-0141): a hosted release's own `pos-edge.exe`, downloaded
// under the name that makes it install *this* store and dial *this* cloud when it is double-clicked
// on the shop's PC. No script, no parameters, no elevated shell to open by hand.
//
// # Why a link and not a button
//
// A release binary is tens of megabytes. A plain `<a download>` lets the browser stream it to disk
// behind its own progress bar; fetching it into a `Blob` first would hold the whole file in the tab
// and show nothing while it did. The console's session is a cookie on this origin, so the link is
// authenticated exactly as `fetch` is.
//
// # Why it looks first
//
// A link to a release the cloud does not hold fails in the browser's download shelf, where nobody
// reads why. So the release is looked up with the read the OTA screen uses, and the link appears
// only once a Windows build is hosted — with the reason, and where to fix it, when it is not.
//
// # Where the cloud's address comes from
//
// The browser's own location, as every generated installer does it: the console is served by
// `pos_cloud`, so the origin the operator is looking at *is* the origin the store must dial. The
// cloud writes it into the file name, which the edge reads back as `https` — so a console reached
// over plain http at a network address cannot hand out a file that would work, and says so instead.

import { createResource, createSignal, onMount, Show } from "solid-js";

import { api } from "../api/client";
import { t } from "../i18n";
import { apiMessage } from "../lib/errors";
import { Banner, TextField } from "./ui";

/** The Windows build the cloud serves a setup file from — the one the release workflow builds. */
const WINDOWS_TARGET = "x86_64-pc-windows-msvc";

/** A console opened on the cloud machine itself, where the edge's reader dials plain http. */
function isLoopback(hostname: string): boolean {
  return hostname === "localhost" || hostname === "[::1]" || /^127\./u.test(hostname);
}

/** The download URL: the release, the store, and the address this browser reached the cloud at. */
export function setupFileHref(release: string, storeId: string, cloud: string): string {
  const query = new URLSearchParams({ store_id: storeId, cloud });
  return `/admin/ota/releases/${encodeURIComponent(release)}/installer?${query.toString()}`;
}

export function SetupFile(props: {
  readonly tenantId: string;
  readonly storeId: string;
  /** Whether a store key was just issued, so the setup window has one to be pasted. */
  readonly hasKey: boolean;
}) {
  const [release, setRelease] = createSignal("");
  const secure = window.location.protocol === "https:" || isLoopback(window.location.hostname);

  // The version this store's rollout targets is the one it would be updated to anyway, so the field
  // starts there — but never over something the operator has typed, and a store with no rollout
  // (every store the wizard has just created) starts blank.
  onMount(() => {
    api.getOtaRollout(props.tenantId, props.storeId).then(
      (rollout) => {
        if (rollout && release().trim() === "") {
          setRelease(rollout.target_version);
        }
      },
      () => undefined,
    );
  });

  const [hosted] = createResource(
    () => release().trim() || false,
    async (version) =>
      (await api.listHostedRelease(version)).artifacts.some(
        (artifact) => artifact.arch === WINDOWS_TARGET,
      ),
  );

  return (
    <div class="flex flex-col gap-2" data-outcome="setup-file">
      <span class="text-sm font-medium text-ink">{t("setupFile.title")}</span>
      <p class="text-sm text-ink-muted">{t("setupFile.hint")}</p>
      <Show when={secure} fallback={<Banner tone="danger" message={t("setupFile.needsHttps")} />}>
        <TextField
          label={t("setupFile.release")}
          value={release()}
          onInput={setRelease}
          hint={t("setupFile.releaseHint")}
        />
        <Show when={release().trim() !== ""}>
          <Show
            when={!hosted.error}
            fallback={<Banner tone="danger" message={apiMessage(hosted.error)} />}
          >
            <Show when={hosted() === false}>
              <Banner
                tone="danger"
                message={t("setupFile.notHosted", { release: release().trim() })}
              />
            </Show>
            <Show when={hosted() === true}>
              <div class="flex flex-wrap gap-2">
                <a
                  class="inline-flex min-h-touch items-center justify-center rounded-token border border-line bg-surface-raised px-4 text-base font-medium text-ink"
                  href={setupFileHref(release().trim(), props.storeId, window.location.host)}
                  download=""
                >
                  {t("setupFile.download")}
                </a>
              </div>
            </Show>
          </Show>
        </Show>
        <p class="text-sm text-ink-muted">
          {props.hasKey ? t("setupFile.pasteKey") : t("setupFile.noKey")}
        </p>
      </Show>
    </div>
  );
}
