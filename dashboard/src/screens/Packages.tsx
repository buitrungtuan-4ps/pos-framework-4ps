// Packages ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md)): the
// tenant's packages and their published versions, and the pin that says which version every store
// runs. Locking every store to a version is also how HQ rolls back: lock the earlier one.
//
// The pin is the `package` node's settings at tenant scope, written in the order
// `lib/package-pin.ts` gives, each write republishing every store. A store whose edge is too old for
// the version keeps what it runs until it reports a release that can; the store's own page says so.

import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import type { PackageRecord, PackageVersionRecord } from "../api/schemas";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, Skeleton, StatusBadge } from "../components/ui";
import { type Column, ConfirmDialog, DataTable, EmptyState } from "../components/kit";
import { toast } from "../components/Toast";
import { apiMessage } from "../lib/errors";
import { formatInstant } from "../lib/format";
import { type PackagePin, type PinRequest, pinWrites, tenantPin } from "../lib/package-pin";

type Loaded = { readonly record: PackageRecord; readonly versions: PackageVersionRecord[] };

/** The label of `versionId` among every loaded version, or the id itself for one no longer listed. */
function labelOf(loaded: readonly Loaded[], versionId: string | null): string {
  for (const entry of loaded) {
    const found = entry.versions.find((v) => v.content.package_version_id === versionId);
    if (found) {
      return found.content.version_label;
    }
  }
  return versionId ?? "";
}

export function Packages() {
  const [packages, setPackages] = createSignal<Loaded[] | null>(null);
  const [pin, setPin] = createSignal<PackagePin | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // The version a lock is being confirmed for, or `null` with no dialog open.
  const [locking, setLocking] = createSignal<PackageVersionRecord | null>(null);
  const [clearing, setClearing] = createSignal(false);

  const load = async () => {
    setError("");
    try {
      const [records, values] = await Promise.all([
        api.listPackages(tenantId()),
        api.listSettingValues(tenantId()),
      ]);
      const loaded = await Promise.all(
        records.map(async (record) => ({
          record,
          versions: await api.listPackageVersions(tenantId(), record.package_id),
        })),
      );
      setPackages(loaded);
      setPin(tenantPin(values));
    } catch (caught) {
      setError(apiMessage(caught));
    }
  };

  onScopedContext("tenant", () => void load());

  /** Sends `request`'s writes in order, stopping at the first refusal. */
  const apply = async (request: PinRequest, done: string) => {
    setBusy(true);
    setError("");
    try {
      for (const write of pinWrites(request)) {
        await api.putSetting(
          tenantId(),
          write.settingKey,
          "SETTING_SCOPE_TENANT",
          tenantId(),
          write.value,
        );
      }
      toast.ok(done);
      setLocking(null);
      setClearing(false);
      await load();
    } catch (caught) {
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const lockVersion = (version: PackageVersionRecord) => setLocking(version);

  const confirmLock = () => {
    const version = locking();
    if (version) {
      void apply(
        { mode: "PACKAGE_MODE_LOCKED", versionId: version.content.package_version_id },
        t("packages.locked", { version: version.content.version_label }),
      );
    }
  };

  const confirmClear = () =>
    void apply({ mode: "PACKAGE_MODE_UNSPECIFIED" }, t("packages.cleared"));

  const isLocked = (version: PackageVersionRecord) =>
    pin()?.mode === "PACKAGE_MODE_LOCKED" &&
    pin()?.lockedVersionId === version.content.package_version_id;

  const columns = (): Column<PackageVersionRecord>[] => [
    {
      key: "label",
      header: t("packages.version"),
      cell: (row) => row.content.version_label,
      sortValue: (row) => row.content.version_label,
    },
    {
      key: "minEdge",
      header: t("packages.minEdge"),
      cell: (row) => row.content.min_edge_version,
    },
    {
      key: "published",
      header: t("packages.published"),
      cell: (row) => formatInstant(Date.parse(row.create_time)),
      sortValue: (row) => row.create_time,
    },
    {
      key: "pin",
      header: t("packages.pinColumn"),
      cell: (row) => (
        <Show when={isLocked(row)}>
          <StatusBadge tone="active" label={t("packages.lockedBadge")} />
        </Show>
      ),
    },
  ];

  const loadedView = () => {
    const loaded = packages();
    const current = pin();
    return loaded && current ? { loaded, current } : null;
  };

  /** What the tenant's pin means for its stores, in words. */
  const pinSummary = (loaded: readonly Loaded[], current: PackagePin) => {
    switch (current.mode) {
      case "PACKAGE_MODE_LOCKED":
        return t("packages.summaryLocked", { version: labelOf(loaded, current.lockedVersionId) });
      case "PACKAGE_MODE_CHOOSE":
        return t("packages.summaryChoose", {
          count: current.allowedVersionIds.length,
          version: labelOf(loaded, current.defaultVersionId),
        });
      default:
        return t("packages.summaryNone");
    }
  };

  return (
    <div>
      <PageHeader title={t("packages.title")} description={t("packages.description")} />
      <RequireContext need="tenant">
        <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show
          when={loadedView()}
          fallback={<Skeleton label={t("common.loading")} rows={4} />}
        >
          {(view) => (
            <div class="flex flex-col gap-4">
              <Card
                title={t("packages.pinTitle")}
                actions={
                  <Show when={view().current.mode !== "PACKAGE_MODE_UNSPECIFIED"}>
                    <Button variant="secondary" disabled={busy()} onClick={() => setClearing(true)}>
                      {t("packages.clear")}
                    </Button>
                  </Show>
                }
              >
                <Show
                  when={view().current.mode === "PACKAGE_MODE_LOCKED"}
                  fallback={
                    <p class="text-sm text-ink">{pinSummary(view().loaded, view().current)}</p>
                  }
                >
                  <p class="text-sm text-ink" data-outcome="package-pinned">
                    {pinSummary(view().loaded, view().current)}
                  </p>
                </Show>
              </Card>
              <Show
                when={view().loaded.length > 0}
                fallback={
                  <Card title={t("packages.listTitle")}>
                    <EmptyState title={t("packages.empty")} description={t("packages.emptyHint")} />
                  </Card>
                }
              >
                <For each={view().loaded}>
                  {(entry) => (
                    <Card title={entry.record.name}>
                      <DataTable
                        columns={columns()}
                        rows={entry.versions}
                        searchText={(row) => row.content.version_label}
                        pageSize={12}
                        empty={<EmptyState title={t("packages.noVersions")} />}
                        actionsHeader={t("common.actions")}
                        actions={(row) => (
                          <Show when={!isLocked(row)}>
                            <Button
                              variant="secondary"
                              disabled={busy()}
                              onClick={() => lockVersion(row)}
                              data-step="lockVersion"
                            >
                              {t("packages.lock")}
                            </Button>
                          </Show>
                        )}
                      />
                    </Card>
                  )}
                </For>
              </Show>
            </div>
          )}
        </Show>

        <ConfirmDialog
          open={locking() !== null}
          title={t("packages.lockTitle")}
          message={t("packages.lockMessage", { version: locking()?.content.version_label ?? "" })}
          confirmLabel={t("packages.lock")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          busy={busy()}
          onConfirm={confirmLock}
          onCancel={() => setLocking(null)}
          data-step="confirmLock"
        />
        <ConfirmDialog
          open={clearing()}
          title={t("packages.clearTitle")}
          message={t("packages.clearMessage")}
          confirmLabel={t("packages.clear")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          busy={busy()}
          danger
          onConfirm={confirmClear}
          onCancel={() => setClearing(false)}
        />
      </RequireContext>
    </div>
  );
}
