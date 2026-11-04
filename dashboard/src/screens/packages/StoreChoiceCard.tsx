// The store in context's own choice of package version
// ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 2). Shown
// only while the store's pin lets it choose; HQ makes the choice here, behind
// `console.config.publish`, and the store is republished. A store's own staff choosing waits for its
// own record (roadmap-v4 phase 4).

import { createEffect, createSignal, For, on, Show } from "solid-js";

import { api } from "../../api/client";
import { chooseStorePackage } from "../../api/packages";
import type { PackageVersionRecord } from "../../api/schemas";
import { t } from "../../i18n";
import { apiMessage } from "../../lib/errors";
import { storePin } from "../../lib/package-pin";
import { onScopedContext } from "../../lib/scoped";
import { storeId, tenantId } from "../../state/session";
import { Banner, Button, Card } from "../../components/ui";
import { toast } from "../../components/Toast";

type StorePin = ReturnType<typeof storePin>;

export function StoreChoiceCard(props: {
  versions: readonly PackageVersionRecord[];
  /** Bumped by every tenant pin write, so this card reads the store's pin again. */
  revision: number;
}) {
  const [pin, setPin] = createSignal<StorePin | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const load = async () => {
    setError("");
    try {
      setPin(storePin(await api.effectiveSettings(tenantId(), storeId())));
    } catch (caught) {
      setError(apiMessage(caught));
    }
  };
  onScopedContext("store", () => void load());
  createEffect(on(() => props.revision, () => storeId() && void load(), { defer: true }));

  const labelOf = (versionId: string | null) =>
    props.versions.find((v) => v.content.package_version_id === versionId)?.content
      .version_label ?? versionId ?? "";

  const chooseForStore = async (versionId: string | null) => {
    setBusy(true);
    setError("");
    try {
      await chooseStorePackage(tenantId(), storeId(), versionId);
      toast.ok(versionId ? t("packages.storeChosen", { version: labelOf(versionId) }) : t("packages.storeCleared"));
      await load();
    } catch (caught) {
      setError(apiMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Show when={pin()?.mode === "PACKAGE_MODE_CHOOSE" ? pin() : null}>
      {(current) => (
        <Card title={t("packages.storeTitle")}>
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show
            when={current().chosenVersionId}
            fallback={
              <p class="mb-3 text-sm text-ink">
                {t("packages.storeDefault", { version: labelOf(current().defaultVersionId) })}
              </p>
            }
          >
            {(chosen) => (
              <p class="mb-3 text-sm text-ink" data-outcome="package-chosen">
                {t("packages.storeRuns", { version: labelOf(chosen()) })}
              </p>
            )}
          </Show>
          <div class="flex flex-wrap gap-2">
            <For each={current().allowedVersionIds.filter((id) => id !== current().chosenVersionId)}>
              {(versionId) => (
                <Button
                  variant="secondary"
                  disabled={busy()}
                  onClick={() => void chooseForStore(versionId)}
                  data-step="chooseForStore"
                >
                  {t("packages.storeChoose", { version: labelOf(versionId) })}
                </Button>
              )}
            </For>
            <Show when={current().chosenVersionId}>
              <Button variant="ghost" disabled={busy()} onClick={() => void chooseForStore(null)}>
                {t("packages.storeClear")}
              </Button>
            </Show>
          </div>
        </Card>
      )}
    </Show>
  );
}
