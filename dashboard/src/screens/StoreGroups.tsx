// Store groups and the batch publish
// ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md)), on the authoring kit
// ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)).
//
// # What this screen is for
//
// The data a tenant authors is already shared — one price row prices an item for the whole estate.
// What was per-store is *delivery*: every publish took a `store_id`, so pushing one menu to fifty
// shops was fifty publishes done by hand. A group is the cohort you publish to instead. It is not
// the brand: a store belongs to exactly one brand and to any number of groups, because the sets an
// operator publishes to cut across identity — the airport branches on a reduced menu, the shops on
// one tax registration, the pilots that take a change first.
//
// # Why the report is the point, not the button
//
// A batch is deliberately not atomic (§5): each store's config tree is its own row, and an
// all-or-nothing publish fails as "nothing happened, at 200 shops, because of one". So the screen
// leads with the per-store report — three outcomes, never two, because a shop that was
// *deliberately protected* (`skipped`) must not be filed beside one that broke (`failed`).
//
// Everything here is operational metadata: cohort names, store ids, and what a publish did. No
// customer or employee identifier passes through this screen.

import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import type {
  BatchOutcome,
  ConfigBatchReport,
  Json,
  Menu,
  QrGuardrails,
  Store,
  StoreGroup,
} from "../api/types";
import { type MessageKey, t } from "../i18n";
import { apiMessage, withStaleReload } from "../lib/errors";
import { onScopedContext, RequireContext } from "../lib/scoped";
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
 * How a node kind gets its arguments — the shape of the second question the operator is asked.
 *
 * `authored` nodes are recompiled from what the tenant already holds, so there is nothing to ask.
 * `menu` needs to know *which* menu. The six `copy` nodes are settings an operator types per store,
 * and the console does not re-implement their editors here: it asks which store to take the
 * setting from and publishes that store's current value to the cohort. That is also the honest
 * mental model — "make the rest of the group look like this one".
 */
type ArgumentSource = "authored" | "menu" | "copy";

/** One batchable node kind (ADR-0122 §4) as the picker offers it. */
interface NodeKind {
  readonly key: string;
  readonly label: MessageKey;
  readonly source: ArgumentSource;
}

/**
 * The thirteen, in the order an operator reaches for them.
 *
 * `locale` and `store_profile` are absent and stay absent: they are per-store by definition
 * (ADR-0114, ADR-0106), and so is the generic level write, whose `If-Match` only means something
 * for one store. The server refuses them by the same list, so this is not the only guard.
 */
const MENU_NODE: NodeKind = { key: "menu", label: "storeGroups.node.menu", source: "menu" };

const NODE_KINDS: readonly NodeKind[] = [
  MENU_NODE,
  { key: "tax", label: "storeGroups.node.tax", source: "authored" },
  { key: "campaigns", label: "storeGroups.node.campaigns", source: "authored" },
  { key: "inventory", label: "storeGroups.node.inventory", source: "authored" },
  { key: "reason_codes", label: "storeGroups.node.reasonCodes", source: "authored" },
  { key: "permissions", label: "storeGroups.node.permissions", source: "authored" },
  { key: "floor", label: "storeGroups.node.floor", source: "authored" },
  { key: "capabilities", label: "storeGroups.node.capabilities", source: "copy" },
  { key: "channels", label: "storeGroups.node.channels", source: "copy" },
  { key: "tender", label: "storeGroups.node.tender", source: "copy" },
  { key: "origins", label: "storeGroups.node.origins", source: "copy" },
  { key: "qr", label: "storeGroups.node.qr", source: "copy" },
  { key: "vendor_policies", label: "storeGroups.node.vendorPolicies", source: "copy" },
];

/** How each outcome is drawn. `skipped` is neutral on purpose: it is not a failure. */
const OUTCOME_TONE: Record<BatchOutcome, "active" | "archived" | "danger"> = {
  applied: "active",
  skipped: "archived",
  failed: "danger",
};

const OUTCOME_LABEL: Record<BatchOutcome, MessageKey> = {
  applied: "storeGroups.outcome.applied",
  skipped: "storeGroups.outcome.skipped",
  failed: "storeGroups.outcome.failed",
};

/**
 * Reads one store's current value for a `copy` node and shapes it as that node's batch arguments,
 * or `null` when that store has never published it.
 *
 * `null` rather than a thrown error, because "nothing to copy" is an *answer* the form renders in
 * the operator's own words — and because publishing it anyway would fan an empty document out to
 * the whole cohort, which is never what was meant.
 */
