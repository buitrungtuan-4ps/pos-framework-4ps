// Console admins & invitations (ADR-0067, Track G1), on the F2 CRUD kit. An owner or admin views the
// roster and invites new admins by email (the single-use token is shown once, for the inviter to
// hand over out-of-band — never an admin-set password). An owner additionally changes roles and
// suspends/reactivates admins; the server enforces the same gate, so a non-owner simply sees the
// roster read-only. This is console-level identity, so it carries no tenant/store context.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { AdminIdentity, AdminInvite, AdminRole } from "../api/types";
import { ADMIN_ROLES } from "../api/types";
import { type MessageKey, t } from "../i18n";
import { actingAdmin } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  FormField,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

const ROLE_LABEL: Record<AdminRole, MessageKey> = {
  owner: "role.owner",
  admin: "role.admin",
  ops: "role.ops",
  viewer: "role.viewer",
};

export function Admins() {
  const [admins, setAdmins] = createSignal<AdminIdentity[] | null>(null);
  const [invites, setInvites] = createSignal<AdminInvite[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [inviteEmail, setInviteEmail] = createSignal("");
  const [inviteName, setInviteName] = createSignal("");
  const [inviteRole, setInviteRole] = createSignal<AdminRole>("viewer");
  const [inviteLink, setInviteLink] = createSignal("");

  const [pendingRevoke, setPendingRevoke] = createSignal<AdminInvite | null>(null);
  const [pendingSuspend, setPendingSuspend] = createSignal<AdminIdentity | null>(null);

  // Only an owner may change roles or suspend/reactivate. An admin sees the roster read-only, and no
  // one edits their own row here (self-service lives on My security / My sessions), which also keeps
  // an owner from accidentally locking themselves out.
  const canManage = () => actingAdmin()?.role === "owner";
  const isSelf = (row: AdminIdentity) => row.id === actingAdmin()?.id;
  // Only an owner may mint another owner, so offer that role in the invite picker to owners alone.
  const invitableRoles = (): readonly AdminRole[] =>
    canManage() ? ADMIN_ROLES : ADMIN_ROLES.filter((role) => role !== "owner");

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [roster, pending] = await Promise.all([api.listAdmins(), api.listInvites()]);
      setAdmins(roster);
      setInvites(pending);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Console-level, so no scoped auto-load — just fetch once on open.
  void load();

  const invite = async () => {
    const email = inviteEmail().trim();
    const name = inviteName().trim();
    if (!email || !name) {
      setError(t("admins.inviteRequired"));
      return;
    }
    setError("");
    setInviteLink("");
    setBusy(true);
    try {
      const created = await api.inviteAdmin(email, name, inviteRole());
      setInviteLink(`${window.location.origin}/invite?token=${encodeURIComponent(created.token)}`);
      setInviteEmail("");
      setInviteName("");
      setInviteRole("viewer");
      toast.ok(t("admins.invited"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const changeRole = async (row: AdminIdentity, role: AdminRole) => {
    if (role === row.role) {
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.setAdminRole(row.id, role);
      toast.ok(t("admins.roleChanged"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setStatus = async (row: AdminIdentity, suspend: boolean) => {
    setError("");
    setBusy(true);
    try {
      await api.setAdminStatus(row.id, suspend ? "suspended" : "active");
      setPendingSuspend(null);
      toast.ok(suspend ? t("admins.suspended") : t("admins.reactivated"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const revoke = async () => {
    const target = pendingRevoke();
    if (!target) {
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.revokeInvite(target.id);
      setPendingRevoke(null);
      toast.ok(t("admins.inviteRevoked"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const columns = (): Column<AdminIdentity>[] => [
    {
      key: "name",
      header: t("admins.name"),
      sortValue: (row) => row.name,
      cell: (row) => <span>{row.name}</span>,
    },
    {
      key: "email",
      header: t("admins.email"),
      sortValue: (row) => row.email,
      cell: (row) => <span class="text-ink-muted">{row.email}</span>,
    },
    {
      key: "role",
      header: t("admins.role"),
      sortValue: (row) => row.role,
      cell: (row) => (
        <Show
          when={canManage() && !isSelf(row)}
          fallback={<span>{t(ROLE_LABEL[row.role])}</span>}
        >
          <select
            class="min-h-touch rounded-token border border-line bg-surface-raised px-2 text-sm text-ink disabled:opacity-60"
            aria-label={t("admins.role")}
            disabled={busy()}
            value={row.role}
            onChange={(event) => void changeRole(row, event.currentTarget.value as AdminRole)}
          >
            <For each={ADMIN_ROLES}>
              {(role) => <option value={role}>{t(ROLE_LABEL[role])}</option>}
            </For>
          </select>
        </Show>
      ),
    },
    {
      key: "status",
      header: t("admins.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.status === "suspended" ? "disabled" : "active"}
          label={row.status === "suspended" ? t("status.suspended") : t("status.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("admins.title")} description={t("admins.description")} />
      <div class="flex flex-col gap-6">
        <Card
          title={t("admins.list")}
          actions={
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          }
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={admins()} fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}>
            {(loaded) => (
              <DataTable
                columns={columns()}
                rows={loaded()}
                searchText={(row) => `${row.name} ${row.email}`}
                pageSize={12}
                empty={<EmptyState title={t("admins.empty")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <Show when={canManage() && !isSelf(row)}>
                    <Show
                      when={row.status === "suspended"}
                      fallback={
                        <Button
                          variant="danger"
                          disabled={busy()}
                          onClick={() => setPendingSuspend(row)}
                        >
                          {t("admins.suspend")}
                        </Button>
                      }
                    >
                      <Button
                        variant="secondary"
                        disabled={busy()}
                        onClick={() => void setStatus(row, false)}
                      >
                        {t("admins.reactivate")}
                      </Button>
                    </Show>
                  </Show>
                )}
              />
            )}
          </Show>
        </Card>

        <div class="grid gap-6 lg:grid-cols-2">
          <Card title={t("admins.invite")}>
            <div class="flex flex-col gap-4">
              <TextField
                label={t("admins.email")}
                type="email"
                value={inviteEmail()}
                onInput={setInviteEmail}
                placeholder={t("admins.emailPlaceholder")}
              />
              <TextField
                label={t("admins.name")}
                value={inviteName()}
                onInput={setInviteName}
                placeholder={t("admins.namePlaceholder")}
              />
              <FormField label={t("admins.role")}>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  value={inviteRole()}
                  onChange={(event) => setInviteRole(event.currentTarget.value as AdminRole)}
                >
                  <For each={invitableRoles()}>
                    {(role) => <option value={role}>{t(ROLE_LABEL[role])}</option>}
                  </For>
                </select>
              </FormField>
              <Show when={inviteLink()}>
                {(link) => (
                  <div>
                    <Banner tone="ok" message={t("admins.inviteLinkOnce")} />
                    <code class="mt-2 block break-all rounded-token border border-line bg-surface-raised p-2 text-xs text-ink">
                      {link()}
                    </code>
                  </div>
                )}
              </Show>
              <Button disabled={busy()} onClick={() => void invite()}>
                {t("admins.sendInvite")}
              </Button>
            </div>
          </Card>

          <Card title={t("admins.pending")}>
            <Show
              when={invites().length > 0}
              fallback={<EmptyState title={t("admins.noPending")} />}
            >
              <ul class="flex flex-col gap-2">
                <For each={invites()}>
                  {(row) => (
                    <li class="flex flex-wrap items-center justify-between gap-2 rounded-token border border-line bg-surface-raised px-3 py-2">
                      <div class="flex flex-col">
                        <span class="text-sm text-ink">{row.name}</span>
                        <span class="text-xs text-ink-muted">{row.email}</span>
                      </div>
                      <div class="flex items-center gap-2">
                        <StatusBadge tone="neutral" label={t(ROLE_LABEL[row.role])} />
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => setPendingRevoke(row)}
                        >
                          {t("action.revoke")}
                        </Button>
                      </div>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </Card>
        </div>
      </div>

      <ConfirmDialog
        open={pendingRevoke() !== null}
        title={t("admins.revokeTitle")}
        message={t("admins.revokeMessage")}
        confirmLabel={t("action.revoke")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        danger
        busy={busy()}
        onConfirm={() => void revoke()}
        onCancel={() => setPendingRevoke(null)}
      />
      <ConfirmDialog
        open={pendingSuspend() !== null}
        title={t("admins.suspendTitle")}
        message={t("admins.suspendMessage")}
        confirmLabel={t("admins.suspend")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        danger
        busy={busy()}
        onConfirm={() => {
          const target = pendingSuspend();
          if (target) {
            void setStatus(target, true);
          }
        }}
        onCancel={() => setPendingSuspend(null)}
      />
    </div>
  );
}
