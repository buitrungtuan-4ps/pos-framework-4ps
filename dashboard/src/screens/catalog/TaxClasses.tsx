// The Tax classes sub-screen (ADR-0082, Track F3): the named tax buckets an item belongs to
// (ADR-0066 entity 10), on the F2 CRUD kit. Behaviour preserved from the monolith — create a class
// by name, rename it, archive/restore — rendered as a searchable `DataTable` with create and rename
// in a `Drawer` and the status shown through the shared `StatusCell`.

import { createSignal, Show } from "solid-js";

import { api } from "../../api/client";
import type { TaxClass } from "../../api/types";
import { t } from "../../i18n";
import { onScopedContext } from "../../lib/scoped";
import { tenantId } from "../../state/session";
import { Banner, Button, Card, TextField } from "../../components/ui";
import { type Column, DataTable, Drawer, EmptyState } from "../../components/kit";
import { toast } from "../../components/Toast";
import { errorMessage, StatusCell } from "./shared";

export function CatalogTaxClasses() {
  const [rows, setRows] = createSignal<TaxClass[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [creating, setCreating] = createSignal(false);
  const [newName, setNewName] = createSignal("");

  const [editing, setEditing] = createSignal<TaxClass | null>(null);
  const [draftName, setDraftName] = createSignal("");

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setRows(await api.listTaxClasses(tenantId()));
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const openCreate = () => {
    setNewName("");
    setCreating(true);
  };

  const createTaxClass = async () => {
    const name = newName().trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.createTaxClass(tenantId(), name);
      toast.ok(t("catalog.taxClassCreated"));
      setCreating(false);
      await load();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const applyFields = async (
    row: TaxClass,
    fields: { name?: string; status?: "active" | "archived" },
  ): Promise<boolean> => {
    const name = (fields.name ?? row.name).trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return false;
    }
    setBusy(true);
    try {
      await api.updateTaxClass(row.tax_class_id, tenantId(), {
        name,
        status: fields.status ?? row.status,
      });
      await load();
      return true;
    } catch (caught) {
      toast.error(errorMessage(caught));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const openEdit = (row: TaxClass) => {
    setEditing(row);
    setDraftName(row.name);
  };

  const saveEdit = async () => {
    const row = editing();
    if (!row) {
      return;
    }
    const ok = await applyFields(row, { name: draftName() });
    if (ok) {
      toast.ok(t("catalog.taxClassSaved"));
      setEditing(null);
    }
  };

  const toggleArchive = async (row: TaxClass) => {
    const archiving = row.status !== "archived";
    const ok = await applyFields(row, { status: archiving ? "archived" : "active" });
    if (ok) {
      toast.ok(archiving ? t("catalog.taxClassArchived") : t("catalog.taxClassRestored"));
    }
  };

  const columns = (): Column<TaxClass>[] => [
    {
      key: "name",
      header: t("catalog.name"),
      sortValue: (row) => row.name,
      cell: (row) => row.name,
    },
    {
      key: "status",
      header: t("catalog.status"),
      sortValue: (row) => row.status,
      cell: (row) => <StatusCell status={row.status} />,
    },
  ];

  return (
    <div class="flex flex-col gap-6">
      <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

      <Card
        title={t("catalog.taxClasses")}
        actions={
          <div class="flex flex-wrap gap-2">
            <Button disabled={busy()} onClick={openCreate}>
              {t("action.create")}
            </Button>
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          </div>
        }
      >
        <Show
          when={rows()}
          fallback={<p class="text-sm text-ink-muted">{t("catalog.loadHint")}</p>}
        >
          {(loaded) => (
            <DataTable
              columns={columns()}
              rows={loaded()}
              searchText={(row) => row.name}
              pageSize={12}
              empty={<EmptyState title={t("catalog.taxClassesEmpty")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <div class="flex flex-wrap gap-2">
                  <Button variant="secondary" disabled={busy()} onClick={() => openEdit(row)}>
                    {t("action.edit")}
                  </Button>
                  <Button
                    variant={row.status === "archived" ? "secondary" : "danger"}
                    disabled={busy()}
                    onClick={() => void toggleArchive(row)}
                  >
                    {row.status === "archived" ? t("catalog.restore") : t("catalog.archive")}
                  </Button>
                </div>
              )}
            />
          )}
        </Show>
      </Card>

      <Drawer
        open={creating()}
        title={t("catalog.taxClassName")}
        closeLabel={t("action.close")}
        onClose={() => setCreating(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreating(false)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void createTaxClass()}>
              {t("action.create")}
            </Button>
          </>
        }
      >
        <TextField
          label={t("catalog.taxClassName")}
          value={newName()}
          onInput={setNewName}
          placeholder={t("catalog.taxClassNamePlaceholder")}
        />
      </Drawer>

      <Drawer
        open={editing() !== null}
        title={editing()?.name ?? t("action.edit")}
        closeLabel={t("action.close")}
        onClose={() => setEditing(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEditing(null)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void saveEdit()}>
              {t("action.save")}
            </Button>
          </>
        }
      >
        <TextField label={t("catalog.name")} value={draftName()} onInput={setDraftName} />
      </Drawer>
    </div>
  );
}