async function copyArguments(
  node: string,
  tenant: string,
  store: string,
  capabilityKeys: readonly string[],
): Promise<Json | null> {
  if (node === "channels") {
    const read = await api.readChannels(tenant, store);
    if (!read) {
      return null;
    }
    return { enabled: read.enabled };
  }
  if (node === "tender") {
    const read = await api.readTender(tenant, store);
    if (!read) {
      return null;
    }
    return { accepted: read.accepted };
  }
  if (node === "origins") {
    const read = await api.readOrigins(tenant, store);
    if (!read) {
      return null;
    }
    return { allowed: read.allowed };
  }
  if (node === "vendor_policies") {
    const read = await api.readVendorPolicies(tenant, store);
    if (!read) {
      return null;
    }
    return { policies: read.policies } as unknown as Json;
  }
  if (node === "qr") {
    const read: QrGuardrails | null = await api.readQrGuardrails(tenant, store);
    if (!read) {
      return null;
    }
    return {
      enabled: read.enabled,
      staff_confirmation_required: read.staff_confirmation_required,
      per_table_limit: read.per_table_limit,
      rate_window_secs: read.rate_window_secs,
      ...(read.business_hours ? { business_hours: read.business_hours } : {}),
    } as unknown as Json;
  }
  // `capabilities`: the flags live as top-level booleans on the store's effective document, so the
  // catalogue is what says which of its keys are flags rather than nodes.
  const document = await api.effectiveConfig(tenant, store);
  if (!document || typeof document !== "object" || Array.isArray(document)) {
    return null;
  }
  const flags: Record<string, boolean> = {};
  for (const key of capabilityKeys) {
    const value = (document as Record<string, Json>)[key];
    if (typeof value === "boolean") {
      flags[key] = value;
    }
  }
  if (Object.keys(flags).length === 0) {
    return null;
  }
  return { flags };
}

