// Chain integrity ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4,
// [ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)): the contradictions
// the cloud refused, where a human can see them.
//
// Every event a store writes carries the hash of the one before it, and every shift close publishes
// the chain's head to the cloud. The cloud keeps one head per chain length per store, and when a
// store offers a second, different head at a length it already holds — or when recomputing the
// records behind an anchor produces a chain that disagrees with itself — it refuses the claim and
// files the pair. This screen is that file.
//
// Until it existed a finding reached a human only as a line in the server's log and a row nobody
// could read. For a mechanism whose entire product is *being noticed*, that is most of the way to
// not having it.
//
// **An empty table is the expected state and says so.** Every other screen's empty state means
// "nothing has happened yet"; this one means "nothing is wrong", and the wording has to carry that
// difference or an operator learns to skip the screen.
//
// Behind console.data.read, tenant-scoped, and narrowed to the store in context when there is one.

import { Show } from "solid-js";

import { api } from "../api/client";
import type { ChainFinding, Store } from "../api/types";
import { t } from "../i18n";
import { formatRelativeAge } from "../lib/format";
import { createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import { storeName, storeId } from "../state/session";
import { Banner, Card, PageHeader, Skeleton, StatusBadge } from "../components/ui";
import { type Column, DataTable, EmptyState } from "../components/kit";

/** How often the findings re-read, so one arriving mid-shift shows without a manual refresh. */
const POLL_MS = 30_000;

/** How much of a 64-character digest is enough to tell two apart on a row. */
const DIGEST_PREVIEW = 12;

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

/** The head of a digest, for a table cell. The full value is on the row's title attribute. */
function shortDigest(digest: string): string {
  return digest.slice(0, DIGEST_PREVIEW);
}

export function ChainFindings() {
  // The findings and a store-name lookup together: one state for two reads that are useless apart,
  // so a half-loaded screen cannot show findings with raw ULIDs where shop names belong.
  const findings = createAdminResource(
    async (tenant, store) => {
      const [rows, stores] = await Promise.all([
        api.listChainFindings(tenant, store || undefined),
        api.listStores(tenant),
      ]);
      return {
        rows,
        names: new Map(stores.map((row: Store) => [row.store_id, row.name])),
      };
    },
    { scope: "tenant", intervalMs: POLL_MS, revalidateOnFocus: true },
  );

  // A store's name if the registry knows it, else its raw id — a finding against an archived store
  // still shows, and is exactly the kind nobody should lose.
  const storeLabel = (id: string) => findings.value()?.names.get(id) ?? id;

  const columns = (): Column<ChainFinding>[] => [
    {
      key: "store",
      header: t("chain.store"),
      sortValue: (row) => storeLabel(row.store_id),
      cell: (row) => <span class="font-medium text-ink">{storeLabel(row.store_id)}</span>,
    },
    {
      key: "position",
      header: t("chain.position"),
      sortValue: (row) => row.chain_seq,
      cell: (row) => <span class="text-ink tabular-nums">{row.chain_seq}</span>,
    },
    {
      key: "heads",
      header: t("chain.heads"),
      sortValue: (row) => row.held_head,
      cell: (row) => (
        // Both, side by side, because the pair *is* the finding — either one alone says nothing.
        <span class="font-mono text-xs text-ink-muted" title={`${row.held_head} / ${row.offered_head}`}>
          {shortDigest(row.held_head)} ≠ {shortDigest(row.offered_head)}
        </span>
      ),
    },
    {
      key: "event",
      header: t("chain.event"),
      sortValue: (row) => row.offered_event_id,
      cell: (row) => (
        <span class="font-mono text-xs text-ink-muted" title={row.offered_event_id}>
          {row.offered_event_id}
        </span>
      ),
    },
    {
      key: "when",
      header: t("chain.when"),
      sortValue: (row) => row.noticed_at_ms,
      cell: (row) => (
        <span class="text-sm text-ink-muted">{formatRelativeAge(ageSeconds(row.noticed_at_ms))}</span>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("chain.title")} description={t("chain.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={failureOf(findings)}>
            {(message) => <Banner tone="danger" message={message()} />}
          </Show>

          <Card
            title={storeId() ? t("chain.forStore", { store: storeName() }) : t("chain.all")}
            actions={
              <Show when={findings.value()} keyed>
                {(loaded) => (
                  <StatusBadge
                    tone={loaded.rows.length > 0 ? "danger" : "active"}
                    label={
                      loaded.rows.length > 0
                        ? t("chain.someRefused", { count: loaded.rows.length })
                        : t("chain.noneRefused")
                    }
                  />
                )}
              </Show>
            }
          >
            <Show
              when={findings.value()}
              fallback={<Skeleton label={t("common.loading")} rows={4} />}
            >
              {(loaded) => (
                <DataTable
                  columns={columns()}
                  rows={loaded().rows}
                  searchText={(row) => `${storeLabel(row.store_id)} ${row.offered_event_id}`}
                  pageSize={15}
                  empty={
                    // Not "nothing has happened yet" — "nothing is wrong". Every store that has
                    // published a head has had it checked, and none contradicted what the cloud
                    // already held.
                    <EmptyState title={t("chain.empty")} description={t("chain.emptyHint")} />
                  }
                />
              )}
            </Show>
          </Card>

          <Card title={t("chain.limits")}>
            {/* Said on the screen, not only in an ADR. Somebody reading an empty table needs to
                know what "clean" does and does not mean, or they will read more into it than is
                there. */}
            <ul class="flex list-disc flex-col gap-2 pl-5 text-sm text-ink-muted">
              <li>{t("chain.limitAnchor")}</li>
              <li>{t("chain.limitGap")}</li>
              <li>{t("chain.limitEvident")}</li>
            </ul>
          </Card>
        </div>
      </RequireContext>
    </div>
  );
}
