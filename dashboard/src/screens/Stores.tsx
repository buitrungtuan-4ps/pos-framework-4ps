// Stores & brands management (ADR-0065, WS-C), on the authoring kit
// ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)). The operator's place to give
// the backfilled placeholder stores (`Store 01J9…`) real names, create new stores and brands, and
// archive/restore — all by name, no ULID typed. Tenant-scoped to the picker's context.
//
// This is the screen the owner reported: it used to end in two permanently-visible cards, "Create
// store" and "Create brand", occupying the bottom half of the page whether or not anyone wanted to
// create anything. Both are now `FormPanel`s behind an Add button in the header of the list they add
// to, which is Stage 4 item 15 of `docs/cloud-admin-ux-plan.md`, four months after it was recorded
// as delivered.
//
// Three things changed beyond moving the forms, each because the kit made the better shape the
// easy one:
//
//   * **Renaming opens the panel** instead of swapping a raw `<input>` into the table cell. The cell
//     input had no `FormField`, so a refused rename had nowhere to appear.
//   * **A store's brand is part of the same save as its name.** It used to be a `<select>` in the
//     cell that fired a write on `change` — a single mis-click silently reassigned a shop, with no
//     confirmation and no undo.
//   * **Stores and brands hold separate lifecycles**, so saving a brand no longer disables the
//     stores table. That was one shared `busy()` across every button on the screen.

import { createSignal, For, Show } from "solid-js";
import { A } from "@solidjs/router";

import { api } from "../api/client";
import type { Brand, CreateApiKeyResponse, Store, Tenant } from "../api/types";
import { t } from "../i18n";
import { useEntityCrud } from "../lib/entity-crud";
import {
  downloadHandoff,
  HANDOFF_FILES,
  RECOVERY,
  type Recovery,
} from "../lib/handoff";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { screenHref } from "../state/screens";
import {
  Banner,
  Button,
  Card,
  PageHeader,
  SelectField,
  StatusBadge,
  TextField,
} from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  FormPanel,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";
import { apiMessage, withStaleReload } from "../lib/errors";
import { DEFAULT_BIND_PORT } from "../installers.mjs";
import type { InstallerValues } from "../installers.d.mts";

