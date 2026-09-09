// Device activation codes (ADR-0050, ADR-0065), on the authoring kit
// ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)): issue a one-time code a store
// device exchanges once for its credentials. The device is chosen (or created) by **name** from the
// registry — no more typing a raw device-slot ULID. Names the tenant + store in context.
//
// Two things this screen used to do in the table and no longer does. Renaming happened in the row,
// through a raw `<input>` that appeared where the name had been; and the kind was a `<select>` in
// the cell that wrote on `change`, so one mis-click turned a printer into a kitchen display with no
// confirmation and no undo — the same hazard ADR-0121 §6 removed from the Stores brand column. Both
// are fields of the edit form now, saved together, deliberately.
//
// The issued code is shown **exactly once** — the cloud stores a hash of it — so it lands in its own
// `Modal`, as the API-key token and the webhook signing secret do. Same reasoning: a form asks for
// input, this hands back something unrecoverable.

import { createSignal, Show } from "solid-js";

import { api } from "../api/client";
import type { Device } from "../api/types";
import { type MessageKey, t } from "../i18n";
import { apiMessage, withStaleReload } from "../lib/errors";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  PageHeader,
  SelectField,
  StatusBadge,
  TextField,
} from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  FormPanel,
  Modal,
  TechnicalDetails,
} from "../components/kit";
import { useEntityCrud } from "../lib/entity-crud";
import { toast } from "../components/Toast";

// The device kinds offered when adding one, each mapped to a static i18n key. `kind` is free text on
// the wire, so this is a convenience list, not a closed set.
const KINDS: readonly { wire: string; key: MessageKey }[] = [
  { wire: "pos", key: "device.kind.pos" },
  { wire: "printer", key: "device.kind.printer" },
  { wire: "kds", key: "device.kind.kds" },
  { wire: "tablet", key: "device.kind.tablet" },
];

/** A kind's label, or the raw wire value for one this list does not offer. */
const kindLabel = (wire: string) => {
  const known = KINDS.find((kind) => kind.wire === wire);
  return known ? t(known.key) : wire;
};

