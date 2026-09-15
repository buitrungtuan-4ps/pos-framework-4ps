// My sessions (ADR-0067, Track G1): every admin lists their own live console sessions and revokes
// them. Self-service — available to any authenticated admin regardless of role — so it carries no
// tenant/store context. The session making this request is marked and protected from accidental
// self-revocation; "sign out everywhere else" keeps only the current one. The handle shown is the
// server's opaque revocation id (a hash, never the token, and not reversible to it).

import { createSignal, Show } from "solid-js";

import { api } from "../api/client";
import type { AdminSessionView } from "../api/types";
import { locale, t } from "../i18n";
import { Banner, Button, Card, PageHeader, Skeleton, StatusBadge } from "../components/ui";
import {
  type Column,
  CLIENT_PAGE_SIZE,
  ConfirmDialog,
  DataTable,
  EmptyState,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";
import { apiMessage } from "../lib/errors";
import { createAdminResource, failureOf } from "../lib/resource";

// A Unix-ms instant as a locale-aware date-time; an unparseable value falls back to its raw number
// rather than throwing, so a malformed row never blanks the table.
function formatInstant(ms: number): string {
  try {
    return new Intl.DateTimeFormat(locale(), { dateStyle: "medium", timeStyle: "short" }).format(
      new Date(ms),
    );
  } catch {
    return String(ms);
  }
}

export function MySessions() {
  const [revoking, setRevoking] = createSignal(false);
  const [pendingRevoke, setPendingRevoke] = createSignal<AdminSessionView | null>(null);
  const [pendingOthers, setPendingOthers] = createSignal(false);

  // No scope: an admin's own sessions belong to the admin, not to a tenant or a store. Revalidated
  // on focus because a session revoked from another browser should stop being listed here without
  // the operator having to ask.
  const sessions = createAdminResource(() => api.listSessions(), { revalidateOnFocus: true });

  const revoke = async () => {
    const target = pendingRevoke();
    if (!target) {
      return;
    }
    setRevoking(true);
    try {
      await api.revokeSession(target.id);
      setPendingRevoke(null);
      toast.ok(t("sessions.revoked"));
      await sessions.refetch();
    } catch (caught) {
      toast.error(apiMessage(caught));
    } finally {
      setRevoking(false);
    }
  };

  const revokeOthers = async () => {
    setRevoking(true);
    try {
      await api.revokeOtherSessions();
      setPendingOthers(false);
      toast.ok(t("sessions.othersRevoked"));
      await sessions.refetch();
    } catch (caught) {
      toast.error(apiMessage(caught));
    } finally {
      setRevoking(false);
    }
  };

  const columns = (): Column<AdminSessionView>[] => [
    {
      key: "created",
      header: t("sessions.created"),
      sortValue: (row) => row.created_at_ms,
      cell: (row) => (
        <div class="flex items-center gap-2">
          <span>{formatInstant(row.created_at_ms)}</span>
          <Show when={row.current}>
            <StatusBadge tone="active" label={t("sessions.thisSession")} />
          </Show>
        </div>
      ),
    },
    {
      key: "expires",
      header: t("sessions.expires"),
      sortValue: (row) => row.expires_at_ms,
      cell: (row) => <span class="text-ink-muted">{formatInstant(row.expires_at_ms)}</span>,
    },
    {
      key: "ip",
      header: t("sessions.ip"),
      cell: (row) => <span class="text-ink-muted">{row.ip ?? t("common.unknown")}</span>,
    },
    {
      key: "userAgent",
      header: t("sessions.userAgent"),
      cell: (row) => (
        <span class="block max-w-xs truncate text-ink-muted" title={row.user_agent ?? ""}>
          {row.user_agent ?? t("common.unknown")}
        </span>
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
      <PageHeader title={t("sessions.title")} description={t("sessions.description")} />
      <Card
        title={t("sessions.list")}
        actions={
          <Button
            variant="secondary"
            disabled={revoking() || (sessions.value()?.length ?? 0) <= 1}
            onClick={() => setPendingOthers(true)}
          >
            {t("sessions.revokeOthers")}
          </Button>
        }
      >
        <Show when={failureOf(sessions)}>
          {(message) => <Banner tone="danger" message={message()} />}
        </Show>
        <Show when={sessions.value()} fallback={<Skeleton label={t("common.loading")} rows={4} />}>
          {(loaded) => (
            <DataTable
              columns={columns()}
              rows={loaded()}
              pageSize={CLIENT_PAGE_SIZE}
              empty={<EmptyState title={t("sessions.empty")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <Button
                  variant="danger-ghost"
                  disabled={revoking() || row.current}
                  title={row.current ? t("sessions.cannotRevokeCurrent") : undefined}
                  onClick={() => setPendingRevoke(row)}
                >
                  {t("action.revoke")}
                </Button>
              )}
            />
          )}
        </Show>
      </Card>

      <ConfirmDialog
        open={pendingRevoke() !== null}
        title={t("sessions.revokeTitle")}
        message={t("sessions.revokeMessage")}
        confirmLabel={t("action.revoke")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        danger
        busy={revoking()}
        onConfirm={() => void revoke()}
        onCancel={() => setPendingRevoke(null)}
      />
      <ConfirmDialog
        open={pendingOthers()}
        title={t("sessions.revokeOthersTitle")}
        message={t("sessions.revokeOthersMessage")}
        confirmLabel={t("sessions.revokeOthers")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        danger
        busy={revoking()}
        onConfirm={() => void revokeOthers()}
        onCancel={() => setPendingOthers(false)}
      />
    </div>
  );
}
