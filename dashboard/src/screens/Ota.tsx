// OTA rollouts (ADR-0078, Track O3). Two panes in one operational glance: fleet-wide rollout
// progress — which stores are running which binary and whether their last self-test passed, from the
// O1 liveness read model extended with OTA reports — and, for the store in context, the first-class
// levers that publish a rollout and engage its kill switch (the `fleet_update` config node, without
// hand-editing raw JSON). The progress pane is tenant-scoped and polls so it stays current; the
// levers are store-scoped and behind console.ota.publish (the server enforces it — a viewer sees the
// progress but a publish returns 403).

import { createSignal, For, onCleanup, onMount, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { FleetStore, OtaPlacement, OtaRollout } from "../api/types";
import { t, type MessageKey } from "../i18n";
import { formatRelativeAge } from "../lib/format";
import { contextReady, onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextArea, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

/** How often the progress pane re-reads the fleet, so "installed" and "reported" stay current. */
const POLL_MS = 15_000;

/** The deployment rings, least to most exposed (pos-core `Ring`); the wire values the node validates. */
const RINGS = ["lab", "pilot", "fleet"] as const;
type RingWire = (typeof RINGS)[number];

const RING_LABEL: Record<RingWire, MessageKey> = {
  lab: "ota.ring.lab",
  pilot: "ota.ring.pilot",
  fleet: "ota.ring.fleet",
};

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

export function Ota() {
  const [stores, setStores] = createSignal<FleetStore[] | null>(null);
  const [rollout, setRollout] = createSignal<OtaRollout | null>(null);
  const [rolloutLoaded, setRolloutLoaded] = createSignal(false);
  const [placement, setPlacement] = createSignal<OtaPlacement | null>(null);
  const [placementLoaded, setPlacementLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // Publish-form fields. Empty until the operator fills them; a fresh publish is live (no `halted`).
  const [targetVersion, setTargetVersion] = createSignal("");
  const [minRing, setMinRing] = createSignal<RingWire>("lab");
  const [rolloutPercent, setRolloutPercent] = createSignal("0");
  const [signingKey, setSigningKey] = createSignal("");
  const [revokedKeys, setRevokedKeys] = createSignal("");

  // Placement fields — where this store sits in the rollout. A rollout says which devices are
  // eligible; without a placement the store is eligible for nothing and never updates (ADR-0052).
  const [placementRing, setPlacementRing] = createSignal<RingWire>("fleet");
  const [canaryBucket, setCanaryBucket] = createSignal("0");

  const [confirmPublish, setConfirmPublish] = createSignal(false);
  const [confirmHalt, setConfirmHalt] = createSignal(false);
  const [confirmPlace, setConfirmPlace] = createSignal(false);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const loadFleet = async () => {
    setError("");
    setBusy(true);
    try {
      setStores(await api.listFleet(tenantId()));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const loadRollout = async () => {
    if (!storeId()) {
      setRollout(null);
      setRolloutLoaded(false);
      return;
    }
    try {
      const loaded = await api.getOtaRollout(tenantId(), storeId());
      setRollout(loaded);
      setRolloutLoaded(true);
      // Prime the form from the published rollout, so an edit starts from what is live.
      if (loaded) {
        setTargetVersion(loaded.target_version);
        setMinRing(RINGS.includes(loaded.min_ring as RingWire) ? (loaded.min_ring as RingWire) : "lab");
        setRolloutPercent(String(loaded.rollout_percent));
        setSigningKey(loaded.signing_key_id);
        setRevokedKeys((loaded.revoked_key_ids ?? []).join("\n"));
      }
    } catch (caught) {
      fail(caught);
    }
  };

  const loadPlacement = async () => {
    if (!storeId()) {
      setPlacement(null);
      setPlacementLoaded(false);
      return;
    }
    try {
      const loaded = await api.getOtaPlacement(tenantId(), storeId());
      setPlacement(loaded);
      setPlacementLoaded(true);
      // Prime the form from the published placement, so an edit starts from what is live. An unplaced
      // store keeps the form's own default rather than showing a ring it is not in.
      if (loaded) {
        setPlacementRing(
          RINGS.includes(loaded.ring as RingWire) ? (loaded.ring as RingWire) : "fleet",
        );
        setCanaryBucket(String(loaded.canary_bucket));
      }
    } catch (caught) {
      fail(caught);
    }
  };

  const load = () => {
    void loadFleet();
    void loadRollout();
    void loadPlacement();
  };

  // Load on open and whenever the tenant/store changes (never with an empty context, F0).
  onScopedContext("tenant", () => load());

  // Poll the progress pane while the screen is open, but only once a tenant is chosen.
  onMount(() => {
    const handle = setInterval(() => {
      if (contextReady("tenant") && !busy()) {
        void loadFleet();
      }
    }, POLL_MS);
    onCleanup(() => clearInterval(handle));
  });

  // Split the revoked-keys textarea on commas and newlines into a trimmed, non-empty list.
  const revokedList = (): string[] =>
    revokedKeys()
      .split(/[\s,]+/)
      .map((id) => id.trim())
      .filter((id) => id.length > 0);

  const publish = async () => {
    setConfirmPublish(false);
    setBusy(true);
    try {
      const percent = Number.parseInt(rolloutPercent(), 10);
      await api.publishOtaRollout({
        tenant_id: tenantId(),
        store_id: storeId(),
        target_version: targetVersion().trim(),
        min_ring: minRing(),
        rollout_percent: Number.isFinite(percent) ? percent : 0,
        signing_key_id: signingKey().trim(),
        revoked_key_ids: revokedList(),
      });
      toast.ok(t("ota.published"));
      await loadRollout();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const place = async () => {
    setConfirmPlace(false);
    setBusy(true);
    try {
      const bucket = Number.parseInt(canaryBucket(), 10);
      await api.publishOtaPlacement({
        tenant_id: tenantId(),
        store_id: storeId(),
        ring: placementRing(),
        canary_bucket: Number.isFinite(bucket) ? bucket : 0,
      });
      toast.ok(t("ota.placed"));
      await loadPlacement();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setHalted = async (halted: boolean) => {
    setConfirmHalt(false);
    setBusy(true);
    try {
      await api.haltOtaRollout(tenantId(), storeId(), halted);
      toast.ok(halted ? t("ota.haltedOk") : t("ota.resumedOk"));
      await loadRollout();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const selfTestBadge = (row: FleetStore) => {
    if (row.self_test_ok === null) {
      return <StatusBadge tone="neutral" label={t("ota.unknown")} />;
    }
    return (
      <StatusBadge
        tone={row.self_test_ok ? "active" : "disabled"}
        label={row.self_test_ok ? t("ota.pass") : t("ota.fail")}
      />
    );
  };

  const columns = (): Column<FleetStore>[] => [
    {
      key: "name",
      header: t("ota.store"),
      sortValue: (row) => row.name,
      cell: (row) => <span class="font-medium text-ink">{row.name}</span>,
    },
    {
      key: "installed",
      header: t("ota.installedVersion"),
      sortValue: (row) => row.installed_version ?? "",
      cell: (row) => (
        <span class="font-mono text-sm text-ink">{row.installed_version ?? t("ota.never")}</span>
      ),
    },
    {
      key: "selfTest",
      header: t("ota.selfTest"),
      sortValue: (row) => (row.self_test_ok === null ? -1 : row.self_test_ok ? 1 : 0),
      cell: (row) => selfTestBadge(row),
    },
    {
      key: "reported",
      header: t("ota.reported"),
      sortValue: (row) => row.reported_at_ms ?? 0,
      cell: (row) => (
        <span class="text-sm text-ink-muted">
          {row.reported_at_ms === null
            ? t("ota.never")
            : formatRelativeAge(ageSeconds(row.reported_at_ms))}
        </span>
      ),
    },
    {
      key: "presence",
      header: t("ota.presence"),
      sortValue: (row) => (row.online ? 1 : 0),
      cell: (row) => (
        <StatusBadge
          tone={row.online ? "active" : "disabled"}
          label={row.online ? t("ota.online") : t("ota.offline")}
        />
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.store_id}</TechnicalDetails>
      ),
    },
  ];

  // The publish form cannot submit without a target, a ring, a percent, and a signing key.
  const publishable = () =>
    targetVersion().trim().length > 0 &&
    signingKey().trim().length > 0 &&
    rolloutPercent().trim().length > 0;

  const currentRow = (label: string, value: string) => (
    <div class="flex justify-between gap-4 border-b border-line py-2 text-sm last:border-0">
      <span class="text-ink-muted">{label}</span>
      <span class="text-right text-ink">{value}</span>
    </div>
  );

  return (
    <div>
      <PageHeader title={t("ota.title")} description={t("ota.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          <Card
            title={t("ota.progress")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void loadFleet()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show
              when={stores()}
              fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}
            >
              {(loaded) => (
                <DataTable
                  columns={columns()}
                  rows={loaded()}
                  searchText={(row) => row.name}
                  pageSize={12}
                  empty={<EmptyState title={t("ota.empty")} description={t("ota.emptyHint")} />}
                />
              )}
            </Show>
          </Card>

          <Card title={t("ota.placementTitle")}>
            <Show
              when={storeId()}
              fallback={
                <EmptyState title={t("ota.pickStore")} description={t("ota.pickStoreHint")} />
              }
            >
              <div class="flex flex-col gap-4">
                <p class="text-sm text-ink-muted">{t("ota.placementHint")}</p>
                <Show when={placementLoaded()}>
                  <Show
                    when={placement()}
                    fallback={
                      <EmptyState
                        title={t("ota.unplaced")}
                        description={t("ota.unplacedHint")}
                      />
                    }
                  >
                    {(live) => (
                      <div class="rounded-token border border-line p-4">
                        <span class="mb-2 block text-sm font-medium text-ink">
                          {t("ota.placementCurrent")}
                        </span>
                        {currentRow(
                          t("ota.placementRing"),
                          t(RING_LABEL[live().ring as RingWire] ?? "ota.ring.fleet"),
                        )}
                        {currentRow(t("ota.canaryBucket"), String(live().canary_bucket))}
                      </div>
                    )}
                  </Show>
                </Show>
                <label class="block">
                  <span class="mb-1 block text-sm font-medium text-ink">
                    {t("ota.placementRing")}
                  </span>
                  <select
                    class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                    aria-label={t("ota.placementRing")}
                    value={placementRing()}
                    onChange={(event) =>
                      setPlacementRing(event.currentTarget.value as RingWire)
                    }
                  >
                    <For each={RINGS}>
                      {(ring) => <option value={ring}>{t(RING_LABEL[ring])}</option>}
                    </For>
                  </select>
                </label>
                <TextField
                  label={t("ota.canaryBucket")}
                  type="number"
                  value={canaryBucket()}
                  onInput={setCanaryBucket}
                  placeholder="0"
                />
                <p class="text-sm text-ink-muted">{t("ota.canaryBucketHint")}</p>
                <div>
                  <Button
                    variant="primary"
                    disabled={busy() || canaryBucket().trim().length === 0}
                    onClick={() => setConfirmPlace(true)}
                  >
                    {t("ota.place")}
                  </Button>
                </div>
              </div>
            </Show>
          </Card>

          <Card title={t("ota.manage")}>
            <Show
              when={storeId()}
              fallback={
                <EmptyState title={t("ota.pickStore")} description={t("ota.pickStoreHint")} />
              }
            >
              <div class="flex flex-col gap-5">
                <p class="text-sm text-ink-muted">{t("ota.manageFor", { store: storeName() })}</p>

                <Show when={rolloutLoaded()}>
                  <Show
                    when={rollout()}
                    fallback={
                      <EmptyState title={t("ota.noRollout")} description={t("ota.noRolloutHint")} />
                    }
                  >
                    {(live) => (
                      <div class="rounded-token border border-line p-4">
                        <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
                          <span class="text-sm font-medium text-ink">{t("ota.current")}</span>
                          <StatusBadge
                            tone={live().halted ? "disabled" : "active"}
                            label={live().halted ? t("ota.halted") : t("ota.liveNow")}
                          />
                        </div>
                        {currentRow(t("ota.targetVersion"), live().target_version)}
                        {currentRow(t("ota.minRing"), t(RING_LABEL[live().min_ring as RingWire] ?? "ota.ring.lab"))}
                        {currentRow(t("ota.rolloutPercent"), `${live().rollout_percent}%`)}
                        <div class="mt-3">
                          <Show
                            when={live().halted}
                            fallback={
                              <Button
                                variant="danger"
                                disabled={busy()}
                                onClick={() => setConfirmHalt(true)}
                              >
                                {t("ota.halt")}
                              </Button>
                            }
                          >
                            <Button
                              variant="primary"
                              disabled={busy()}
                              onClick={() => void setHalted(false)}
                            >
                              {t("ota.resume")}
                            </Button>
                          </Show>
                        </div>
                      </div>
                    )}
                  </Show>
                </Show>

                <div class="flex flex-col gap-4">
                  <span class="text-sm font-medium text-ink">{t("ota.publishTitle")}</span>
                  <p class="text-sm text-ink-muted">{t("ota.publishHint")}</p>
                  <TextField
                    label={t("ota.targetVersion")}
                    value={targetVersion()}
                    onInput={setTargetVersion}
                    placeholder="1.4.0"
                  />
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">{t("ota.minRing")}</span>
                    <select
                      class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                      aria-label={t("ota.minRing")}
                      value={minRing()}
                      onChange={(event) => setMinRing(event.currentTarget.value as RingWire)}
                    >
                      <For each={RINGS}>
                        {(ring) => <option value={ring}>{t(RING_LABEL[ring])}</option>}
                      </For>
                    </select>
                  </label>
                  <TextField
                    label={t("ota.rolloutPercent")}
                    type="number"
                    value={rolloutPercent()}
                    onInput={setRolloutPercent}
                    placeholder="0"
                  />
                  <TextField
                    label={t("ota.signingKey")}
                    value={signingKey()}
                    onInput={setSigningKey}
                    placeholder="0011223344556677"
                  />
                  <TextArea
                    label={t("ota.revokedKeys")}
                    value={revokedKeys()}
                    onInput={setRevokedKeys}
                    rows={3}
                    placeholder={t("ota.revokedKeysHint")}
                  />
                  <div>
                    <Button
                      variant="primary"
                      disabled={busy() || !publishable()}
                      onClick={() => setConfirmPublish(true)}
                    >
                      {t("ota.publish")}
                    </Button>
                  </div>
                </div>
              </div>
            </Show>
          </Card>
        </div>

        <ConfirmDialog
          open={confirmPublish()}
          title={t("ota.confirmPublish")}
          message={t("ota.confirmPublishBody", {
            version: targetVersion(),
            ring: t(RING_LABEL[minRing()]),
            store: storeName(),
          })}
          confirmLabel={t("ota.publish")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          busy={busy()}
          onConfirm={() => void publish()}
          onCancel={() => setConfirmPublish(false)}
        />
        <ConfirmDialog
          open={confirmPlace()}
          title={t("ota.confirmPlace")}
          message={t("ota.confirmPlaceBody", {
            store: storeName(),
            ring: t(RING_LABEL[placementRing()]),
            bucket: canaryBucket(),
          })}
          confirmLabel={t("ota.place")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          busy={busy()}
          onConfirm={() => void place()}
          onCancel={() => setConfirmPlace(false)}
        />
        <ConfirmDialog
          open={confirmHalt()}
          danger
          title={t("ota.confirmHalt")}
          message={t("ota.confirmHaltBody", { store: storeName() })}
          confirmLabel={t("ota.halt")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          busy={busy()}
          onConfirm={() => void setHalted(true)}
          onCancel={() => setConfirmHalt(false)}
        />
      </RequireContext>
    </div>
  );
}
