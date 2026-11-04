// What the store in context runs against what it is pinned to, and why it waits when they differ
// ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 5). A
// store whose edge is too old for its pinned version keeps the version it runs; this says so in
// words, with the release it reported, rather than leaving HQ to wonder why a pin did not land.

import { createEffect, createSignal, on, Show } from "solid-js";

import { storePackage } from "../../api/packages";
import type { PackageVersionRecord, StorePackage } from "../../api/schemas";
import { type MessageKey, t } from "../../i18n";
import { apiMessage } from "../../lib/errors";
import { onScopedContext } from "../../lib/scoped";
import { storeId, tenantId } from "../../state/session";
import { Banner, Card } from "../../components/ui";

/** The sentence for each reason the cloud gives. */
const WAITING: Record<NonNullable<StorePackage["waiting"]>["reason"], MessageKey> = {
  EDGE_TOO_OLD: "packages.waitEdgeTooOld",
  RELEASE_UNKNOWN: "packages.waitReleaseUnknown",
  VERSION_MISSING: "packages.waitVersionMissing",
  PIN_UNRESOLVED: "packages.waitPinUnresolved",
  UNREADABLE: "packages.waitUnreadable",
};

export function StorePackageCard(props: {
  versions: readonly PackageVersionRecord[];
  /** Bumped by every tenant pin write, so this card reads the store again. */
  revision: number;
}) {
  const [standing, setStanding] = createSignal<StorePackage | null>(null);
  const [error, setError] = createSignal("");
  const load = async () => {
    setError("");
    try {
      setStanding(await storePackage(tenantId(), storeId()));
    } catch (caught) {
      setError(apiMessage(caught));
    }
  };
  onScopedContext("store", () => void load());
  createEffect(on(() => props.revision, () => storeId() && void load(), { defer: true }));

  const labelOf = (versionId: string | undefined) =>
    props.versions.find((v) => v.content.package_version_id === versionId)?.content
      .version_label ?? versionId ?? "";
  const running = () =>
    standing()?.running_version_id
      ? labelOf(standing()?.running_version_id)
      : t("packages.defaultPackage");

  return (
    <Card title={t("packages.storeStandingTitle")}>
      <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <Show when={standing()}>
        {(shown) => (
          <div class="flex flex-col gap-2 text-sm text-ink">
            <p data-outcome="store-package">{t("packages.storeRunning", { version: running() })}</p>
            <Show when={shown().pinned_version_id}>
              {(pinned) => <p>{t("packages.storePinned", { version: labelOf(pinned()) })}</p>}
            </Show>
            <Show when={shown().waiting}>
              {(waiting) => (
                <p class="text-ink-muted" data-outcome="package-waiting">
                  {t(WAITING[waiting().reason], {
                    release: waiting().installed_release ?? t("common.unknown"),
                    version: running(),
                  })}
                </p>
              )}
            </Show>
          </div>
        )}
      </Show>
    </Card>
  );
}
