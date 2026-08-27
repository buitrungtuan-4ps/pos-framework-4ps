// My sessions (ADR-0067, Track G1): every admin lists their own live console sessions and revokes
// them. Self-service — available to any authenticated admin regardless of role — so it carries no
// tenant/store context. The session making this request is marked and protected from accidental
// self-revocation; "sign out everywhere else" keeps only the current one. The handle shown is the
// server's opaque revocation id (a hash, never the token, and not reversible to it).

import { createSignal, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { AdminSessionView } from "../api/types";
import { locale, t } from "../i18n";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

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
  const [sessions, setSessions] = createSignal<AdminSessionView[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [pendingRevoke, setPendingRevoke] = createSignal<AdminSessionView | null>(null);
  const [pendingOthers, setPendingOthers] = createSignal(false);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setSessions(await api.listSessions());
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  void load();

  const revoke = async () => {
    const target = pendingRevoke();
    if (!target) {
      return;
    }
    setBusy(true);
    try {
      await api.revokeSession(target.id);
      setPendingRevoke(null);
      toast.ok(t("sessions.revoked"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const revokeOthers = async () => {
    setBusy(true);
    try {
      await api.revokeOtherSessions();
      setPendingOthers(false);
      toast.ok(t("sessions.othersRevoked"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
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
          <div class="flex gap-2">
            <Button
              variant="secondary"
              disabled={busy() || (sessions()?.length ?? 0) <= 1}
              onClick={() => setPendingOthers(true)}
            >
              {t("sessions.revokeOthers")}
            </Button>
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          </div>
        }
      >
        <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show when={sessions()} fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}>
          {(loaded) => (
            <DataTable
              columns={columns()}
              rows={loaded()}
              empty={<EmptyState title={t("sessions.empty")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <Button
                  variant="danger"
                  disabled={busy() || row.current}
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
        busy={busy()}
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
        busy={busy()}
        onConfirm={() => void revokeOthers()}
        onCancel={() => setPendingOthers(false)}
      />
    </div>
  );
}
