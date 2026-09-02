// The Modifiers sub-screen (ADR-0082, Track F3): modifier groups — a min/max selection rule with
// member modifiers, attached to items (ADR-0066 entities 4/5) — on the F2 CRUD kit. Behaviour is
// preserved from the monolith: a group is created with its rule, its member items, and the items it
// attaches to; afterwards the group can be renamed or archived/restored, and the rule/members re-ship
// untouched on those edits, exactly as the monolith's `setGroupFields` did. Rendered as a searchable
// `DataTable` (rule and counts as columns) with create and rename in a `Drawer`.

import { createSignal, For, Show } from "solid-js";

import { api } from "../../api/client";
import type { CatalogItem, ModifierGroup } from "../../api/types";
import { t } from "../../i18n";
import { onScopedContext } from "../../lib/scoped";
import { tenantId } from "../../state/session";
import { Banner, Button, Card, TextField } from "../../components/ui";
import { type Column, DataTable, Drawer, EmptyState, FormField } from "../../components/kit";
import { toast } from "../../components/Toast";
import { errorMessage, isStale, StatusCell } from "./shared";

export function CatalogModifiers() {
  const [groups, setGroups] = createSignal<ModifierGroup[] | null>(null);
  const [items, setItems] = createSignal<CatalogItem[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // Create drawer — a group's full shape is authored here.
  const [creating, setCreating] = createSignal(false);
  const [newName, setNewName] = createSignal("");
  const [newMin, setNewMin] = createSignal("0");
  const [newMax, setNewMax] = createSignal("1");
  const [newMembers, setNewMembers] = createSignal<string[]>([]);
  const [newAttached, setNewAttached] = createSignal<string[]>([]);

  // Edit drawer — rename only; the rule, members and attachments re-ship unchanged (monolith parity).
  const [editing, setEditing] = createSignal<ModifierGroup | null>(null);
  const [draftName, setDraftName] = createSignal("");

  const selectedValues = (select: HTMLSelectElement): string[] =>
    Array.from(select.selectedOptions, (option) => option.value);

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedGroups, loadedItems] = await Promise.all([
        api.listModifierGroups(tenantId()),
        api.listItems(tenantId()),
      ]);
      setGroups(loadedGroups);
      setItems(loadedItems);
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
    setNewMin("0");
    setNewMax("1");
    setNewMembers([]);
    setNewAttached([]);
    setCreating(true);
  };

  const createGroup = async () => {
    const name = newName().trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return;
    }
    const min = Number(newMin().trim() || "0");
    const max = Number(newMax().trim() || "0");
    if (!Number.isInteger(min) || min < 0 || !Number.isInteger(max) || max < min) {
      toast.error(t("catalog.groupRuleInvalid"));
      return;
    }
    setBusy(true);
    try {
      await api.createModifierGroup(tenantId(), {
        name,
        minSelect: min,
        maxSelect: max,
        memberItemIds: newMembers(),
        attachedItemIds: newAttached(),
      });
      toast.ok(t("catalog.modifierGroupCreated"));
      setCreating(false);
      await load();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const applyGroup = async (
    group: ModifierGroup,
    fields: { name?: string; status?: "active" | "archived" },
  ): Promise<boolean> => {
    const name = (fields.name ?? group.name).trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return false;
    }
    setBusy(true);
    try {
      await api.updateModifierGroup(group.modifier_group_id, tenantId(), {
        name,
        minSelect: group.min_select,
        maxSelect: group.max_select,
        memberItemIds: group.member_item_ids,
        attachedItemIds: group.attached_item_ids,
        status: fields.status ?? group.status,
      }, group.etag);
      await load();
      return true;
    } catch (caught) {
      toast.error(errorMessage(caught));
      // A stale copy is recovered by reloading, so the reader sees what actually changed.
      if (isStale(caught)) {
        await load();
      }
      return false;
    } finally {
      setBusy(false);
    }
  };

  const openEdit = (group: ModifierGroup) => {
    setEditing(group);
    setDraftName(group.name);
  };

  const saveEdit = async () => {
    const group = editing();
    if (!group) {
      return;
    }
    const ok = await applyGroup(group, { name: draftName() });
    if (ok) {
      toast.ok(t("catalog.modifierGroupSaved"));
      setEditing(null);
    }
  };

  const toggleArchive = async (group: ModifierGroup) => {
    const archiving = group.status !== "archived";
    const ok = await applyGroup(group, { status: archiving ? "archived" : "active" });
    if (ok) {
      toast.ok(archiving ? t("catalog.modifierGroupArchived") : t("catalog.modifierGroupRestored"));
    }
  };

  const columns = (): Column<ModifierGroup>[] => [
    {
      key: "name",
      header: t("catalog.name"),
      sortValue: (row) => row.name,
      cell: (row) => row.name,
    },
    {
      key: "rule",
      header: t("catalog.groupRule"),
      cell: (row) => `${row.min_select}–${row.max_select}`,
    },
    {
      key: "members",
      header: t("catalog.groupMembers"),
      sortValue: (row) => row.member_item_ids.length,
      cell: (row) => row.member_item_ids.length,
    },
    {
      key: "attached",
      header: t("catalog.groupAttached"),
      sortValue: (row) => row.attached_item_ids.length,
      cell: (row) => row.attached_item_ids.length,
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
        title={t("catalog.modifierGroups")}
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
        <p class="mb-3 text-sm text-ink-muted">{t("catalog.modifierGroupsHint")}</p>
        <Show
          when={groups()}
          fallback={<p class="text-sm text-ink-muted">{t("catalog.loadHint")}</p>}
        >
          {(loaded) => (
            <DataTable
              columns={columns()}
              rows={loaded()}
              searchText={(row) => row.name}
              pageSize={12}
              empty={<EmptyState title={t("catalog.modifierGroupsEmpty")} />}
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
        title={t("catalog.createGroup")}
        closeLabel={t("action.close")}
        onClose={() => setCreating(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreating(false)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void createGroup()}>
              {t("action.create")}
            </Button>
          </>
        }
      >
        <div class="flex flex-col gap-4">
          <TextField
            label={t("catalog.name")}
            value={newName()}
            onInput={setNewName}
            placeholder={t("catalog.groupNamePlaceholder")}
          />
          <div class="grid grid-cols-2 gap-4">
            <FormField label={t("catalog.groupMin")}>
              <input
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                inputmode="numeric"
                aria-label={t("catalog.groupMin")}
                value={newMin()}
                onInput={(event) => setNewMin(event.currentTarget.value)}
              />
            </FormField>
            <FormField label={t("catalog.groupMax")}>
              <input
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                inputmode="numeric"
                aria-label={t("catalog.groupMax")}
                value={newMax()}
                onInput={(event) => setNewMax(event.currentTarget.value)}
              />
            </FormField>
          </div>
          <FormField label={t("catalog.groupMembers")}>
            <select
              multiple
              class="w-full rounded-token border border-line bg-surface-raised p-2 text-sm text-ink"
              size="6"
              onChange={(event) => setNewMembers(selectedValues(event.currentTarget))}
            >
              <For each={items()}>
                {(item) => (
                  <option value={item.menu_item_id} selected={newMembers().includes(item.menu_item_id)}>
                    {item.name}
                  </option>
                )}
              </For>
            </select>
          </FormField>
          <FormField label={t("catalog.groupAttached")}>
            <select
              multiple
              class="w-full rounded-token border border-line bg-surface-raised p-2 text-sm text-ink"
              size="6"
              onChange={(event) => setNewAttached(selectedValues(event.currentTarget))}
            >
              <For each={items()}>
                {(item) => (
                  <option
                    value={item.menu_item_id}
                    selected={newAttached().includes(item.menu_item_id)}
                  >
                    {item.name}
                  </option>
                )}
              </For>
            </select>
          </FormField>
        </div>
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
