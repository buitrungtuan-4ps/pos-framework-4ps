// The operational alerts console (ADR-0073, Track O2). A fleet-wide, read-then-act view of the
// conditions the alert evaluator maintains: a store gone offline, a relay backlog, a disabled
// webhook, an unhealthy projector. The active set loads on mount (no tenant needed — alerts are a
// server-wide operational signal); a toggle swaps in recent history (active + resolved). An operator
// with console.alerts.manage (Owner/Admin/Ops) can acknowledge or resolve a row; both are audited on
// the server and both are idempotent, so a double-click is harmless. Read-only for everyone else.

import { createSignal, onMount, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Alert, AlertSeverity } from "../api/types";
import { locale, type MessageKey, t } from "../i18n";
import { formatRelativeAge } from "../lib/format";
import { actingAdmin } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import {
  type Column,
  DataTable,
  Drawer,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

/** The most rows one recent-history read returns; the server caps it too. */
const ALERT_LIMIT = 200;

/** The roles the server lets manage (acknowledge/resolve) alerts — mirrored so the UI hides what the
 *  role cannot do; the server's console.alerts.manage check is the real gate (ADR-0067/0073). */
const ALERT_MANAGERS = new Set(["owner", "admin", "ops"]);

/** The stable wire `kind` tokens the evaluator emits, mapped to their localized labels. An unknown
 *  future kind falls back to its raw token rather than crashing the screen. */
const KIND_LABEL: Record<string, MessageKey> = {
  store_offline: "alerts.kind.storeOffline",
  relay_backlog: "alerts.kind.relayBacklog",
  webhook_disabled: "alerts.kind.webhookDisabled",
  projector_unhealthy: "alerts.kind.projectorUnhealthy",
  jetstream_capacity: "alerts.kind.jetstreamCapacity",
  print_agent_stalled: "alerts.kind.printAgentStalled",
};

/** The three severities, least-to-most, mapped to their localized labels. */
const SEVERITY_LABEL: Record<AlertSeverity, MessageKey> = {
  info: "alerts.severity.info",
  warning: "alerts.severity.warning",
  critical: "alerts.severity.critical",
};

/** Sort rank for a severity, so a sort surfaces the most serious first. */
const SEVERITY_RANK: Record<AlertSeverity, number> = { info: 0, warning: 1, critical: 2 };

/** The three lifecycle states of an alert, derived from its timestamps. */
type AlertState = "firing" | "acknowledged" | "resolved";

function alertState(alert: Alert): AlertState {
  if (alert.resolved_at_ms !== null) {
    return "resolved";
  }
  if (alert.acknowledged_at_ms !== null) {
    return "acknowledged";
  }
  return "firing";
}

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

/** The absolute instant, in the reader's locale, for the detail drawer. */
function absolute(atMs: number): string {
  return new Date(atMs).toLocaleString(locale());
}

/** A JSON value pretty-printed for the detail panel, or a placeholder when there is none. */
function pretty(value: unknown): string {
  return value === null || value === undefined
    ? t("alerts.noValue")
    : JSON.stringify(value, null, 2);
}

export function Alerts() {
  const [alerts, setAlerts] = createSignal<Alert[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [recent, setRecent] = createSignal(false);
  const [selected, setSelected] = createSignal<Alert | null>(null);

  // Whether the signed-in admin may acknowledge/resolve. Absent until whoami loads (the Shell fetches
  // it once), which safely hides the actions rather than flashing them for a role that cannot act.
  const canManage = () => {
    const role = actingAdmin()?.role;
    return role !== undefined && ALERT_MANAGERS.has(role);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const loaded = await api.listAlerts(recent(), recent() ? ALERT_LIMIT : undefined);
      setAlerts(loaded);
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  onMount(() => void load());

  // Swap the active/recent scope, then reload from the server (the two are different reads).
  const setScope = (next: boolean) => {
    if (next === recent()) {
      return;
    }
    setRecent(next);
    void load();
  };

  const acknowledge = async (alert: Alert) => {
    setBusy(true);
    try {
      await api.acknowledgeAlert(alert.id);
      toast.ok(t("alerts.acknowledged"));
      setSelected(null);
      await load();
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const resolve = async (alert: Alert) => {
    setBusy(true);
    try {
      await api.resolveAlert(alert.id);
      toast.ok(t("alerts.resolved"));
      setSelected(null);
      await load();
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const kindLabel = (kind: string): string => {
    const key = KIND_LABEL[kind];
    return key ? t(key) : kind;
  };

  const scopeLabel = (alert: Alert): string => alert.tenant_id ?? t("alerts.fleet");

  const stateBadge = (alert: Alert) => {
    const state = alertState(alert);
    if (state === "resolved") {
      return <StatusBadge tone="archived" label={t("alerts.state.resolved")} />;
    }
    if (state === "acknowledged") {
      return <StatusBadge tone="neutral" label={t("alerts.state.acknowledged")} />;
    }
    return <StatusBadge tone="danger" label={t("alerts.state.firing")} />;
  };

  const columns = (): Column<Alert>[] => [
    {
      key: "severity",
      header: t("alerts.severity"),
      sortValue: (row) => SEVERITY_RANK[row.severity],
      cell: (row) => (
        <StatusBadge
          tone={row.severity === "critical" ? "danger" : "neutral"}
          label={t(SEVERITY_LABEL[row.severity])}
        />
      ),
    },
    {
      key: "kind",
      header: t("alerts.kind"),
      sortValue: (row) => kindLabel(row.kind),
      cell: (row) => <span class="font-medium text-ink">{kindLabel(row.kind)}</span>,
    },
    {
      key: "summary",
      header: t("alerts.summary"),
      cell: (row) => <span class="text-sm text-ink">{row.summary}</span>,
    },
    {
      key: "scope",
      header: t("alerts.scope"),
      sortValue: (row) => scopeLabel(row),
      cell: (row) => <span class="text-sm text-ink-muted">{scopeLabel(row)}</span>,
    },
    {
      key: "lastSeen",
      header: t("alerts.lastSeen"),
      sortValue: (row) => row.last_seen_at_ms,
      cell: (row) => (
        <span class="text-sm text-ink-muted" title={absolute(row.last_seen_at_ms)}>
          {formatRelativeAge(ageSeconds(row.last_seen_at_ms))}
        </span>
      ),
    },
    {
      key: "state",
      header: t("alerts.status"),
      sortValue: (row) => alertState(row),
      cell: (row) => stateBadge(row),
    },
  ];

  const rowActions = (row: Alert) => {
    const state = alertState(row);
    return (
      <div class="flex flex-wrap gap-2">
        <Button variant="secondary" onClick={() => setSelected(row)}>
          {t("alerts.details")}
        </Button>
        <Show when={canManage() && state === "firing"}>
          <Button variant="secondary" disabled={busy()} onClick={() => void acknowledge(row)}>
            {t("alerts.acknowledge")}
          </Button>
        </Show>
        <Show when={canManage() && state !== "resolved"}>
          <Button variant="primary" disabled={busy()} onClick={() => void resolve(row)}>
            {t("alerts.resolve")}
          </Button>
        </Show>
      </div>
    );
  };

  const detailRow = (label: string, value: string) => (
    <div class="flex justify-between gap-4 border-b border-line py-2 text-sm last:border-0">
      <span class="text-ink-muted">{label}</span>
      <span class="text-right text-ink">{value}</span>
    </div>
  );

  return (
    <div>
      <PageHeader title={t("alerts.title")} description={t("alerts.description")} />
      <Card
        title={recent() ? t("alerts.recent") : t("alerts.active")}
        actions={
          <div class="flex flex-wrap gap-2">
            <Button variant={recent() ? "secondary" : "primary"} onClick={() => setScope(false)}>
              {t("alerts.showActive")}
            </Button>
            <Button variant={recent() ? "primary" : "secondary"} onClick={() => setScope(true)}>
              {t("alerts.showRecent")}
            </Button>
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          </div>
        }
      >
        <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show when={alerts()} fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}>
          {(loaded) => (
            <DataTable
              columns={columns()}
              rows={loaded()}
              searchText={(row) => `${kindLabel(row.kind)} ${row.summary} ${scopeLabel(row)}`}
              pageSize={20}
              empty={
                <EmptyState
                  title={recent() ? t("alerts.emptyRecent") : t("alerts.empty")}
                  description={t("alerts.emptyHint")}
                />
              }
              actionsHeader={t("common.actions")}
              actions={rowActions}
            />
          )}
        </Show>
      </Card>

      <Drawer
        open={selected() !== null}
        title={selected() ? kindLabel(selected()!.kind) : ""}
        closeLabel={t("action.close")}
        onClose={() => setSelected(null)}
      >
        <Show when={selected()}>
          {(alert) => (
            <div class="flex flex-col gap-4">
              <div>
                {detailRow(t("alerts.severity"), t(SEVERITY_LABEL[alert().severity]))}
                {detailRow(t("alerts.kind"), kindLabel(alert().kind))}
                {detailRow(t("alerts.summary"), alert().summary)}
                {detailRow(t("alerts.scope"), scopeLabel(alert()))}
                {detailRow(t("alerts.firstSeen"), absolute(alert().first_seen_at_ms))}
                {detailRow(t("alerts.lastSeen"), absolute(alert().last_seen_at_ms))}
                {detailRow(
                  t("alerts.acknowledgedAt"),
                  alert().acknowledged_at_ms === null
                    ? t("alerts.noValue")
                    : absolute(alert().acknowledged_at_ms!),
                )}
                {detailRow(
                  t("alerts.resolvedAt"),
                  alert().resolved_at_ms === null
                    ? t("alerts.noValue")
                    : absolute(alert().resolved_at_ms!),
                )}
              </div>
              <div>
                <span class="mb-1 block text-sm font-medium text-ink">{t("alerts.detail")}</span>
                <pre class="max-h-64 overflow-auto rounded-token border border-line bg-surface-raised p-3 font-mono text-xs text-ink">
                  {pretty(alert().detail)}
                </pre>
              </div>
              <Show when={canManage()}>
                <div class="flex flex-wrap gap-2">
                  <Show when={alertState(alert()) === "firing"}>
                    <Button
                      variant="secondary"
                      disabled={busy()}
                      onClick={() => void acknowledge(alert())}
                    >
                      {t("alerts.acknowledge")}
                    </Button>
                  </Show>
                  <Show when={alertState(alert()) !== "resolved"}>
                    <Button
                      variant="primary"
                      disabled={busy()}
                      onClick={() => void resolve(alert())}
                    >
                      {t("alerts.resolve")}
                    </Button>
                  </Show>
                </div>
              </Show>
              <TechnicalDetails label={t("common.technicalDetails")}>
                {alert().id}
                <Show when={alert().dedup_key}>
                  <div class="mt-1">{alert().dedup_key}</div>
                </Show>
              </TechnicalDetails>
            </div>
          )}
        </Show>
      </Drawer>
    </div>
  );
}
