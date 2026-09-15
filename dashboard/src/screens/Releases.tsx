// The publish centre — config releases
// ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)), on the authoring
// kit ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)).
//
// # What this screen is for
//
// A Tết menu is not one node. It is a menu, the tax rates it prices against, the campaigns that
// discount it, the reason codes the staff void it with, and the layout that shows it — five to nine
// nodes, to forty stores, all switching on together. Before this screen that was forty-five to
// eighty separate publishes, each with its own button, each landing whenever the operator got to it
// (findings F8 / F9 / F10). This is the one place that names the set, times it, and reports it.
//
// # Why the grid is the point, not the button
//
// A release fans out to one `scheduled_publishes` row per (node, store) pair, and those rows fail
// individually. A screen that could only say "done" or "failed" would be useless at forty stores:
// what an operator needs at 04:05 on Monday is the two shops that did not take it. So the detail
// leads with the node × store grid, and a pair carries the reason it last could not apply — a
// failed pair is still *pending*, because the activator retries it.
//
// # Why the moment is two fields and not one
//
// "Monday 04:00 at each store" is not a time. It is one instant per timezone, and a fleet spanning
// Ho Chi Minh City and Tokyo has a two-hour spread (finding F17). The wall-clock form lets the
// operator say what they mean and be shown the forty instants it resolved to; the instant form is
// the escape hatch for a store whose locale nobody has published yet, which the wall-clock form
// refuses **by name** rather than quietly assuming UTC.
//
// Everything here is operational metadata: release names, node keys, store ids and publish
// outcomes. No customer or employee identifier passes through this screen.

import { useSearchParams } from "@solidjs/router";
import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import type {
  Json,
  Release,
  ReleaseNode,
  ReleasePair,
  ReleaseReport,
  ReleaseStatus,
  ScheduledPublishStatus,
} from "../api/types";
import { type MessageKey, t } from "../i18n";
import { apiMessage } from "../lib/errors";
import { RELEASE_NODE_PARAM } from "../lib/publish-copy";
import { createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  ComboboxField,
  MultiComboboxField,
  PageHeader,
  StatusBadge,
  TextField,
} from "../components/ui";
import {
  type Column,
  DataTable,
  EmptyState,
  FormPanel,
  TechnicalDetails,
} from "../components/kit";
import { useEntityCrud } from "../lib/entity-crud";
import { toast } from "../components/Toast";

/**
 * The nodes a release may carry, in the order an operator reaches for them.
 *
 * The same table the batch publish offers, minus the six `copy` kinds. Those are settings taken
 * *from a named source store* — "make the rest of the group look like this one" — and that question
 * has no good answer inside a release, which is a set of nodes chosen once and applied at forty
 * shops at a future instant: the source store's value could change between the choosing and the
 * firing, which is precisely the leak snapshot-at-schedule exists to prevent. They stay on the
 * Store groups screen, where the publish is immediate and the source is visible.
 */
const RELEASE_NODES: readonly { key: string; label: MessageKey }[] = [
  { key: "menu", label: "releases.node.menu" },
  { key: "tax", label: "releases.node.tax" },
  { key: "campaigns", label: "releases.node.campaigns" },
  { key: "inventory", label: "releases.node.inventory" },
  { key: "reason_codes", label: "releases.node.reasonCodes" },
  { key: "permissions", label: "releases.node.permissions" },
  { key: "floor", label: "releases.node.floor" },
];

/** How each release state is drawn. `partial` is danger; `applied` is the only unqualified good. */
const STATUS_TONE: Record<ReleaseStatus, "active" | "archived" | "neutral" | "danger"> = {
  draft: "archived",
  scheduled: "neutral",
  applying: "neutral",
  applied: "active",
  partial: "danger",
};

const STATUS_LABEL: Record<ReleaseStatus, MessageKey> = {
  draft: "releases.status.draft",
  scheduled: "releases.status.scheduled",
  applying: "releases.status.applying",
  applied: "releases.status.applied",
  partial: "releases.status.partial",
};

/**
 * How each pair is drawn.
 *
 * `pending` is neutral rather than a warning even when it carries a failure: the activator will try
 * again, and colouring a retry as a loss teaches an operator to intervene in something that is
 * about to fix itself. The reason is shown as text beside it, which is the honest weight.
 */
