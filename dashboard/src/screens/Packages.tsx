// Packages ([ADR-0189](../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md)): the
// tenant's packages and their published versions, and the pin that says which version every store
// runs. Locking every store to a version is also how HQ rolls back: lock the earlier one.
//
// The pin is the `package` node's settings at tenant scope, written in the order
// `lib/package-pin.ts` gives, each write republishing every store. A store whose edge is too old for
// the version keeps what it runs until it reports a release that can; the store's own page says so.

import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import { listPackages, listPackageVersions } from "../api/packages";
import type { PackageRecord, PackageVersionRecord } from "../api/schemas";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, Skeleton, StatusBadge } from "../components/ui";
import { type Column, ConfirmDialog, DataTable, EmptyState } from "../components/kit";
import { toast } from "../components/Toast";
import { apiMessage } from "../lib/errors";
import { formatInstant } from "../lib/format";
import { type PackagePin, type PinRequest, pinWrites, tenantPin } from "../lib/package-pin";
import { ChoiceCard } from "./packages/ChoiceCard";
import { StoreChoiceCard } from "./packages/StoreChoiceCard";

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

/** Every loaded version, package by package. */
function allVersions(loaded: readonly Loaded[]): PackageVersionRecord[] {
  return loaded.flatMap((entry) => entry.versions);
}

/** What the tenant's pin means for its stores, in words. */
function pinSummary(loaded: readonly Loaded[], current: PackagePin): string {
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
}

/** The pin as it stands, and the way to remove it. */
function PinCard(props: {
  loaded: readonly Loaded[];
  current: PackagePin;
  busy: boolean;
  onClear: () => void;
}) {
  const summary = () => pinSummary(props.loaded, props.current);
  return (
    <Card
      title={t("packages.pinTitle")}
      actions={
        <Show when={props.current.mode !== "PACKAGE_MODE_UNSPECIFIED"}>
          <Button variant="secondary" disabled={props.busy} onClick={() => props.onClear()}>
            {t("packages.clear")}
          </Button>
        </Show>
      }
    >
      <Show
        when={props.current.mode === "PACKAGE_MODE_LOCKED"}
        fallback={<p class="text-sm text-ink">{summary()}</p>}
      >
        <p class="text-sm text-ink" data-outcome="package-pinned">
          {summary()}
        </p>
      </Show>
    </Card>
  );
}

/** One package's versions, each one a lock away from running on every store. */
function VersionsCard(props: {
  entry: Loaded;
  current: PackagePin;
  busy: boolean;
  onLock: (version: PackageVersionRecord) => void;
}) {
  const isLocked = (row: PackageVersionRecord) =>
    props.current.mode === "PACKAGE_MODE_LOCKED" &&
    props.current.lockedVersionId === row.content.package_version_id;
  const lockVersion = (row: PackageVersionRecord) => props.onLock(row);
  const rollBack = (row: PackageVersionRecord) => props.onLock(row);
  const position = (row: PackageVersionRecord) =>
    props.entry.versions.findIndex((v) => v.content.package_version_id === row.content.package_version_id);
  const lockedAt = () =>
    props.entry.versions.findIndex(
      (v) => props.current.mode === "PACKAGE_MODE_LOCKED" && v.content.package_version_id === props.current.lockedVersionId,
    );
  /** An earlier version than the one every store runs: running it is a rollback. */
  const isEarlier = (row: PackageVersionRecord) => position(row) < lockedAt();
  /** The locked version is not this package's newest: the stores were rolled back to it. */
  const rolledBack = () => lockedAt() >= 0 && lockedAt() < props.entry.versions.length - 1;
  const columns = (): Column<PackageVersionRecord>[] => [
    {
      key: "label",
      header: t("packages.version"),
      cell: (row) => row.content.version_label,
      sortValue: (row) => row.content.version_label,
    },
    { key: "minEdge", header: t("packages.minEdge"), cell: (row) => row.content.min_edge_version },
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
          <Show
            when={rolledBack()}
            fallback={<StatusBadge tone="active" label={t("packages.lockedBadge")} />}
          >
            <span data-outcome="package-rolled-back">
              <StatusBadge tone="active" label={t("packages.rolledBackBadge")} />
            </span>
          </Show>
        </Show>
      ),
    },
  ];
  return (
    <Card title={props.entry.record.name}>
      <DataTable
        columns={columns()}
        rows={props.entry.versions}
        searchText={(row) => row.content.version_label}
        pageSize={12}
        empty={<EmptyState title={t("packages.noVersions")} />}
        actionsHeader={t("common.actions")}
        actions={(row) => (
          <Show when={!isLocked(row)}>
            <Show
              when={isEarlier(row)}
              fallback={
                <Button
                  variant="secondary"
                  disabled={props.busy}
                  onClick={() => lockVersion(row)}
                  data-step="lockVersion"
                >
                  {t("packages.lock")}
                </Button>
              }
            >
              <Button
                variant="secondary"
                disabled={props.busy}
                onClick={() => rollBack(row)}
                data-step="rollBack"
              >
                {t("packages.rollBack")}
              </Button>
            </Show>
          </Show>
        )}
      />
    </Card>
  );
}