export function Activation() {
  const [devices, setDevices] = createSignal<Device[] | null>(null);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);

  // Four lifecycles: the device form, the archive confirm, the code issue, and the restore. Separate
  // so that issuing a code does not disable the roster's buttons (ADR-0121 §3).
  const editor = useEntityCrud<Device>();
  const archival = useEntityCrud<Device>();
  const issuing = useEntityCrud<never>();
  // Which device is being restored, so one slow restore disables that row's button only.
  const [restoring, setRestoring] = createSignal("");

  // The device form's fields.
  const [name, setName] = createSignal("");
  const [kind, setKind] = createSignal("pos");
  // The issue form's field, and the code it produced. Nothing persists the code, here or anywhere.
  const [chosen, setChosen] = createSignal("");
  const [code, setCode] = createSignal("");

  const load = async () => {
    setError("");
    setLoading(true);
    try {
      setDevices(await api.listDevices(tenantId(), storeId()));
    } catch (caught) {
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    } finally {
      setLoading(false);
    }
  };

  // Load on open and whenever the tenant/store changes — never with an empty context (F0).
  onScopedContext("store", () => void load());

  // Every edit here is conditional on the version the row was read at (ADR-0094), so a refusal owes
  // the reader a reload and a sentence of its own; `withStaleReload` carries both.
  const conditional = <T,>(write: () => Promise<T>) =>
    withStaleReload(write, load, t("activation.stale"));

  const openCreate = () => {
    setName("");
    setKind("pos");
    editor.create();
  };

  const openEdit = (device: Device) => {
    setName(device.name);
    setKind(device.kind);
    editor.edit(device);
  };

  const save = () => {
    const trimmed = name().trim();
    if (!trimmed) {
      editor.refuse(t("activation.deviceNameRequired"));
      return;
    }
    const target = editor.subject();
    void editor
      .run(() =>
        conditional(async () => {
          if (target) {
            await api.updateDevice(
              target.device_id,
              tenantId(),
              storeId(),
              { name: trimmed, kind: kind(), status: target.status },
              target.etag,
            );
            return;
          }
          const created = await api.createDevice(tenantId(), storeId(), trimmed, kind());
          // A device added here is almost always the one about to be activated, so it becomes the
          // issue form's pick rather than leaving the operator to find it in the list again.
          setChosen(created.device_id);
        }),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(target ? t("activation.deviceRenamed") : t("activation.deviceAdded"));
          void load();
        }
      });
  };

  /**
   * Archiving takes a device off the roster: it stops being offered a code, and stops being counted
   * as a device this store runs. It does **not** retire a paired till — that is the store server's
   * own `POST /api/pair/revoke` (a different credential in a different tier), reachable from the
   * till's own Devices screen.
   */
  const archive = () => {
    const device = archival.subject();
    if (!device) {
      return;
    }
    void archival
      .run(() =>
        conditional(() =>
          api.updateDevice(
            device.device_id,
            tenantId(),
            storeId(),
            { name: device.name, kind: device.kind, status: "archived" },
            device.etag,
          ),
        ),
      )
      .then((archived) => {
        if (archived) {
          // An archived device is no longer offered a code, so it must not stay the chosen one.
          if (chosen() === device.device_id) {
            setChosen("");
          }
          toast.ok(t("activation.deviceArchived"));
          void load();
        } else {
          // The confirm stays open carrying the refusal; a page banner would say it twice.
          toast.error(archival.error());
        }
      });
  };

  const restore = async (device: Device) => {
    setError("");
    setRestoring(device.device_id);
    try {
      await conditional(() =>
        api.updateDevice(
          device.device_id,
          tenantId(),
          storeId(),
          { name: device.name, kind: device.kind, status: "active" },
          device.etag,
        ),
      );
      toast.ok(t("activation.deviceRestored"));
      await load();
    } catch (caught) {
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    } finally {
      setRestoring("");
    }
  };

  const activatable = () => (devices() ?? []).filter((device) => device.status !== "archived");

  const openIssue = () => {
    setChosen(chosen() || activatable()[0]?.device_id || "");
    issuing.create();
  };

  const issue = () => {
    if (!chosen()) {
      issuing.refuse(t("activation.deviceRequired"));
      return;
    }
    void issuing.run(async () => {
      const issued = await api.issueActivation(tenantId(), storeId(), chosen());
      // Set inside the write, so the code exists before `run` closes the form and the second dialog
      // can open in the same tick. It is the only copy that will ever be shown.
      setCode(issued.activation_code);
    });
  };

  const columns = (): Column<Device>[] => [
    {
      key: "name",
      header: t("activation.deviceName"),
      sortValue: (row) => row.name,
      cell: (row) => <span class="text-ink">{row.name}</span>,
    },
    {
      key: "kind",
      header: t("activation.deviceKind"),
      cell: (row) => <span class="text-ink-muted">{kindLabel(row.kind)}</span>,
      sortValue: (row) => kindLabel(row.kind),
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
      sortValue: (row) => row.status,
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.device_id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("activation.title")} description={t("activation.description")} />
      <RequireContext need="store">
        <Card
          title={t("activation.roster")}
          actions={
            <div class="flex flex-wrap gap-2">
              <Button onClick={openCreate}>{t("activation.createDevice")}</Button>
              <Button
                variant="secondary"
                disabled={activatable().length === 0}
                onClick={openIssue}
              >
                {t("action.issue")}
              </Button>
              <Button variant="secondary" disabled={loading()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            </div>
          }
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
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
                    <Button variant="secondary" onClick={() => openEdit(row)}>
                      {t("action.edit")}
                    </Button>
                    <Show
                      when={row.status === "archived"}
                      fallback={
                        <Button variant="danger" onClick={() => archival.confirm(row)}>
                          {t("activation.archive")}
                        </Button>
                      }
                    >
                      <Button
                        variant="secondary"
                        disabled={restoring() === row.device_id}
                        onClick={() => void restore(row)}
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

        <FormPanel
          crud={editor}
          createTitle={t("activation.createDevice")}
          editTitle={t("activation.editDevice")}
          submitLabel={t("action.save")}
          onSubmit={save}
          dirty={() => name() !== "" && editor.mode() === "creating"}
        >
          <TextField
            label={t("activation.deviceName")}
            placeholder={t("activation.deviceNamePlaceholder")}
            value={name()}
            onInput={setName}
          />
          <SelectField
            label={t("activation.deviceKind")}
            value={kind()}
            // A `kind` the list does not offer still appears as itself rather than silently becoming
            // the first option — it is free text on the wire.
            options={[
              ...(KINDS.some((entry) => entry.wire === kind())
                ? []
                : [{ value: kind(), label: kind() }]),
              ...KINDS.map((entry) => ({ value: entry.wire, label: t(entry.key) })),
            ]}
            onChange={setKind}
          />
        </FormPanel>

        <FormPanel
          crud={issuing}
          createTitle={t("activation.issueTitle")}
          editTitle={t("activation.issueTitle")}
          submitLabel={t("action.issue")}
          onSubmit={issue}
          as="modal"
        >
          {/* An archived device is off the roster: it is still in the table, where it can be
              restored, but it is not offered a fresh activation code. */}
          <SelectField
            label={t("activation.deviceSelect")}
            value={chosen()}
            options={activatable().map((device) => ({
              value: device.device_id,
              label: `${device.name} · ${kindLabel(device.kind)}`,
            }))}
            onChange={setChosen}
            placeholder={t("activation.chooseDevice")}
          />
        </FormPanel>

        {/* The code, once. Dismissing this drops the only copy the console will ever hold. */}
        <Modal
          open={code() !== ""}
          title={t("activation.codeTitle")}
          closeLabel={t("action.close")}
          onClose={() => setCode("")}
          footer={<Button onClick={() => setCode("")}>{t("action.close")}</Button>}
        >
          <Banner tone="ok" message={t("activation.codeOnce")} />
          <code class="mt-3 block break-all rounded-token border border-line bg-surface-raised p-2 text-sm text-ink">
            {code()}
          </code>
        </Modal>

        <ConfirmDialog
          open={archival.mode() === "confirming"}
          title={t("activation.archiveTitle")}
          message={t("activation.archiveMessage")}
          confirmLabel={t("activation.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={archival.saving()}
          onConfirm={archive}
          onCancel={archival.close}
        />
      </RequireContext>
    </div>
  );
}