const PAIR_TONE: Record<ScheduledPublishStatus, "active" | "archived" | "neutral"> = {
  pending: "neutral",
  applied: "active",
  cancelled: "archived",
};

const PAIR_LABEL: Record<ScheduledPublishStatus, MessageKey> = {
  pending: "releases.pair.pending",
  applied: "releases.pair.applied",
  cancelled: "releases.pair.cancelled",
};

/** A release's moment, in the operator's own words. */
function momentOf(release: Release): string {
  if (release.wall_clock_date && release.wall_clock_time) {
    return t("releases.momentLocal", {
      date: release.wall_clock_date,
      time: release.wall_clock_time,
    });
  }
  if (release.instant_at_ms !== null) {
    return new Date(release.instant_at_ms).toLocaleString();
  }
  return t("releases.momentNone");
}

export function Releases() {
  // A publish bar can hand this screen a node — "Tax rates is ready to go into a release" (L1). It is
  // remembered rather than acted on: the node is chosen when a release is *scheduled*, which is also
  // when it is snapshotted, so the honest thing is to pre-tick it there rather than to invent a
  // release around it here.
  const [searchParams] = useSearchParams();
  const arrivedWith = () => {
    const asked = searchParams[RELEASE_NODE_PARAM];
    const node = Array.isArray(asked) ? asked[0] : asked;
    return node && RELEASE_NODES.some((known) => known.key === node) ? node : "";
  };

  // The releases, the cohorts they may be aimed at, and the store registry as one state: a grid
  // that got the pairs but not the registry names forty shops in ULIDs.
  const centre = createAdminResource(
    async (tenant) => {
      const [releases, groups, stores] = await Promise.all([
        api.listReleases(tenant),
        api.listStoreGroups(tenant),
        api.listStores(tenant),
      ]);
      return { releases, groups, stores };
    },
    { scope: "tenant" },
  );
  const rows = () => centre.value()?.releases ?? null;
  const groups = () => centre.value()?.groups ?? [];
  const stores = () => centre.value()?.stores ?? [];

  // Three independent lifecycles (ADR-0121 §3): naming a release must not grey out the schedule,
  // and one slow schedule must not disable the list.
  const drafting = useEntityCrud<never>();
  const scheduling = useEntityCrud<Release>();
  const [error, setError] = createSignal("");

  // The draft form.
  const [name, setName] = createSignal("");
  const [targetGroup, setTargetGroup] = createSignal("");
  const [wallDate, setWallDate] = createSignal("");
  const [wallTime, setWallTime] = createSignal("04:00");
  const [instantAt, setInstantAt] = createSignal("");

  // The schedule form.
  const [nodes, setNodes] = createSignal<string[]>([]);
  const [menuId, setMenuId] = createSignal("");
  const [explicitStores, setExplicitStores] = createSignal<string[]>([]);
  const [menus, setMenus] = createSignal<{ value: string; label: string }[]>([]);

  // The report the screen is showing. The deliverable, not the button.
  const [report, setReport] = createSignal<ReleaseReport | null>(null);

  /**
   * What is still coming, soonest first — the calendar half of this screen (L2).
   *
   * A list ordered by the moment rather than a month grid, and by the *stated* moment rather than a
   * converted one: a wall-clock release has no single instant to place on a calendar square, which
   * is the same reason the table prints "04:00, each shop's own clock". Drafts are in, because a
   * release nobody has timed is exactly the one an operator forgets.
   */
  const upcoming = () =>
    (rows() ?? [])
      .filter((row) => row.status !== "applied" && row.status !== "partial")
      .slice()
      .sort((left, right) => momentOf(left).localeCompare(momentOf(right)));

  const storeName = (id: string) => stores().find((row) => row.store_id === id)?.name ?? id;
  const groupName = (id: string) => groups().find((row) => row.group_id === id)?.name ?? id;
  const nodeLabel = (key: string) => {
    const known = RELEASE_NODES.find((node) => node.key === key);
    return known ? t(known.label) : key;
  };

  const storeOptions = () =>
    stores()
      .filter((row) => row.status === "active")
      .map((row) => ({ value: row.store_id, label: row.name, keywords: [row.store_id] }));

  const groupOptions = () =>
    groups()
      .filter((row) => row.status === "active")
      .map((row) => ({ value: row.group_id, label: row.name, keywords: [row.group_id] }));

  const nodeOptions = () =>
    RELEASE_NODES.map((node) => ({ value: node.key, label: t(node.label) }));

  const openDraft = () => {
    setName("");
    setTargetGroup("");
    setWallDate("");
    setWallTime("04:00");
    setInstantAt("");
    setError("");
    drafting.create();
  };

  const saveDraft = () => {
    const label = name().trim();
    if (!label) {
      drafting.refuse(t("releases.nameRequired"));
      return;
    }
    const instant = instantAt().trim();
    const instantMs = instant ? new Date(instant).getTime() : undefined;
    if (instant && Number.isNaN(instantMs)) {
      drafting.refuse(t("releases.instantInvalid"));
      return;
    }
    // Both forms at once is the server's refusal too, but saying it here spares a round trip and
    // lets the operator see which of the two fields to clear.
    if (instant && wallDate()) {
      drafting.refuse(t("releases.momentConflict"));
      return;
    }
    void drafting
      .run(async () => {
        const created = await api.createRelease(tenantId(), label, {
          targetGroupId: targetGroup() || undefined,
          wallClockDate: wallDate() || undefined,
          wallClockTime: wallDate() ? wallTime() : undefined,
          instantAtMs: instantMs,
        });
        toast.ok(t("releases.drafted", { name: created.name }));
      })
      .then((saved) => {
        if (saved) {
          void centre.refetch();
        }
      });
  };

  const openSchedule = (release: Release) => {
    setNodes(arrivedWith() ? [arrivedWith()] : []);
    setMenuId("");
    setExplicitStores([]);
    setError("");
    // The menu list is only needed once this panel is open, and a failure to load it costs the
    // operator a picker rather than the screen.
    void api
      .listMenus(tenantId())
      .then((list) => setMenus(list.map((menu) => ({ value: menu.menu_id, label: menu.name }))))
      .catch(() => setMenus([]));
    scheduling.edit(release);
  };

  const submitSchedule = () => {
    const release = scheduling.subject();
    if (!release) {
      return;
    }
    const chosen = nodes();
    if (chosen.length === 0) {
      scheduling.refuse(t("releases.nodesRequired"));
      return;
    }
    if (chosen.includes("menu") && !menuId()) {
      scheduling.refuse(t("releases.menuRequired"));
      return;
    }
    // Aimed at neither a cohort nor a store list is the one shape the server cannot resolve, and
    // the operator is the only one who knows which they meant.
    if (!release.target_group_id && explicitStores().length === 0) {
      scheduling.refuse(t("releases.targetRequired"));
      return;
    }
    const payload: ReleaseNode[] = chosen.map((key) => ({
      node: key,
      arguments: key === "menu" ? { menu_id: menuId() } : ({} as Json),
    }));
    void scheduling
      .run(async () => {
        const scheduled = await api.scheduleRelease(
          tenantId(),
          release.release_id,
          payload,
          explicitStores().length > 0 ? explicitStores() : undefined,
        );
        setReport(scheduled);
        toast.ok(
          t("releases.scheduled", {
            name: scheduled.release.name,
            pairs: String(scheduled.pairs.length),
          }),
        );
      })
      .then((saved) => {
        if (saved) {
          void centre.refetch();
        }
      });
  };

  const openReport = (release: Release) => {
    setError("");
    void api
      .readRelease(tenantId(), release.release_id)
      .then(setReport)
      .catch((cause: unknown) => setError(apiMessage(cause)));
  };

  const cancel = (release: Release) => {
    void api
      .cancelRelease(tenantId(), release.release_id)
      .then((cancelled) => {
        setReport(cancelled);
        toast.ok(t("releases.cancelled", { name: cancelled.release.name }));
        void centre.refetch();
      })
      .catch((cause: unknown) => setError(apiMessage(cause)));
  };

  const columns = (): Column<Release>[] => [
    {
      key: "name",
      header: t("releases.name"),
      cell: (row) => (
        <div>
          <div>{row.name}</div>
          <Show when={row.target_group_id}>
            {(group) => (
              <div class="text-sm text-ink-muted">
                {t("releases.aimedAt", { group: groupName(group()) })}
              </div>
            )}
          </Show>
        </div>
      ),
      sortValue: (row) => row.name,
    },
    {
      key: "moment",
      header: t("releases.moment"),
      cell: (row) => momentOf(row),
      sortValue: (row) => row.wall_clock_date ?? String(row.instant_at_ms ?? 0),
    },
    {
      key: "standing",
      header: t("releases.standing"),
      cell: (row) => (
        <StatusBadge tone={STATUS_TONE[row.status]} label={t(STATUS_LABEL[row.status])} />
      ),
      sortValue: (row) => row.status,
    },
    {
      key: "actions",
      header: t("releases.actions"),
      cell: (row) => (
        <div class="flex flex-wrap gap-2">
          <Button variant="ghost" onClick={() => openReport(row)}>
            {t("releases.viewReport")}
          </Button>
          <Show when={row.status === "draft"}>
            {/* An ellipsis because it opens the form rather than scheduling: the submit inside says
                "Schedule", and two controls on one screen reading the same word is one of them
                lying about what it does. */}
            <Button onClick={() => openSchedule(row)}>{t("releases.schedule")}</Button>
          </Show>
          <Show when={row.status === "draft" || row.status === "scheduled"}>
            <Button variant="ghost" onClick={() => cancel(row)}>
              {t("releases.cancel")}
            </Button>
          </Show>
        </div>
      ),
    },
    {
      key: "id",
      header: t("releases.id"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.release_id}</TechnicalDetails>
      ),
    },
  ];

  /** One pair of the node × store grid. */
  const pairRow = (pair: ReleasePair) => (
    <li class="flex flex-wrap items-center gap-2 rounded-token border border-line bg-surface-raised px-3 py-2">
      <StatusBadge tone={PAIR_TONE[pair.status]} label={t(PAIR_LABEL[pair.status])} />
      <span class="text-sm text-ink">{storeName(pair.store_id)}</span>
      <span class="text-sm text-ink-muted">{nodeLabel(pair.node)}</span>
      <span class="text-sm text-ink-muted">
        {new Date(pair.effective_at_ms).toLocaleString()}
      </span>
      <Show when={pair.failure}>
        {(reason) => <span class="text-sm text-danger">{reason()}</span>}
      </Show>
      <Show when={pair.applied_version_id}>
        {(version) => (
          <TechnicalDetails label={t("common.technicalDetails")}>{version()}</TechnicalDetails>
        )}
      </Show>
    </li>
  );

  return (
    <div>
      <PageHeader title={t("releases.title")} description={t("releases.description")} />
      <RequireContext need="tenant">
        <Card
          title={t("releases.list")}
          actions={<Button onClick={openDraft}>{t("releases.new")}</Button>}
        >
          <Show when={arrivedWith()}>
            {(node) => (
              <Banner tone="ok" message={t("releases.fromNode", { node: nodeLabel(node()) })} />
            )}
          </Show>
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={failureOf(centre)}>
            {(message) => <Banner tone="danger" message={message()} />}
          </Show>
          <Show when={rows()}>
            {(loaded) => (
              <DataTable
                columns={columns()}
                rows={loaded()}
                searchText={(row) => row.name}
                pageSize={12}
                empty={
                  <EmptyState
                    title={t("releases.emptyTitle")}
                    description={t("releases.emptyBody")}
                    action={<Button onClick={openDraft}>{t("releases.new")}</Button>}
                  />
                }
              />
            )}
          </Show>
        </Card>

        <Card title={t("releases.upcoming")}>
          <Show
            when={upcoming().length > 0}
            fallback={<p class="text-sm text-ink-muted">{t("releases.upcomingEmpty")}</p>}
          >
            <ul class="flex flex-col gap-1">
              <For each={upcoming()}>
                {(row) => (
                  <li class="flex flex-wrap items-center gap-2 rounded-token border border-line bg-surface-raised px-3 py-2">
                    <span class="text-sm text-ink-muted tabular-nums">{momentOf(row)}</span>
                    <span class="text-sm text-ink">{row.name}</span>
                    <StatusBadge tone={STATUS_TONE[row.status]} label={t(STATUS_LABEL[row.status])} />
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Card>

        <Show when={report()}>
          {(shown) => (
            <Card title={t("releases.reportTitle", { name: shown().release.name })}>
              <p class="text-sm text-ink">
                {t("releases.reportSummary", {
                  pairs: String(shown().pairs.length),
                  applied: String(
                    shown().pairs.filter((pair) => pair.status === "applied").length,
                  ),
                  pending: String(
                    shown().pairs.filter((pair) => pair.status === "pending").length,
                  ),
                  cancelled: String(
                    shown().pairs.filter((pair) => pair.status === "cancelled").length,
                  ),
                })}
              </p>
              <Show
                when={shown().pairs.length > 0}
                fallback={
                  <EmptyState
                    title={t("releases.noPairsTitle")}
                    description={t("releases.noPairsBody")}
                  />
                }
              >
                <ul class="flex flex-col gap-1">
                  <For each={shown().pairs}>{pairRow}</For>
                </ul>
              </Show>
            </Card>
          )}
        </Show>

        <FormPanel
          crud={drafting}
          createTitle={t("releases.draftTitle")}
          editTitle={t("releases.draftTitle")}
          submitLabel={t("releases.saveDraft")}
          onSubmit={saveDraft}
          dirty={() => name().trim() !== ""}
        >
          <TextField
            label={t("releases.name")}
            value={name()}
            onInput={setName}
            hint={t("releases.nameHint")}
          />
          <ComboboxField
            label={t("releases.targetGroup")}
            value={targetGroup()}
            options={groupOptions()}
            onChange={setTargetGroup}
            placeholder={t("releases.pickAGroup")}
            searchLabel={t("releases.searchGroups")}
            emptyLabel={t("picker.noMatch")}
            hint={t("releases.targetGroupHint")}
          />
          <TextField
            label={t("releases.wallDate")}
            value={wallDate()}
            onInput={setWallDate}
            type="date"
            hint={t("releases.wallDateHint")}
          />
          <Show when={wallDate()}>
            <TextField
              label={t("releases.wallTime")}
              value={wallTime()}
              onInput={setWallTime}
              type="time"
              hint={t("releases.wallTimeHint")}
            />
          </Show>
          <TextField
            label={t("releases.instant")}
            value={instantAt()}
            onInput={setInstantAt}
            type="datetime-local"
            hint={t("releases.instantHint")}
          />
        </FormPanel>

        <FormPanel
          crud={scheduling}
          createTitle={t("releases.scheduleTitle")}
          editTitle={t("releases.scheduleTitle")}
          submitLabel={t("releases.scheduleNow")}
          onSubmit={submitSchedule}
          dirty={() => nodes().length > 0}
        >
          <MultiComboboxField
            label={t("releases.nodes")}
            values={nodes()}
            options={nodeOptions()}
            onChange={setNodes}
            searchLabel={t("releases.searchNodes")}
            emptyLabel={t("picker.noMatch")}
            removeLabel={t("picker.remove")}
            hint={t("releases.nodesHint")}
          />
          <Show when={nodes().includes("menu")}>
            <ComboboxField
              label={t("releases.menu")}
              value={menuId()}
              options={menus()}
              onChange={setMenuId}
              placeholder={t("releases.pickAMenu")}
              searchLabel={t("releases.searchMenus")}
              emptyLabel={t("picker.noMatch")}
              hint={t("releases.menuHint")}
            />
          </Show>
          <Show
            when={scheduling.subject()?.target_group_id}
            fallback={
              <MultiComboboxField
                label={t("releases.stores")}
                values={explicitStores()}
                options={storeOptions()}
                onChange={setExplicitStores}
                searchLabel={t("releases.searchStores")}
                emptyLabel={t("picker.noMatch")}
                removeLabel={t("picker.remove")}
                hint={t("releases.storesHint")}
              />
            }
          >
            {(group) => (
              <p class="text-sm text-ink-muted">
                {t("releases.expandsCohort", { group: groupName(group()) })}
              </p>
            )}
          </Show>
        </FormPanel>
      </RequireContext>
    </div>
  );
}