/** The tenant's packages and pin, and the writes that change the pin. */
function usePackages() {
  const [packages, setPackages] = createSignal<Loaded[] | null>(null);
  const [pin, setPin] = createSignal<PackagePin | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [revision, setRevision] = createSignal(0);
  const load = async () => {
    setError("");
    try {
      const [records, values] = await Promise.all([
        listPackages(tenantId()),
        api.listSettingValues(tenantId()),
      ]);
      const versions = await Promise.all(
        records.map((record) => listPackageVersions(tenantId(), record.package_id)),
      );
      setPackages(records.map((record, at) => ({ record, versions: versions[at] ?? [] })));
      setPin(tenantPin(values));
    } catch (caught) {
      setError(apiMessage(caught));
    }
  };
  /** Sends `request`'s writes in order, stopping at the first refusal; `true` once all landed. */
  const apply = async (request: PinRequest, done: string): Promise<boolean> => {
    setBusy(true);
    setError("");
    try {
      for (const write of pinWrites(request)) {
        await api.putSetting(tenantId(), write.settingKey, "SETTING_SCOPE_TENANT", tenantId(), write.value);
      }
      toast.ok(done);
      await load();
      setRevision((n) => n + 1);
      return true;
    } catch (caught) {
      setError(apiMessage(caught));
      toast.error(apiMessage(caught));
      return false;
    } finally {
      setBusy(false);
    }
  };
  const view = () => {
    const loaded = packages();
    const current = pin();
    return loaded && current ? { loaded, current } : null;
  };
  return { load, apply, view, error, busy, revision };
}

export function Packages() {
  const state = usePackages();
  // The version a lock is being confirmed for, or `null` with no dialog open.
  const [locking, setLocking] = createSignal<PackageVersionRecord | null>(null);
  const [clearing, setClearing] = createSignal(false);
  onScopedContext("tenant", () => void state.load());

  const confirmLock = () => {
    const version = locking()?.content;
    if (version) {
      const request = { mode: "PACKAGE_MODE_LOCKED", versionId: version.package_version_id } as const;
      const done = t("packages.locked", { version: version.version_label });
      void state.apply(request, done).then((landed) => landed && setLocking(null));
    }
  };
  const confirmClear = () =>
    void state
      .apply({ mode: "PACKAGE_MODE_UNSPECIFIED" }, t("packages.cleared"))
      .then((landed) => landed && setClearing(false));

  return (
    <div>
      <PageHeader title={t("packages.title")} description={t("packages.description")} />
      <RequireContext need="tenant">
        <Show when={state.error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show when={state.view()} fallback={<Skeleton label={t("common.loading")} rows={4} />}>
          {(shown) => (
            <div class="flex flex-col gap-4">
              <PinCard {...shown()} busy={state.busy()} onClear={() => setClearing(true)} />
              <Show when={storeId()}>
                <StoreChoiceCard versions={allVersions(shown().loaded)} revision={state.revision()} />
              </Show>
              <Show
                when={shown().loaded.length > 0}
                fallback={
                  <Card title={t("packages.listTitle")}>
                    <EmptyState title={t("packages.empty")} description={t("packages.emptyHint")} />
                  </Card>
                }
              >
                <ChoiceCard
                  versions={allVersions(shown().loaded)}
                  current={shown().current}
                  busy={state.busy()}
                  onSave={(request) => void state.apply(request, t("packages.choiceDone"))}
                />
                <For each={shown().loaded}>
                  {(entry) => (
                    <VersionsCard
                      entry={entry}
                      current={shown().current}
                      busy={state.busy()}
                      onLock={setLocking}
                    />
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
          busy={state.busy()}
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
          busy={state.busy()}
          danger
          onConfirm={confirmClear}
          onCancel={() => setClearing(false)}
        />
      </RequireContext>
    </div>
  );
}
