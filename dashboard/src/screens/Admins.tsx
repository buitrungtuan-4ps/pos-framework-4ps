// Console admins & invitations (ADR-0067, Track G1), on the authoring kit
// ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)). An owner or admin views the
// roster and invites new admins by email (the single-use token is shown once, for the inviter to
// hand over out-of-band — never an admin-set password). An owner additionally changes roles and
// suspends/reactivates admins; the server enforces the same gate, so a non-owner simply sees the
// roster read-only. This is console-level identity, so it carries no tenant/store context.
//
// The role used to be a `<select>` in the table cell that wrote on `change`. On this screen that
// is not a cosmetic issue: one mis-click demoted an owner to a viewer, with no confirmation and
// no undo, and the only way back was another owner. It is an explicit form now — pick, then
// confirm — which is the same correction ADR-0121 §6 made to the Stores brand column, for a
// worse consequence.

import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import type { AdminIdentity, AdminInvite, AdminRole } from "../api/types";
import { ADMIN_ROLES } from "../api/types";
import { type MessageKey, t } from "../i18n";
import { actingAdmin } from "../state/session";
import {
  Banner,
  Button,
  Card,
  PageHeader,
  SelectField,
  Skeleton,
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
import { apiMessage } from "../lib/errors";

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
  const [loading, setLoading] = createSignal(false);

  // Four lifecycles, so revoking an invitation does not disable the roster and a role change does
  // not disable the invite form (ADR-0121 §3).
  const invitation = useEntityCrud<never>();
  const roleChange = useEntityCrud<AdminIdentity>();
  const suspension = useEntityCrud<AdminIdentity>();
  const revocation = useEntityCrud<AdminInvite>();
  // Which admin is being reactivated, so one slow write disables that row's button only.
  const [reactivating, setReactivating] = createSignal("");

  const [inviteEmail, setInviteEmail] = createSignal("");
  const [inviteName, setInviteName] = createSignal("");
  const [inviteRole, setInviteRole] = createSignal<AdminRole>("viewer");
  // The invite link, held only until the operator dismisses it. Nothing persists it: the cloud
  // stored a hash of the token and cannot show this again.
  const [inviteLink, setInviteLink] = createSignal("");
  const [draftRole, setDraftRole] = createSignal<AdminRole>("viewer");

  // Only an owner may change roles or suspend/reactivate. An admin sees the roster read-only, and no
  // one edits their own row here (self-service lives on My security / My sessions), which also keeps
  // an owner from accidentally locking themselves out.
  const canManage = () => actingAdmin()?.role === "owner";
  const isSelf = (row: AdminIdentity) => row.id === actingAdmin()?.id;
  // Only an owner may mint another owner, so offer that role in the invite picker to owners alone.
  const invitableRoles = (): readonly AdminRole[] =>
    canManage() ? ADMIN_ROLES : ADMIN_ROLES.filter((role) => role !== "owner");

  const fail = (caught: unknown) => {
    const message = apiMessage(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setLoading(true);
    try {
      const [roster, pending] = await Promise.all([api.listAdmins(), api.listInvites()]);
      setAdmins(roster);
      setInvites(pending);
    } catch (caught) {
      fail(caught);
    } finally {
      setLoading(false);
    }
  };

  // Console-level, so no scoped auto-load — just fetch once on open.
  void load();

  const openInvite = () => {
    setInviteEmail("");
    setInviteName("");
    setInviteRole("viewer");
    invitation.create();
  };

  const invite = () => {
    const email = inviteEmail().trim();
    const name = inviteName().trim();
    if (!email || !name) {
      invitation.refuse(t("admins.inviteRequired"));
      return;
    }
    void invitation
      .run(async () => {
        const created = await api.inviteAdmin(email, name, inviteRole());
        // Set inside the write, so the link exists before `run` closes the form and the second
        // dialog can open in the same tick. It is the only copy that will ever be shown.
        setInviteLink(
          `${window.location.origin}/invite?token=${encodeURIComponent(created.token)}`,
        );
      })
      .then((invited) => {
        if (invited) {
          toast.ok(t("admins.invited"));
          void load();
        }
      });
  };

  const openRoleChange = (row: AdminIdentity) => {
    setDraftRole(row.role);
    roleChange.edit(row);
  };

  const changeRole = () => {
    const row = roleChange.subject();
    if (!row) {
      return;
    }
    // Confirming without changing the pick is a no-op rather than a redundant write: it is the
    // ordinary way out of a form somebody opened to check what the role currently was.
    if (draftRole() === row.role) {
      roleChange.close();
      return;
    }
    void roleChange.run(() => api.setAdminRole(row.id, draftRole())).then((changed) => {
      if (changed) {
        toast.ok(t("admins.roleChanged"));
        void load();
      }
    });
  };

  const suspend = () => {
    const row = suspension.subject();
    if (!row) {
      return;
    }
    void suspension.run(() => api.setAdminStatus(row.id, "suspended")).then((suspended) => {
      if (suspended) {
        toast.ok(t("admins.suspended"));
        void load();
      } else {
        // The confirm stays open carrying the refusal; a page banner would say it twice.
        toast.error(suspension.error());
      }
    });
  };

  const reactivate = async (row: AdminIdentity) => {
    setError("");
    setReactivating(row.id);
    try {
      await api.setAdminStatus(row.id, "active");
      toast.ok(t("admins.reactivated"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setReactivating("");
    }
  };

  const revoke = () => {
    const target = revocation.subject();
    if (!target) {
      return;
    }
    void revocation.run(() => api.revokeInvite(target.id)).then((revoked) => {
      if (revoked) {
        toast.ok(t("admins.inviteRevoked"));
        void load();
      } else {
        toast.error(revocation.error());
      }
    });
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
      cell: (row) => <span class="text-ink">{t(ROLE_LABEL[row.role])}</span>,
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
            <div class="flex gap-2">
              <Button onClick={openInvite}>{t("admins.invite")}</Button>
              <Button variant="secondary" disabled={loading()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            </div>
          }
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={admins()} fallback={<Skeleton label={t("common.loading")} rows={4} />}>
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
                    <div class="flex flex-wrap gap-2">
                      <Button variant="secondary" onClick={() => openRoleChange(row)}>
                        {t("admins.changeRole")}
                      </Button>
                      <Show
                        when={row.status === "suspended"}
                        fallback={
                          <Button variant="danger" onClick={() => suspension.confirm(row)}>
                            {t("admins.suspend")}
                          </Button>
                        }
                      >
                        <Button
                          variant="secondary"
                          disabled={reactivating() === row.id}
                          onClick={() => void reactivate(row)}
                        >
                          {t("admins.reactivate")}
                        </Button>
                      </Show>
                    </div>
                  </Show>
                )}
              />
            )}
          </Show>
        </Card>

        <div>
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
                        <Button variant="secondary" onClick={() => revocation.confirm(row)}>
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

      <FormPanel
        crud={invitation}
        createTitle={t("admins.invite")}
        editTitle={t("admins.invite")}
        submitLabel={t("admins.sendInvite")}
        onSubmit={invite}
        dirty={() => inviteEmail() !== "" || inviteName() !== ""}
      >
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
        <SelectField
          label={t("admins.role")}
          value={inviteRole()}
          options={invitableRoles().map((role) => ({ value: role, label: t(ROLE_LABEL[role]) }))}
          onChange={(value) => setInviteRole(value as AdminRole)}
        />
      </FormPanel>

      <FormPanel
        crud={roleChange}
        createTitle={t("admins.roleTitle")}
        editTitle={t("admins.roleTitle")}
        submitLabel={t("action.save")}
        onSubmit={changeRole}
        as="modal"
      >
        <Show when={roleChange.subject()}>
          {(target) => (
            <p class="text-sm text-ink-muted">
              {target().name} · {target().email}
            </p>
          )}
        </Show>
        <SelectField
          label={t("admins.role")}
          value={draftRole()}
          options={ADMIN_ROLES.map((role) => ({ value: role, label: t(ROLE_LABEL[role]) }))}
          onChange={(value) => setDraftRole(value as AdminRole)}
          hint={t("admins.roleHint")}
        />
      </FormPanel>

      {/* The invite link, once. A separate dialog from the form that made it, because it hands back
          something the console cannot recover: the cloud stored a hash of the token. */}
      <Modal
        open={inviteLink() !== ""}
        title={t("admins.inviteLinkTitle")}
        closeLabel={t("action.close")}
        onClose={() => setInviteLink("")}
        footer={<Button onClick={() => setInviteLink("")}>{t("action.close")}</Button>}
      >
        <Banner tone="ok" message={t("admins.inviteLinkOnce")} />
        <code class="mt-3 block break-all rounded-token border border-line bg-surface-raised p-2 text-xs text-ink">
          {inviteLink()}
        </code>
      </Modal>

      <ConfirmDialog
        open={revocation.mode() === "confirming"}
        title={t("admins.revokeTitle")}
        message={t("admins.revokeMessage")}
        confirmLabel={t("action.revoke")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        danger
        busy={revocation.saving()}
        onConfirm={revoke}
        onCancel={revocation.close}
      />
      <ConfirmDialog
        open={suspension.mode() === "confirming"}
        title={t("admins.suspendTitle")}
        message={t("admins.suspendMessage")}
        confirmLabel={t("admins.suspend")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        danger
        busy={suspension.saving()}
        onConfirm={suspend}
        onCancel={suspension.close}
      />
    </div>
  );
}
