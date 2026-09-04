// The console audit trail (ADR-0069, Track G2 slice 4). A fleet-wide, filterable record of who
// changed what across the console: every /admin mutation appends one entry, and this reads them back
// newest-first. Filters (entity type, action, acting admin) apply on the server; a row's before/after
// opens in a detail drawer. Read-only, behind console.data.read; global by design, so no tenant is
// required to open it.

import { createSignal, For, onMount, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { AdminIdentity, AuditEntry, TrailOrder } from "../api/types";
import { locale, t } from "../i18n";
import { formatRelativeAge } from "../lib/format";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  DataTable,
  Drawer,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

/**
 * How many entries one page of the table carries.
 *
 * The server pages this read (ADR-0098), so the table shows exactly what was fetched and the pager
 * counts from the server's total rather than from `rows.length`. Before this it pulled the newest
 * 200 and paged them locally at 20, which quietly made "of 200" the whole history.
 */
const PAGE_SIZE = 25;

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

/** The absolute instant, in the reader's locale, for the detail drawer. */
function absolute(atMs: number): string {
  return new Date(atMs).toLocaleString(locale());
}

/** A JSON value pretty-printed for the before/after panels, or a placeholder when there is none. */
function pretty(value: unknown): string {
  return value === null || value === undefined
    ? t("audit.noValue")
    : JSON.stringify(value, null, 2);
}

export function Audit() {
  const [entries, setEntries] = createSignal<readonly AuditEntry[] | null>(null);
  const [total, setTotal] = createSignal(0);
  const [offset, setOffset] = createSignal(0);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [selected, setSelected] = createSignal<AuditEntry | null>(null);

  // The applied filters. Empty strings mean "no filter"; the client drops absent fields.
  const [entityType, setEntityType] = createSignal("");
  const [action, setAction] = createSignal("");
  // The acting admin, held as the id the server filters on. The control offers names, because that
  // is what the table's actor column shows — asking an operator to paste a ULID they can only read
  // as an email was a filter nobody could use (#295).
  //
  // A plain `<select>` over the whole set, deliberately. Console admins are the people who
  // administer this deployment, not a tenant's staff: a handful, bounded by how many humans hold
  // console logins, and `GET /admin/admins` has always returned all of them unpaged. That is the
  // distinction ADR-0098 draws — a picker over a small bounded set is fine, one over a set that
  // grows with the business is not, and the People screen's roster was the latter.
  const [actor, setActor] = createSignal("");
  const [admins, setAdmins] = createSignal<AdminIdentity[]>([]);
  // Which end of the trail the pager walks from. The server orders the whole matching set, so this
  // is a re-read rather than a re-sort of the page (ADR-0098 B3-3).
  const [order, setOrder] = createSignal<TrailOrder>("newest");

  const load = async (from = offset()) => {
    setError("");
    setBusy(true);
    try {
      const page = await api.listAuditPage(
        {
          entityType: entityType().trim() || undefined,
          action: action().trim() || undefined,
          actorAdminId: actor().trim() || undefined,
        },
        { limit: PAGE_SIZE, offset: from },
        order(),
      );
      // A page that came back empty from somewhere other than the start means the matching set
      // shrank under the pager — usually a filter that just narrowed. Step back rather than showing
      // an empty table over a non-zero count.
      if (page.items.length === 0 && from > 0) {
        await load(Math.max(0, from - PAGE_SIZE));
        return;
      }
      setEntries(page.items);
      setTotal(page.total);
      setOffset(page.offset);
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  onMount(() => {
    void load(0);
    // The roster fills the actor picker. A failure here is not worth a banner over the trail: the
    // picker degrades to "any admin" and the rest of the screen works, so it is logged and dropped.
    void api
      .listAdmins()
      .then(setAdmins)
      .catch(() => setAdmins([]));
  });

  /**
   * Re-reads the trail from the other end, starting at its first page.
   *
   * The table hands back the *column's* direction, and the column is the instant an action
   * happened, so an ascending caret means earliest-first. The route's two orders are named for the
   * trail rather than for a column, which is why this maps instead of passing the flag through.
   *
   * A page-four offset means nothing in the reversed order — it would name the fourth page from the
   * other end — so this returns to the start.
   */
  const applyOrder = (_field: string, columnDescends: boolean) => {
    setOrder(columnDescends ? "newest" : "oldest");
    void load(0);
  };

  const columns = (): Column<AuditEntry>[] => [
    {
      key: "when",
      header: t("audit.when"),
      // The one server-sortable column, and the only one worth sorting: `at` is what an audit trail
      // is ordered by, and the server reverses the whole matching set rather than this page.
      sortField: "at",
      cell: (row) => (
        <span class="text-sm text-ink-muted" title={absolute(row.at_ms)}>
          {formatRelativeAge(ageSeconds(row.at_ms))}
        </span>
      ),
    },
    {
      key: "actor",
      header: t("audit.actor"),
      // No sort, here or on the next two columns. Each already has an *exact filter* above, which
      // answers "what did this admin do" better than ordering a million-row trail by a
      // low-cardinality column ever would — and a `sortValue` would sort this page as if it were
      // the trail, the lie the DataTable doc disclaims. Ordering by them is `?sort=` if it is ever
      // asked for; it was deliberately not decided here (ADR-0098 B3-3).
      cell: (row) => (
        <div class="flex flex-wrap items-center gap-2">
          <span class="text-ink">{row.actor_email}</span>
          <StatusBadge tone="neutral" label={row.actor_role} />
        </div>
      ),
    },
    {
      key: "action",
      header: t("audit.action"),
      cell: (row) => <span class="font-medium text-ink">{row.action}</span>,
    },
    {
      key: "entity",
      header: t("audit.entity"),
      cell: (row) => <span class="text-sm text-ink">{row.entity_type}</span>,
    },
  ];

  const detailRow = (label: string, value: string) => (
    <div class="flex justify-between gap-4 border-b border-line py-2 text-sm last:border-0">
      <span class="text-ink-muted">{label}</span>
      <span class="text-right text-ink">{value}</span>
    </div>
  );

  const jsonPanel = (label: string, value: unknown) => (
    <div>
      <span class="mb-1 block text-sm font-medium text-ink">{label}</span>
      <pre class="max-h-64 overflow-auto rounded-token border border-line bg-surface-raised p-3 font-mono text-xs text-ink">
        {pretty(value)}
      </pre>
    </div>
  );

  return (
    <div>
      <PageHeader title={t("audit.title")} description={t("audit.description")} />
      <Card
        title={t("audit.recent")}
        actions={
          <Button variant="secondary" disabled={busy()} onClick={() => void load(0)}>
            {t("action.refresh")}
          </Button>
        }
      >
        <div class="mb-4 grid gap-3 sm:grid-cols-3">
          <TextField
            label={t("audit.filter.entityType")}
            value={entityType()}
            onInput={setEntityType}
            placeholder={t("audit.filter.entityTypeHint")}
          />
          <TextField
            label={t("audit.filter.action")}
            value={action()}
            onInput={setAction}
            placeholder={t("audit.filter.actionHint")}
          />
          <label class="block">
            <span class="mb-1 block text-sm font-medium text-ink">
              {t("audit.filter.actor")}
            </span>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              aria-label={t("audit.filter.actor")}
              value={actor()}
              onChange={(event) => setActor(event.currentTarget.value)}
            >
              <option value="">{t("audit.filter.anyActor")}</option>
              <For each={admins()}>
                {(admin) => (
                  <option value={admin.id}>
                    {admin.name} ({admin.email})
                  </option>
                )}
              </For>
            </select>
          </label>
        </div>
        <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
        <Show when={entries()} fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}>
          {(loaded) => (
            // No `searchText`: a client-side box would filter this page rather than the log, which
            // is the lie the DataTable doc warns about. The three fields above already search the
            // whole trail on the server — and did so even before this, unlike the box, which only
            // ever saw the newest 200 entries. Free-text across fields would be ADR-0098's `?q=`,
            // which on a trail of millions is a trigram-index decision and was not made in B3-3.
            //
            // `onSort` is what puts this table in server-sort mode, and until it was passed the
            // headers re-sorted the page — twenty-five rows arranged as if they were the trail.
            <DataTable
              columns={columns()}
              rows={loaded()}
              pageSize={PAGE_SIZE}
              serverTotal={total()}
              onPage={(next) => void load(next)}
              onSort={applyOrder}
              empty={<EmptyState title={t("audit.empty")} description={t("audit.emptyHint")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <Button variant="secondary" onClick={() => setSelected(row)}>
                  {t("audit.details")}
                </Button>
              )}
            />
          )}
        </Show>
      </Card>

      <Drawer
        open={selected() !== null}
        title={selected()?.action ?? ""}
        closeLabel={t("action.close")}
        onClose={() => setSelected(null)}
      >
        <Show when={selected()}>
          {(entry) => (
            <div class="flex flex-col gap-4">
              <div>
                {detailRow(t("audit.actor"), entry().actor_email)}
                {detailRow(t("audit.role"), entry().actor_role)}
                {detailRow(t("audit.action"), entry().action)}
                {detailRow(t("audit.entity"), entry().entity_type)}
                {detailRow(t("audit.entityId"), entry().entity_id)}
                {detailRow(t("audit.when"), absolute(entry().at_ms))}
                {detailRow(t("audit.tenant"), entry().tenant_id ?? t("audit.global"))}
              </div>
              {jsonPanel(t("audit.before"), entry().before)}
              {jsonPanel(t("audit.after"), entry().after)}
              <TechnicalDetails label={t("common.technicalDetails")}>
                {entry().id}
              </TechnicalDetails>
            </div>
          )}
        </Show>
      </Drawer>
    </div>
  );
}
