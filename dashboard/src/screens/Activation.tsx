// Device activation codes (ADR-0050, ADR-0065): issue a one-time code a store device exchanges once
// for its credentials. The device is now chosen (or created) by **name** from the registry — no more
// typing a raw device-slot ULID. Names the tenant + store in context; the code is shown once.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Device } from "../api/types";
import { type MessageKey, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

// The device kinds offered when adding one, each mapped to a static i18n key. `kind` is free text on
// the wire, so this is a convenience list, not a closed set.
const KINDS: readonly { wire: string; key: MessageKey }[] = [
  { wire: "pos", key: "device.kind.pos" },
  { wire: "printer", key: "device.kind.printer" },
  { wire: "kds", key: "device.kind.kds" },
  { wire: "tablet", key: "device.kind.tablet" },
];

export function Activation() {
  const [devices, setDevices] = createSignal<Device[] | null>(null);
  const [selected, setSelected] = createSignal("");
  const [code, setCode] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [newName, setNewName] = createSignal("");
  const [newKind, setNewKind] = createSignal("pos");

  // Correcting a device already in the registry (production-readiness O2): `PATCH` shipped with the
  // registry in WS-C and had no caller, so a device typed in wrong, or replaced, stayed that way.
  const [editing, setEditing] = createSignal("");
  const [draftName, setDraftName] = createSignal("");
  const [pendingArchive, setPendingArchive] = createSignal<Device | null>(null);

  const fail = (caught: unknown) =>
    setError(caught instanceof ApiError ? caught.message : String(caught));

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setDevices(await api.listDevices(tenantId(), storeId()));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant/store changes — never with an empty context (F0).
  onScopedContext("store", () => void load());

  const createDevice = async () => {
    const name = newName().trim();
    if (!name) {
      setError(t("activation.deviceNameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      const device = await api.createDevice(tenantId(), storeId(), name, newKind());
      setNewName("");
      setSelected(device.device_id);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const saveRename = async (device: Device) => {
    const name = draftName().trim();
    if (!name) {
      setError(t("activation.deviceNameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateDevice(
        device.device_id,
        tenantId(),
        storeId(),
        { name, kind: device.kind, status: device.status },
        device.etag,
      );
      setEditing("");
      setDraftName("");
      toast.ok(t("activation.deviceRenamed"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setKind = async (device: Device, kind: string) => {
    setError("");
    setBusy(true);
    try {
      await api.updateDevice(
        device.device_id,
        tenantId(),
        storeId(),
        { name: device.name, kind, status: device.status },
        device.etag,
      );
      toast.ok(t("activation.deviceKindChanged"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Archiving takes a device off the roster: it stops being offered a code, and stops being counted
  // as a device this store runs. It does **not** retire a paired till — that is the store server's
  // own `POST /api/pair/revoke` (a different credential in a different tier), reachable from the
  // till's own Devices screen.
  const setStatus = async (device: Device, status: "active" | "archived") => {
    setError("");
    setBusy(true);
    try {
      await api.updateDevice(
        device.device_id,
        tenantId(),
        storeId(),
        { name: device.name, kind: device.kind, status },
        device.etag,
      );
      setPendingArchive(null);
      if (selected() === device.device_id && status === "archived") {
        setSelected("");
      }
      toast.ok(status === "archived" ? t("activation.deviceArchived") : t("activation.deviceRestored"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const columns = (): Column<Device>[] => [
    {
      key: "name",
      header: t("activation.deviceName"),
      sortValue: (row) => row.name,
      cell: (row) => (
        <Show when={editing() === row.device_id} fallback={<span>{row.name}</span>}>
          <div class="flex flex-wrap items-center gap-2">
            <input
              class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
              aria-label={t("activation.deviceName")}
              value={draftName()}
              onInput={(event) => setDraftName(event.currentTarget.value)}
            />
            <Button disabled={busy()} onClick={() => void saveRename(row)}>
              {t("action.save")}
            </Button>
            <Button
              variant="secondary"
              onClick={() => {
                setEditing("");
                setDraftName("");
              }}
            >
              {t("action.cancel")}
            </Button>
          </div>
        </Show>
      ),
    },
    {
      key: "kind",
      header: t("activation.deviceKind"),
      cell: (row) => (
        <select
          class="min-h-touch rounded-token border border-line bg-surface-raised px-2 text-sm text-ink disabled:opacity-60"
          aria-label={t("activation.deviceKind")}
          disabled={busy() || row.status === "archived"}
          value={row.kind}
          onChange={(event) => void setKind(row, event.currentTarget.value)}
        >
          {/* `kind` is free text on the wire, so a value this list does not offer still renders as
              itself rather than silently becoming the first option. */}
          <Show when={!KINDS.some((kind) => kind.wire === row.kind)}>
            <option value={row.kind}>{row.kind}</option>
          </Show>
          <For each={KINDS}>{(kind) => <option value={kind.wire}>{t(kind.key)}</option>}</For>
        </select>
      ),
    },
    {
      key: "status",
      header: t("activation.deviceStatus"),
      cell: (row) => (
        <StatusBadge
          tone={row.status === "archived" ? "archived" : "active"}
          label={row.status === "archived" ? t("status.archived") : t("status.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.device_id}</TechnicalDetails>
      ),
    },
  ];

  const issue = async () => {
    if (!selected()) {
      setError(t("activation.deviceRequired"));
      return;
    }
    setError("");
    setCode("");
    setBusy(true);
    try {
      const issued = await api.issueActivation(tenantId(), storeId(), selected());
      setCode(issued.activation_code);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("activation.title")} description={t("activation.description")} />
      <RequireContext need="store">
        <div class="flex flex-col gap-6">
          <Card
            title={t("activation.devices")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
            <div class="flex max-w-md flex-col gap-4">
              <Show
                when={devices()}
                fallback={<p class="text-sm text-ink-muted">{t("activation.loadHint")}</p>}
              >
                {(loaded) => (
                  <Show
                    when={loaded().length > 0}
                    fallback={<p class="text-sm text-ink-muted">{t("activation.noDevices")}</p>}
                  >
                    <label class="block">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("activation.deviceSelect")}
                      </span>
                      <select
                        class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                        value={selected()}
                        onChange={(event) => setSelected(event.currentTarget.value)}
                      >
                        <option value="">{t("activation.chooseDevice")}</option>
                        {/* An archived device is off the roster: it is still listed below, where it
                            can be restored, but it is not offered a fresh activation code. */}
                        <For each={loaded().filter((device) => device.status !== "archived")}>
                          {(device) => (
                            <option value={device.device_id}>
                              {device.name} · {device.kind}
                            </option>
                          )}
                        </For>
                      </select>
                    </label>
                  </Show>
                )}
              </Show>

              <Show when={code()}>
                {(value) => (
                  <div>
                    <Banner tone="ok" message={t("activation.codeOnce")} />
                    <code class="mt-2 block break-all rounded-token border border-line bg-surface-raised p-2 text-sm text-ink">
                      {value()}
                    </code>
                  </div>
                )}
              </Show>

              <Button disabled={busy() || !selected()} onClick={() => void issue()}>
                {t("action.issue")}
              </Button>
            </div>
          </Card>

          <Card title={t("activation.roster")}>
            <Show
              when={devices()}
              fallback={<p class="text-sm text-ink-muted">{t("activation.loadHint")}</p>}
            >
              {(loaded) => (
                <DataTable
                  columns={columns()}
                  rows={loaded()}
                  searchText={(row) => `${row.name} ${row.kind}`}
                  pageSize={12}
                  empty={<EmptyState title={t("activation.noDevices")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <div class="flex flex-wrap gap-2">
                      <Button
                        variant="secondary"
                        disabled={busy()}
                        onClick={() => {
                          setEditing(row.device_id);
                          setDraftName(row.name);
                        }}
                      >
                        {t("activation.rename")}
                      </Button>
                      <Show
                        when={row.status === "archived"}
                        fallback={
                          <Button
                            variant="danger"
                            disabled={busy()}
                            onClick={() => setPendingArchive(row)}
                          >
                            {t("activation.archive")}
                          </Button>
                        }
                      >
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => void setStatus(row, "active")}
                        >
                          {t("activation.restore")}
                        </Button>
                      </Show>
                    </div>
                  )}
                />
              )}
            </Show>
          </Card>

          <Card title={t("activation.createDevice")}>
            <div class="flex max-w-md flex-col gap-4">
              <TextField
                label={t("activation.deviceName")}
                placeholder={t("activation.deviceNamePlaceholder")}
                value={newName()}
                onInput={setNewName}
              />
              <label class="block">
                <span class="mb-1 block text-sm font-medium text-ink">
                  {t("activation.deviceKind")}
                </span>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  value={newKind()}
                  onChange={(event) => setNewKind(event.currentTarget.value)}
                >
                  <For each={KINDS}>
                    {(kind) => <option value={kind.wire}>{t(kind.key)}</option>}
                  </For>
                </select>
              </label>
              <Button disabled={busy()} onClick={() => void createDevice()}>
                {t("action.add")}
              </Button>
            </div>
          </Card>
        </div>

        <ConfirmDialog
          open={pendingArchive() !== null}
          title={t("activation.archiveTitle")}
          message={t("activation.archiveMessage")}
          confirmLabel={t("activation.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => {
            const device = pendingArchive();
            if (device) {
              void setStatus(device, "archived");
            }
          }}
          onCancel={() => setPendingArchive(null)}
        />
      </RequireContext>
    </div>
  );
}