export function Stores() {
  const [stores, setStores] = createSignal<Store[] | null>(null);
  const [brands, setBrands] = createSignal<Brand[]>([]);
  // The tenant in context, read back from the registry so this screen holds its current name,
  // status, and the version any rename must present (production-readiness O2).
  const [tenant, setTenant] = createSignal<Tenant | null>(null);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);

  // One lifecycle per entity type, not one per screen (ADR-0121 §3). The tenant block gets one too:
  // it never opens a `FormPanel` — a single-record settings row is not a list, and a panel over one
  // field would be ceremony — but it uses `confirming` for the archive and `run` for the rename, so
  // its busy state is its own.
  const storeCrud = useEntityCrud<Store>();
  const brandCrud = useEntityCrud<Brand>();
  const tenantCrud = useEntityCrud<Tenant>();

  // The panel's draft fields. Seeded when a panel opens, read when it submits.
  const [storeName, setStoreName] = createSignal("");
  const [storeBrand, setStoreBrand] = createSignal("");
  const [brandName, setBrandName] = createSignal("");
  const [tenantName, setTenantName] = createSignal("");

  const load = async () => {
    setError("");
    setLoading(true);
    try {
      const [loadedStores, loadedBrands, loadedTenants] = await Promise.all([
        api.listStores(tenantId()),
        api.listBrands(tenantId()),
        api.listTenants(),
      ]);
      setStores(loadedStores);
      setBrands(loadedBrands);
      // The list is every tenant the super-admin can see; this screen is scoped to one, so it keeps
      // the row for the context and ignores the rest.
      const inContext = loadedTenants.find((row) => row.tenant_id === tenantId()) ?? null;
      setTenant(inContext);
      setTenantName(inContext?.name ?? "");
    } catch (caught) {
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    } finally {
      setLoading(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  /**
   * Wraps a write so a stale refusal reloads the table and comes back saying so.
   *
   * A `412` means somebody else saved this row while the form was open (ADR-0094). The refusal is
   * re-thrown (reworded) so `crud.run` still keeps the form open with the message beside the
   * refreshed table — which is the whole reason `run` does not close on failure.
   */
  const conditional = <T,>(write: () => Promise<T>) =>
    withStaleReload(write, load, t("stores.stale"));

  /** Reloads and says so, after a write that changed something. */
  const settled = async (message: string) => {
    toast.ok(message);
    await load();
  };

  // --- stores -----------------------------------------------------------------------------------

  const openCreateStore = () => {
    setStoreName("");
    setStoreBrand("");
    storeCrud.create();
  };

  const openEditStore = (row: Store) => {
    setStoreName(row.name);
    setStoreBrand(row.brand_id ?? "");
    storeCrud.edit(row);
  };

  const submitStore = () => {
    const name = storeName().trim();
    if (!name) {
      storeCrud.refuse(t("stores.nameRequired"));
      return;
    }
    const editingRow = storeCrud.subject();
    void storeCrud
      .run(() =>
        conditional(() =>
          editingRow
            ? api.updateStore(
                editingRow.store_id,
                tenantId(),
                { name, status: editingRow.status, brandId: storeBrand() || null },
                editingRow.etag,
              )
            : api.createStore(tenantId(), name, storeBrand() || undefined),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(editingRow ? t("stores.renamed") : t("stores.created"));
        }
      });
  };

  const setStoreStatus = (row: Store, status: "active" | "archived") =>
    storeCrud
      .run(() =>
        conditional(() =>
          api.updateStore(
            row.store_id,
            tenantId(),
            { name: row.name, status, brandId: row.brand_id },
            row.etag,
          ),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(status === "archived" ? t("stores.archived") : t("stores.restored"));
        }
      });

  // --- the replacement handoff ------------------------------------------------------------------
  // The wizard's last step emitted four files for a store it had just created, and nothing else in
  // the console emitted them ever again. That made the handoff a one-shot: the only way to get an
  // installer for an existing store was to create a second store. Which is survivable right up to
  // the day a shop's machine dies — the day it matters most, and the day nobody wants to be reading
  // the source of a generator to reconstruct a config file by hand.
  //
  // Deliberately not a new cloud route and deliberately not on the wizard. Every value the files
  // need is already on this screen (the registry row) or is a property of the browser's own origin,
  // and the one write it makes — a fresh scoped key — is a route that has existed since WS-C. The
  // whole feature is a `Drawer` over `src/lib/handoff.ts`.

  const [handoffStore, setHandoffStore] = createSignal<Store | null>(null);
  // A property of the *machine*, not of the store: it shapes the generated config and never reaches
  // the cloud, so it is asked for here rather than stored on the row. Blank takes the edge's default.
  const [handoffPort, setHandoffPort] = createSignal("");
  const [handoffKey, setHandoffKey] = createSignal<CreateApiKeyResponse | null>(null);
  // "I know this box already holds its key" — the deliberate way past the key step, so that the
  // files can be downloaded without a write. Distinct from "no key yet", which is the initial state
  // and is the one that must not silently produce a credential-less env file.
  const [handoffKeyless, setHandoffKeyless] = createSignal(false);
  const [handoffBusy, setHandoffBusy] = createSignal(false);
  const [handoffError, setHandoffError] = createSignal("");

  const openHandoff = (row: Store) => {
    setHandoffStore(row);
    setHandoffPort("");
    setHandoffKey(null);
    setHandoffKeyless(false);
    setHandoffError("");
  };

  const closeHandoff = () => setHandoffStore(null);

  /**
   * Issues the key the replacement box will present on `/sync`.
   *
   * Two scopes, and both are load-bearing: `read_config` is how the box collects its published
   * configuration, `relay_orders` is how it drains the cloud's order relay. Store-scoped rather than
   * tenant-wide because the store sync routes refuse a key that names another store or none (S1) —
   * a tenant-wide key here would hand the operator a credential the box cannot use.
   *
   * It does **not** revoke the dead machine's key. That is a separate, per-key decision on the API
   * keys screen, and doing it from here would mean guessing which of a store's keys belonged to the
   * machine that died — a guess that, wrong, takes a working store offline.
   */
  const issueHandoffKey = async () => {
    const row = handoffStore();
    if (!row) {
      return;
    }
    setHandoffError("");
    setHandoffBusy(true);
    try {
      setHandoffKey(await api.createApiKey(tenantId(), ["read_config", "relay_orders"], row.store_id));
      toast.ok(t("handoff.keyIssued"));
    } catch (caught) {
      setHandoffError(apiMessage(caught));
    } finally {
      setHandoffBusy(false);
    }
  };

  /**
   * The values every generated artifact is rendered from.
   *
   * `cloudUrl` and `cloudHost` come from the browser's own location rather than from configuration,
   * for the same reason the wizard does it: the console is served by `pos_cloud`, so the origin the
   * operator is looking at *is* the origin the store must dial. A configured value could disagree
   * with it, and the disagreement would only show up as a box that boots LAN-only.
   */
  const handoffValues = (): InstallerValues | null => {
    const row = handoffStore();
    if (!row) {
      return null;
    }
    return {
      storeName: row.name,
      storeId: row.store_id,
      tenantLabel: tenant()?.name ?? tenantId(),
      tenantId: tenantId(),
      cloudUrl: window.location.origin,
      cloudHost: window.location.hostname,
      bindPort: handoffPort(),
      key: handoffKey()?.token ?? null,
    };
  };

  /** Whether the operator has settled the key question, which is what unlocks the downloads. */
  const handoffReady = () => handoffKey() !== null || handoffKeyless();

  const recoveryTone = (recovery: Recovery) =>
    recovery === "regenerated" ? "active" : recovery === "reissued" ? "neutral" : "danger";

  const recoveryLabel = (recovery: Recovery) =>
    recovery === "regenerated"
      ? t("handoff.recoveryRegenerated")
      : recovery === "reissued"
        ? t("handoff.recoveryReissued")
        : t("handoff.recoveryLost");

  // --- brands (production-readiness O2) ---------------------------------------------------------
  // The same three verbs the stores table has had since WS-C, over the `PATCH /admin/brands/{id}`
  // route that shipped with it and had no caller: a brand could be created and never corrected.

  const openCreateBrand = () => {
    setBrandName("");
    brandCrud.create();
  };

  const openEditBrand = (row: Brand) => {
    setBrandName(row.name);
    brandCrud.edit(row);
  };

  const submitBrand = () => {
    const name = brandName().trim();
    if (!name) {
      brandCrud.refuse(t("stores.nameRequired"));
      return;
    }
    const editingRow = brandCrud.subject();
    void brandCrud
      .run(() =>
        conditional(() =>
          editingRow
            ? api.updateBrand(
                editingRow.brand_id,
                tenantId(),
                { name, status: editingRow.status },
                editingRow.etag,
              )
            : api.createBrand(tenantId(), name),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(editingRow ? t("stores.brandRenamed") : t("stores.brandCreated"));
        }
      });
  };

  const setBrandStatus = (row: Brand, status: "active" | "archived") =>
    brandCrud
      .run(() =>
        conditional(() =>
          api.updateBrand(row.brand_id, tenantId(), { name: row.name, status }, row.etag),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(status === "archived" ? t("stores.brandArchived") : t("stores.brandRestored"));
        }
      });

  // --- the tenant itself ------------------------------------------------------------------------
  // Renaming the organisation in context. Archiving it is offered too, because the route exists and
  // an org that closed should not linger as active — but it is the one action on this screen that
  // changes the context the operator is standing in, so it is behind a confirm that says so.

  const saveTenantRename = () => {
    const current = tenant();
    const name = tenantName().trim();
    if (!current) {
      return;
    }
    if (!name) {
      tenantCrud.refuse(t("stores.nameRequired"));
      return;
    }
    void tenantCrud
      .run(() =>
        conditional(() =>
          api.updateTenant(current.tenant_id, { name, status: current.status }, current.etag),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(t("stores.tenantRenamed"));
        }
      });
  };

  const setTenantStatus = (status: "active" | "archived") => {
    const current = tenant();
    if (!current) {
      return;
    }
    void tenantCrud
      .run(() =>
        conditional(() =>
          api.updateTenant(current.tenant_id, { name: current.name, status }, current.etag),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(status === "archived" ? t("stores.tenantArchived") : t("stores.tenantRestored"));
        }
      });
  };

  /** The brands a store can be assigned to. An archived brand is not offered for a new link. */
  const brandOptions = () =>
    brands()
      .filter((brand) => brand.status !== "archived")
      .map((brand) => ({ value: brand.brand_id, label: brand.name }));

  const brandColumns = (): Column<Brand>[] => [
    {
      key: "name",
      header: t("stores.brandName"),
      sortValue: (row) => row.name,
      cell: (row) => <span>{row.name}</span>,
    },
    {
      key: "status",
      header: t("stores.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.status === "archived" ? "archived" : "active"}
          label={row.status === "archived" ? t("status.archived") : t("status.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.brand_id}</TechnicalDetails>
      ),
    },
  ];

  const columns = (): Column<Store>[] => [
    {
      key: "name",
      header: t("stores.name"),
      sortValue: (row) => row.name,
      cell: (row) => <span>{row.name}</span>,
    },
    {
      key: "brand",
      header: t("stores.brand"),
      sortValue: (row) => brands().find((b) => b.brand_id === row.brand_id)?.name ?? "",
      // Read-only now. Reassigning happens in the edit panel, with the name, in one save — the cell
      // used to be a `<select>` that wrote on `change`, so a mis-click moved a shop to another brand
      // with no confirmation.
      cell: (row) => (
        <span class={row.brand_id ? "" : "text-ink-muted"}>
          {brands().find((brand) => brand.brand_id === row.brand_id)?.name ?? t("stores.noBrand")}
        </span>
      ),
    },
    {
      key: "status",
      header: t("stores.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.status === "archived" ? "archived" : "active"}
          label={row.status === "archived" ? t("status.archived") : t("status.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.store_id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("stores.title")} description={t("stores.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={tenant()}>
            {(current) => (
              <Card title={t("stores.organisation")}>
                <div class="flex flex-wrap items-end gap-4">
                  <div class="grow">
                    <TextField
                      label={t("stores.organisationName")}
                      value={tenantName()}
                      onInput={setTenantName}
                    />
                  </div>
                  <StatusBadge
                    tone={current().status === "archived" ? "archived" : "active"}
                    label={
                      current().status === "archived" ? t("status.archived") : t("status.active")
                    }
                  />
                  <Button
                    disabled={tenantCrud.saving() || tenantName().trim() === current().name}
                    onClick={saveTenantRename}
                  >
                    {tenantCrud.saving() ? t("common.saving") : t("action.save")}
                  </Button>
                  <Show
                    when={current().status === "archived"}
                    fallback={
                      <Button
                        variant="danger"
                        disabled={tenantCrud.saving()}
                        onClick={() => tenantCrud.confirm(current())}
                      >
                        {t("stores.archive")}
                      </Button>
                    }
                  >
                    <Button
                      variant="secondary"
                      disabled={tenantCrud.saving()}
                      onClick={() => void setTenantStatus("active")}
                    >
                      {t("stores.restore")}
                    </Button>
                  </Show>
                </div>
                <Show when={tenantCrud.error()}>
                  {(message) => (
                    <div class="mt-3">
                      <Banner tone="danger" message={message()} />
                    </div>
                  )}
                </Show>
                <div class="mt-3">
                  <TechnicalDetails label={t("common.technicalDetails")}>
                    {current().tenant_id}
                  </TechnicalDetails>
                </div>
              </Card>
            )}
          </Show>

          <Card
            title={t("stores.list")}
            actions={
              <div class="flex gap-2">
                {/* Add sits in the header of the list it adds to (ADR-0121 §6). This screen carries
                    three sections, so the section header is where it belongs; a single button on the
                    `PageHeader` could not say which list it meant. */}
                <Button onClick={openCreateStore}>{t("stores.create")}</Button>
                <A
                  href={screenHref("newStore", tenantId(), "")}
                  class="inline-flex min-h-touch items-center justify-center rounded-token border border-line bg-surface-raised px-4 text-base font-medium text-ink"
                >
                  {t("wizard.open")}
                </A>
                <Button variant="secondary" disabled={loading()} onClick={() => void load()}>
                  {t("action.refresh")}
                </Button>
              </div>
            }
          >
            <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
            <Show
              when={stores()}
              fallback={<p class="text-sm text-ink-muted">{t("stores.loadHint")}</p>}
            >
              {(loaded) => (
                <DataTable
                  columns={columns()}
                  rows={loaded()}
                  searchText={(row) => row.name}
                  pageSize={12}
                  empty={<EmptyState title={t("stores.empty")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <div class="flex flex-wrap gap-2">
                      <Button
                        variant="secondary"
                        disabled={storeCrud.saving()}
                        onClick={() => openEditStore(row)}
                      >
                        {t("action.edit")}
                      </Button>
                      <Button variant="secondary" onClick={() => openHandoff(row)}>
                        {t("handoff.open")}
                      </Button>
                      <Show
                        when={row.status === "archived"}
                        fallback={
                          <Button
                            variant="danger"
                            disabled={storeCrud.saving()}
                            onClick={() => storeCrud.confirm(row)}
                          >
                            {t("stores.archive")}
                          </Button>
                        }
                      >
                        <Button
                          variant="secondary"
                          disabled={storeCrud.saving()}
                          onClick={() => void setStoreStatus(row, "active")}
                        >
                          {t("stores.restore")}
                        </Button>
                      </Show>
                    </div>
                  )}
                />
              )}
            </Show>
          </Card>

          <Card
            title={t("stores.brands")}
            actions={<Button onClick={openCreateBrand}>{t("stores.createBrand")}</Button>}
          >
            <Show
              when={brands().length > 0}
              fallback={<EmptyState title={t("stores.noBrands")} />}
            >
              <DataTable
                columns={brandColumns()}
                rows={brands()}
                searchText={(row) => row.name}
                pageSize={12}
                empty={<EmptyState title={t("stores.noBrands")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex flex-wrap gap-2">
                    <Button
                      variant="secondary"
                      disabled={brandCrud.saving()}
                      onClick={() => openEditBrand(row)}
                    >
                      {t("action.edit")}
                    </Button>
                    <Show
                      when={row.status === "archived"}
                      fallback={
                        <Button
                          variant="danger"
                          disabled={brandCrud.saving()}
                          onClick={() => brandCrud.confirm(row)}
                        >
                          {t("stores.archive")}
                        </Button>
                      }
                    >
                      <Button
                        variant="secondary"
                        disabled={brandCrud.saving()}
                        onClick={() => void setBrandStatus(row, "active")}
                      >
                        {t("stores.restore")}
                      </Button>
                    </Show>
                  </div>
                )}
              />
            </Show>
          </Card>
        </div>

        <FormPanel
          crud={storeCrud}
          as="modal"
          createTitle={t("stores.create")}
          editTitle={t("stores.edit")}
          submitLabel={storeCrud.mode() === "editing" ? t("action.save") : t("action.create")}
          onSubmit={submitStore}
          dirty={() => storeName().trim() !== (storeCrud.subject()?.name ?? "")}
        >
          <TextField
            label={t("stores.name")}
            value={storeName()}
            onInput={setStoreName}
            placeholder={t("stores.namePlaceholder")}
          />
          <SelectField
            label={t("stores.brand")}
            value={storeBrand()}
            options={brandOptions()}
            onChange={setStoreBrand}
            placeholder={t("stores.noBrand")}
          />
        </FormPanel>

        <FormPanel
          crud={brandCrud}
          as="modal"
          createTitle={t("stores.createBrand")}
          editTitle={t("stores.editBrand")}
          submitLabel={brandCrud.mode() === "editing" ? t("action.save") : t("action.create")}
          onSubmit={submitBrand}
          dirty={() => brandName().trim() !== (brandCrud.subject()?.name ?? "")}
        >
          <TextField
            label={t("stores.brandName")}
            value={brandName()}
            onInput={setBrandName}
            placeholder={t("stores.brandNamePlaceholder")}
          />
        </FormPanel>

        {/* The replacement handoff. A `Drawer` rather than a `Modal` because it is a reading task
            first — four facts about what a dead machine takes with it — and only then four buttons;
            a modal that tall is a page pretending to be a dialog. */}
        <Drawer
          open={handoffStore() !== null}
          title={t("handoff.title")}
          closeLabel={t("action.close")}
          onClose={closeHandoff}
          footer={
            <Button variant="secondary" onClick={closeHandoff}>
              {t("action.close")}
            </Button>
          }
        >
          <Show when={handoffStore()}>
            {(row) => (
              <div class="flex flex-col gap-5">
                <p class="text-sm text-ink-muted">
                  {t("handoff.intro", { store: row().name })}
                </p>
                <Show when={handoffError()}>
                  {(message) => <Banner tone="danger" message={message()} />}
                </Show>

                {/* What survives the machine. First, because the two rows that need an action are
                    the ones an operator does not know to look for — the device credential above
                    all: a replacement box installs and boots and then will not sell, and nothing
                    else on this screen would ever have said why. */}
                <section class="flex flex-col gap-3">
                  <h3 class="text-sm font-semibold text-ink">{t("handoff.factsTitle")}</h3>
                  <ul class="flex flex-col gap-3">
                    <For each={RECOVERY}>
                      {(fact) => (
                        <li class="flex flex-col gap-1 border-l-2 border-line pl-3">
                          <div class="flex flex-wrap items-center gap-2">
                            <span class="text-sm font-medium text-ink">{t(fact.labelKey)}</span>
                            <StatusBadge
                              tone={recoveryTone(fact.recovery)}
                              label={recoveryLabel(fact.recovery)}
                            />
                          </div>
                          <p class="text-sm text-ink-muted">{t(fact.actionKey)}</p>
                        </li>
                      )}
                    </For>
                  </ul>
                </section>

                {/* The key. A write, so it is a button the operator presses rather than something
                    that happens on open — opening this drawer to read the facts must not mint a
                    credential. */}
                <section class="flex flex-col gap-2">
                  <h3 class="text-sm font-semibold text-ink">{t("handoff.keyTitle")}</h3>
                  <p class="text-sm text-ink-muted">{t("handoff.keyHint")}</p>
                  <Show
                    when={handoffKey()}
                    fallback={
                      <div class="flex flex-col gap-2">
                        <div class="flex flex-wrap gap-2">
                          <Button disabled={handoffBusy()} onClick={() => void issueHandoffKey()}>
                            {handoffBusy() ? t("common.saving") : t("handoff.issueKey")}
                          </Button>
                          <Button
                            variant="secondary"
                            disabled={handoffBusy() || handoffKeyless()}
                            onClick={() => setHandoffKeyless(true)}
                          >
                            {t("wizard.skipKey")}
                          </Button>
                        </div>
                        <Show when={handoffKeyless()}>
                          <Banner tone="danger" message={t("handoff.withoutKey")} />
                        </Show>
                      </div>
                    }
                  >
                    {(key) => (
                      <>
                        <Banner tone="ok" message={t("handoff.keyIssued")} />
                        <div class="break-all rounded-token border border-line bg-surface-raised p-3 font-mono text-xs text-ink">
                          {key().token}
                        </div>
                        <TechnicalDetails label={t("common.technicalDetails")}>
                          {key().id}
                        </TechnicalDetails>
                      </>
                    )}
                  </Show>
                </section>

                {/* The files, behind the key decision. Not because a credential-less handoff is
                    invalid — a box that already holds its key is a real case — but because the
                    silent version of it is an `env` file with no credential in it, which produces a
                    store that trades and never syncs. Choosing is cheap; discovering is not. */}
                <Show
                  when={handoffReady()}
                  fallback={<p class="text-sm text-ink-muted">{t("handoff.keyHint")}</p>}
                >
                  <section class="flex flex-col gap-3">
                    <h3 class="text-sm font-semibold text-ink">{t("handoff.filesTitle")}</h3>
                    <TextField
                      label={t("wizard.bindPort")}
                      value={handoffPort()}
                      onInput={setHandoffPort}
                      placeholder={DEFAULT_BIND_PORT}
                    />
                    <p class="text-sm text-ink-muted">{t("wizard.bindPortHint")}</p>
                    <Show when={handoffKey()}>
                      <Banner tone="danger" message={t("handoff.secretWarning")} />
                    </Show>
                    <For each={HANDOFF_FILES}>
                      {(file) => (
                        <div class="flex flex-col gap-1">
                          <Button
                            variant="secondary"
                            onClick={() => {
                              const values = handoffValues();
                              if (values) {
                                downloadHandoff(file, values);
                              }
                            }}
                          >
                            {t(file.labelKey)}
                          </Button>
                          <p class="text-sm text-ink-muted">{t(file.hintKey)}</p>
                        </div>
                      )}
                    </For>
                  </section>

                  <section class="flex flex-col gap-2">
                    <h3 class="text-sm font-semibold text-ink">{t("handoff.stepsTitle")}</h3>
                    <ol class="flex list-decimal flex-col gap-2 pl-5 text-sm text-ink-muted">
                      <li>{t("handoff.step1")}</li>
                      <li>{t("handoff.step2")}</li>
                      <li>{t("handoff.step3")}</li>
                      <li>{t("handoff.step4")}</li>
                      <li>{t("handoff.step5")}</li>
                    </ol>
                    <div class="flex flex-wrap gap-2">
                      <A
                        href={screenHref("activation", tenantId(), row().store_id)}
                        class="inline-flex min-h-touch items-center justify-center rounded-token border border-line bg-surface-raised px-4 text-base font-medium text-ink"
                      >
                        {t("handoff.goActivation")}
                      </A>
                      <A
                        href={screenHref("apiKeys", tenantId(), row().store_id)}
                        class="inline-flex min-h-touch items-center justify-center rounded-token border border-line bg-surface-raised px-4 text-base font-medium text-ink"
                      >
                        {t("handoff.goApiKeys")}
                      </A>
                    </div>
                  </section>
                </Show>
              </div>
            )}
          </Show>
        </Drawer>

        <ConfirmDialog
          open={storeCrud.mode() === "confirming"}
          title={t("stores.archiveTitle")}
          message={t("stores.archiveMessage")}
          confirmLabel={t("stores.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={storeCrud.saving()}
          onConfirm={() => {
            const row = storeCrud.subject();
            if (row) {
              void setStoreStatus(row, "archived");
            }
          }}
          onCancel={storeCrud.close}
        />

        <ConfirmDialog
          open={brandCrud.mode() === "confirming"}
          title={t("stores.brandArchiveTitle")}
          message={t("stores.brandArchiveMessage")}
          confirmLabel={t("stores.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={brandCrud.saving()}
          onConfirm={() => {
            const row = brandCrud.subject();
            if (row) {
              void setBrandStatus(row, "archived");
            }
          }}
          onCancel={brandCrud.close}
        />

        <ConfirmDialog
          open={tenantCrud.mode() === "confirming"}
          title={t("stores.tenantArchiveTitle")}
          message={t("stores.tenantArchiveMessage")}
          confirmLabel={t("stores.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={tenantCrud.saving()}
          onConfirm={() => void setTenantStatus("archived")}
          onCancel={tenantCrud.close}
        />
      </RequireContext>
    </div>
  );
}