export function StoreGroups() {
  const [rows, setRows] = createSignal<StoreGroup[] | null>(null);
  const [stores, setStores] = createSignal<Store[]>([]);
  const [menus, setMenus] = createSignal<Menu[]>([]);
  const [capabilityKeys, setCapabilityKeys] = createSignal<string[]>([]);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);

  // Four independent lifecycles (ADR-0121 §3), so editing a cohort's name does not grey out the
  // publish, and one slow publish does not disable the membership editor.
  const editor = useEntityCrud<StoreGroup>();
  const membership = useEntityCrud<StoreGroup>();
  const publishing = useEntityCrud<never>();
  const history = useEntityCrud<StoreGroup>();

  const [name, setName] = createSignal("");
  const [members, setMembers] = createSignal<string[]>([]);
  // Which cohort the publish panel is aimed at, and what it will send.
  const [target, setTarget] = createSignal("");
  const [node, setNode] = createSignal("menu");
  const [menuId, setMenuId] = createSignal("");
  const [sourceStore, setSourceStore] = createSignal("");
  // The last batch this screen ran or read back. The deliverable, not the button.
  const [report, setReport] = createSignal<ConfigBatchReport | null>(null);
  const [batches, setBatches] = createSignal<ConfigBatchReport[]>([]);

  const load = async () => {
    setError("");
    setLoading(true);
    try {
      const [groups, storeRows] = await Promise.all([
        api.listStoreGroups(tenantId()),
        api.listStores(tenantId()),
      ]);
      setRows(groups);
      setStores(storeRows);
    } catch (caught) {
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    } finally {
      setLoading(false);
    }
  };

  // The menu list and the capability catalogue are only needed once the publish panel is open, and
  // a failure to load either is not a reason to hide the cohorts — so they load quietly beside the
  // main read rather than inside it.
  const loadPublishOptions = () => {
    void api
      .listMenus(tenantId())
      .then(setMenus)
      .catch(() => setMenus([]));
    void api
      .capabilityCatalogue()
      .then((catalogue) => setCapabilityKeys(catalogue.flags.map((flag) => flag.key)))
      .catch(() => setCapabilityKeys([]));
  };

  const conditional = <T,>(write: () => Promise<T>) =>
    withStaleReload(write, load, t("storeGroups.stale"));

  onScopedContext("tenant", () => {
    void load();
    loadPublishOptions();
  });

  const storeName = (id: string) => stores().find((row) => row.store_id === id)?.name ?? id;

  const storeOptions = () =>
    stores()
      .filter((row) => row.status === "active")
      .map((row) => ({ value: row.store_id, label: row.name, keywords: [row.store_id] }));

  const openCreate = () => {
    setName("");
    editor.create();
  };

  const openEdit = (row: StoreGroup) => {
    setName(row.name);
    editor.edit(row);
  };

  const openMembers = (row: StoreGroup) => {
    setMembers([...row.store_ids]);
    membership.edit(row);
  };

  const save = () => {
    const label = name().trim();
    if (!label) {
      editor.refuse(t("storeGroups.nameRequired"));
      return;
    }
    const subject = editor.subject();
    void editor
      .run(() =>
        conditional(() =>
          subject
            ? api.updateStoreGroup(
                tenantId(),
                subject.group_id,
                { name: label, status: subject.status },
                subject.etag,
              )
            : api.createStoreGroup(tenantId(), label),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(t("storeGroups.saved"));
          void load();
        }
      });
  };

  const saveMembers = () => {
    const subject = membership.subject();
    if (!subject) {
      return;
    }
    void membership
      .run(() =>
        conditional(() =>
          api.setStoreGroupMembers(tenantId(), subject.group_id, members(), subject.etag),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(t("storeGroups.membersSaved", { count: String(members().length) }));
          void load();
        }
      });
  };

  /** Archive or restore in place — a field, so it goes through the same version check. */
  const setStanding = (row: StoreGroup, active: boolean) => {
    void editor
      .run(() =>
        conditional(() =>
          api.updateStoreGroup(
            tenantId(),
            row.group_id,
            { name: row.name, status: active ? "active" : "archived" },
            row.etag,
          ),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(active ? t("storeGroups.restored") : t("storeGroups.archived"));
          void load();
        } else {
          setError(editor.error());
          toast.error(editor.error());
        }
      });
  };

  const chosenNode = (): NodeKind =>
    NODE_KINDS.find((kind) => kind.key === node()) ?? MENU_NODE;

  const publish = () => {
    const groupId = target();
    const kind = chosenNode();
    if (!groupId) {
      publishing.refuse(t("storeGroups.pickAGroup"));
      return;
    }
    if (kind.source === "menu" && !menuId()) {
      publishing.refuse(t("storeGroups.pickAMenu"));
      return;
    }
    if (kind.source === "copy" && !sourceStore()) {
      publishing.refuse(t("storeGroups.pickASource"));
      return;
    }
    setReport(null);
    void (async () => {
      // The copy read happens *before* the batch starts, so a source with nothing to copy is a
      // sentence in the form rather than a fan-out that wrote an empty document to every member.
      let args: Json = {} as unknown as Json;
      if (kind.source === "menu") {
        args = { menu_id: menuId() } as unknown as Json;
      } else if (kind.source === "copy") {
        let copied: Json | null;
        try {
          copied = await copyArguments(kind.key, tenantId(), sourceStore(), capabilityKeys());
        } catch (caught) {
          publishing.refuse(apiMessage(caught));
          return;
        }
        if (copied === null) {
          publishing.refuse(t("storeGroups.sourceHasNoNode"));
          return;
        }
        args = copied;
      }
      await runPublish(groupId, kind.key, args);
    })();
  };

  /** The batch itself, once the arguments are settled. */
  const runPublish = (groupId: string, key: string, args: Json) =>
    publishing
      .run(async () => {
        setReport(await api.publishToStoreGroup(tenantId(), groupId, key, args));
      })
      .then((ok) => {
        const ran = report();
        if (ok && ran) {
          // A `200` is not "every store succeeded" — say what actually happened.
          toast.ok(
            t("storeGroups.batchRan", {
              applied: String(ran.applied),
              skipped: String(ran.skipped),
              failed: String(ran.failed),
            }),
          );
        } else {
          toast.error(publishing.error());
        }
      });

  const openHistory = (row: StoreGroup) => {
    setBatches([]);
    history.edit(row);
    void api
      .listStoreGroupBatches(tenantId(), row.group_id)
      .then(setBatches)
      .catch((caught: unknown) => {
        const message = apiMessage(caught);
        setError(message);
        toast.error(message);
      });
  };

  const columns = (): Column<StoreGroup>[] => [
    {
      key: "name",
      header: t("storeGroups.name"),
      cell: (row) => row.name,
      sortValue: (row) => row.name,
    },
    {
      key: "members",
      header: t("storeGroups.members"),
      cell: (row) => (
        <div>
          <div>{t("storeGroups.memberCount", { count: String(row.store_ids.length) })}</div>
          <Show when={row.store_ids.length > 0}>
            <div class="text-sm text-ink-muted">
              {row.store_ids
                .slice(0, 3)
                .map((id) => storeName(id))
                .join(", ")}
              {row.store_ids.length > 3 ? "…" : ""}
            </div>
          </Show>
        </div>
      ),
      sortValue: (row) => row.store_ids.length,
    },
    {
      key: "standing",
      header: t("storeGroups.standing"),
      cell: (row) => (
        <StatusBadge
          tone={row.status === "active" ? "active" : "archived"}
          label={row.status === "active" ? t("storeGroups.active") : t("storeGroups.archived")}
        />
      ),
    },
    {
      key: "id",
      header: t("storeGroups.id"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.group_id}</TechnicalDetails>
      ),
    },
  ];

  const reportRows = (shown: ConfigBatchReport) => (
    <div class="flex flex-col gap-2">
      <p class="text-sm text-ink">
        {t("storeGroups.reportSummary", {
          node: shown.node,
          applied: String(shown.applied),
          skipped: String(shown.skipped),
          failed: String(shown.failed),
        })}
      </p>
      <ul class="flex flex-col gap-1">
        <For each={shown.results}>
          {(row) => (
            <li class="flex flex-wrap items-center gap-2 rounded-token border border-line bg-surface-raised px-3 py-2">
              <StatusBadge
                tone={OUTCOME_TONE[row.outcome]}
                label={t(OUTCOME_LABEL[row.outcome])}
              />
              <span class="text-sm text-ink">{storeName(row.store_id)}</span>
              <Show when={row.detail}>
                {(detail) => <span class="text-sm text-ink-muted">{detail()}</span>}
              </Show>
              <Show when={row.config_version_id}>
                {(version) => (
                  <TechnicalDetails label={t("common.technicalDetails")}>
                    {version()}
                  </TechnicalDetails>
                )}
              </Show>
            </li>
          )}
        </For>
      </ul>
    </div>
  );

  return (
    <div>
      <PageHeader title={t("storeGroups.title")} description={t("storeGroups.description")} />
      <RequireContext need="tenant">
        <Card
          title={t("storeGroups.list")}
          actions={
            <div class="flex gap-2">
              <Button onClick={openCreate}>{t("storeGroups.new")}</Button>
              <Button variant="secondary" disabled={loading()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            </div>
          }
        >
          <p class="mb-3 text-sm text-ink-muted">{t("storeGroups.listHint")}</p>
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={rows()}>
            {(loaded) => (
              <DataTable
                columns={columns()}
                rows={loaded()}
                searchText={(row) => row.name}
                pageSize={12}
                empty={
                  <EmptyState
                    title={t("storeGroups.empty")}
                    description={t("storeGroups.emptyHint")}
                  />
                }
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex flex-wrap gap-2">
                    <Button variant="secondary" onClick={() => openEdit(row)}>
                      {t("action.edit")}
                    </Button>
                    <Button variant="secondary" onClick={() => openMembers(row)}>
                      {t("storeGroups.editMembers")}
                    </Button>
                    <Button variant="secondary" onClick={() => openHistory(row)}>
                      {t("storeGroups.history")}
                    </Button>
                    <Button
                      variant="secondary"
                      onClick={() => setStanding(row, row.status !== "active")}
                    >
                      {row.status === "active"
                        ? t("storeGroups.archive")
                        : t("storeGroups.restore")}
                    </Button>
                  </div>
                )}
              />
            )}
          </Show>
        </Card>

        <Card title={t("storeGroups.publishTitle")}>
          <p class="mb-3 text-sm text-ink-muted">{t("storeGroups.publishHint")}</p>
          <div class="flex flex-col gap-4">
            <ComboboxField
              label={t("storeGroups.publishTo")}
              value={target()}
              options={(rows() ?? [])
                .filter((row) => row.status === "active")
                .map((row) => ({ value: row.group_id, label: row.name }))}
              onChange={setTarget}
              placeholder={t("storeGroups.pickAGroup")}
              searchLabel={t("storeGroups.searchGroups")}
              emptyLabel={t("picker.noMatch")}
            />
            <ComboboxField
              label={t("storeGroups.node")}
              value={node()}
              options={NODE_KINDS.map((kind) => ({ value: kind.key, label: t(kind.label) }))}
              onChange={(next) => {
                setNode(next);
                setReport(null);
              }}
              placeholder={t("storeGroups.pickANode")}
              searchLabel={t("storeGroups.searchNodes")}
              emptyLabel={t("picker.noMatch")}
              hint={t("storeGroups.nodeHint")}
            />
            <Show when={chosenNode().source === "menu"}>
              <ComboboxField
                label={t("storeGroups.menu")}
                value={menuId()}
                options={menus().map((menu) => ({ value: menu.menu_id, label: menu.name }))}
                onChange={setMenuId}
                placeholder={t("storeGroups.pickAMenu")}
                searchLabel={t("storeGroups.searchMenus")}
                emptyLabel={t("picker.noMatch")}
                hint={t("storeGroups.menuPrerequisite")}
              />
            </Show>
            <Show when={chosenNode().source === "copy"}>
              <ComboboxField
                label={t("storeGroups.copyFrom")}
                value={sourceStore()}
                options={storeOptions()}
                onChange={setSourceStore}
                placeholder={t("storeGroups.pickASource")}
                searchLabel={t("storeGroups.searchStores")}
                emptyLabel={t("picker.noMatch")}
                hint={t("storeGroups.copyFromHint")}
              />
            </Show>
            <Show when={chosenNode().source === "authored"}>
              <p class="text-sm text-ink-muted">{t("storeGroups.authoredHint")}</p>
            </Show>
            <Show when={publishing.error()}>
              {(message) => <Banner tone="danger" message={message()} />}
            </Show>
            <div>
              <Button disabled={publishing.saving()} onClick={publish}>
                {t("storeGroups.publish")}
              </Button>
            </div>
            <Show when={report()}>
              {(shown) => (
                <div class="border-t border-line pt-4">
                  <h3 class="mb-2 text-sm font-medium text-ink">{t("storeGroups.report")}</h3>
                  {reportRows(shown())}
                </div>
              )}
            </Show>
          </div>
        </Card>

        <FormPanel
          crud={editor}
          createTitle={t("storeGroups.new")}
          editTitle={t("storeGroups.edit")}
          submitLabel={t("action.save")}
          onSubmit={save}
          dirty={() => name() !== ""}
        >
          <div class="flex flex-col gap-4">
            <TextField
              label={t("storeGroups.name")}
              value={name()}
              onInput={setName}
              placeholder={t("storeGroups.namePlaceholder")}
            />
            <p class="text-sm text-ink-muted">{t("storeGroups.nameHint")}</p>
          </div>
        </FormPanel>

        <FormPanel
          crud={membership}
          createTitle={t("storeGroups.editMembers")}
          editTitle={t("storeGroups.editMembers")}
          submitLabel={t("action.save")}
          onSubmit={saveMembers}
          dirty={() => members().length > 0}
        >
          <div class="flex flex-col gap-4">
            <MultiComboboxField
              label={t("storeGroups.members")}
              values={members()}
              options={storeOptions()}
              onChange={setMembers}
              searchLabel={t("storeGroups.searchStores")}
              emptyLabel={t("picker.noMatch")}
              removeLabel={t("picker.remove")}
              hint={t("storeGroups.membersHint")}
            />
          </div>
        </FormPanel>

        <FormPanel
          crud={history}
          createTitle={t("storeGroups.history")}
          editTitle={t("storeGroups.history")}
          submitLabel={t("action.close")}
          onSubmit={() => history.close()}
          dirty={() => false}
        >
          <div class="flex flex-col gap-4">
            <p class="text-sm text-ink-muted">{t("storeGroups.historyHint")}</p>
            <Show
              when={batches().length > 0}
              fallback={<p class="text-sm text-ink-muted">{t("storeGroups.noBatches")}</p>}
            >
              <For each={batches()}>
                {(batch) => (
                  <div class="border-t border-line pt-3">{reportRows(batch)}</div>
                )}
              </For>
            </Show>
          </div>
        </FormPanel>
      </RequireContext>
    </div>
  );
}
