// The reason-code authoring screen (ADR-0115, roadmap B2.2), on the authoring kit
// ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)). An operator authors
// the tenant's managed list — the handle a report groups by, the name staff read on the picker, its
// Vietnamese name, and the actions it may be cited for — and the till then refuses a void, discount,
// comp, refund, drawer opening, staff rejection, cash movement or stock correction that does not
// cite one of them. `docs/pos-spec.md` §11 item 2 makes this a fraud control, so the screen leads
// with **retire** and puts delete behind a type-to-confirm: a historic `sales.order_line.voided`
// names the id forever, and a deleted row turns that event's reason into an unreadable ULID.
//
// Everything here is reference data — a code, a name, and the actions it applies to. No customer or
// employee identifier passes through this screen.

import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import {
  REASON_ACTIONS,
  type ReasonAction,
  type ReasonCode,
  type ReasonCodeInput,
} from "../api/types";
import { type MessageKey, t } from "../i18n";
import { apiMessage, withStaleReload } from "../lib/errors";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  PageHeader,
  StatusBadge,
  TextField,
} from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  FormPanel,
  TechnicalDetails,
} from "../components/kit";
import { useEntityCrud } from "../lib/entity-crud";
import { toast } from "../components/Toast";

/** The action tokens mapped to their labels, so the picker names the act rather than the token. */
const ACTION_LABEL: Record<ReasonAction, MessageKey> = {
  REASON_ACTION_VOID_LINE: "reasonCodes.action.voidLine",
  REASON_ACTION_VOID_BILL: "reasonCodes.action.voidBill",
  REASON_ACTION_DISCOUNT: "reasonCodes.action.discount",
  REASON_ACTION_COMP: "reasonCodes.action.comp",
  REASON_ACTION_REFUND: "reasonCodes.action.refund",
  REASON_ACTION_DRAWER_OPEN: "reasonCodes.action.drawerOpen",
  REASON_ACTION_REJECT_ORDER: "reasonCodes.action.rejectOrder",
  REASON_ACTION_CASH_PAID_IN: "reasonCodes.action.cashPaidIn",
  REASON_ACTION_CASH_PAID_OUT: "reasonCodes.action.cashPaidOut",
  REASON_ACTION_STOCK_ADJUSTMENT: "reasonCodes.action.stockAdjustment",
  REASON_ACTION_STOCK_WASTE: "reasonCodes.action.stockWaste",
};

export function ReasonCodes() {
  const [rows, setRows] = createSignal<ReasonCode[] | null>(null);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);

  // Three independent lifecycles, so publishing does not disable the editor and retiring one row
  // does not grey out the rest of the table (ADR-0121 §3). The editor's subject is the whole row,
  // which is how the version an update is conditional on (ADR-0095) travels with the id rather than
  // in a second signal that could drift out of step with it.
  const editor = useEntityCrud<ReasonCode>();
  const deletion = useEntityCrud<ReasonCode>();
  const publishing = useEntityCrud<never>();
  // Which row's standing is being flipped, so one slow retire disables that row's button only.
  const [flipping, setFlipping] = createSignal("");
  const [code, setCode] = createSignal("");
  const [name, setName] = createSignal("");
  const [nameVi, setNameVi] = createSignal("");
  const [actions, setActions] = createSignal<ReasonAction[]>([]);
  const [active, setActive] = createSignal(true);
  // What the last publish reported: the acts this store can no longer record, because the list it
  // now holds offers no reason for them. Empty is the healthy answer; null means it has not run.
  const [uncovered, setUncovered] = createSignal<ReasonAction[] | null>(null);

  const load = async () => {
    setError("");
    setLoading(true);
    try {
      setRows(await api.listReasonCodes(tenantId()));
    } catch (caught) {
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    } finally {
      setLoading(false);
    }
  };

  // A conditional write can be refused because somebody else saved first (ADR-0094's `412`). Every
  // edit here sends an `etag`, so the screen invites that refusal and owes the reader a sentence
  // naming what changed plus a reload; `withStaleReload` does both and hands the refusal back for
  // `run` to keep beside the form.
  const conditional = <T,>(write: () => Promise<T>) =>
    withStaleReload(write, load, t("reasonCodes.stale"));

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const openCreate = () => {
    setCode("");
    setName("");
    setNameVi("");
    setActions([]);
    setActive(true);
    editor.create();
  };

  const openEdit = (row: ReasonCode) => {
    setCode(row.code);
    setName(row.display_name);
    setNameVi(row.display_name_translations?.vi ?? "");
    setActions([...row.applies_to]);
    setActive(row.active);
    editor.edit(row);
  };

  const toggleAction = (action: ReasonAction, on: boolean) => {
    setActions((prev) =>
      on ? (prev.includes(action) ? prev : [...prev, action]) : prev.filter((a) => a !== action),
    );
  };

  const save = () => {
    const handle = code().trim();
    const label = name().trim();
    if (!handle || !label) {
      editor.refuse(t("reasonCodes.codeAndNameRequired"));
      return;
    }
    // The server refuses this too; refusing here names the field while the operator is still in the
    // form, rather than after a round trip. It goes through `refuse` rather than the page banner so
    // the sentence lands *inside* the form the operator is looking at (ADR-0121 §4).
    if (actions().length === 0) {
      editor.refuse(t("reasonCodes.actionRequired"));
      return;
    }
    const vi = nameVi().trim();
    const input: ReasonCodeInput = {
      code: handle,
      display_name: label,
      display_name_translations: vi ? { vi } : {},
      applies_to: actions(),
      active: active(),
    };
    const target = editor.subject();
    void editor
      .run(() =>
        conditional(() =>
          target
            ? api.updateReasonCode(tenantId(), target.id, target.etag, input)
            : api.createReasonCode(tenantId(), input),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(t("reasonCodes.saved"));
          void load();
        }
      });
  };

  /** Retire or restore in place: `active` is a field, so it goes through the same version check. */
  const setStanding = async (row: ReasonCode, standing: boolean) => {
    setError("");
    setFlipping(row.id);
    try {
      await conditional(() =>
        api.updateReasonCode(tenantId(), row.id, row.etag, {
          code: row.code,
          display_name: row.display_name,
          display_name_translations: row.display_name_translations ?? {},
          applies_to: row.applies_to,
          active: standing,
        }),
      );
      toast.ok(standing ? t("reasonCodes.restored") : t("reasonCodes.retired"));
      await load();
    } catch (caught) {
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    } finally {
      setFlipping("");
    }
  };

  const remove = () => {
    const row = deletion.subject();
    if (!row) {
      return;
    }
    void deletion.run(() => api.deleteReasonCode(tenantId(), row.id)).then((deleted) => {
      if (deleted) {
        toast.ok(t("reasonCodes.deleted"));
        void load();
      } else {
        // The confirm stays open carrying the refusal; a page banner would say it twice.
        toast.error(deletion.error());
      }
    });
  };


  // Push the authored list onto the store in context.
  const publish = () => {
    void publishing
      .run(async () => {
        const result = await api.publishReasonCodes(tenantId(), storeId());
        setUncovered(result.uncovered_actions);
        toast.ok(t("reasonCodes.published", { count: String(result.active) }));
      })
      .then((published) => {
        if (!published) {
          setError(publishing.error());
          toast.error(publishing.error());
        }
      });
  };

  const columns = (): Column<ReasonCode>[] => [
    {
      key: "code",
      header: t("reasonCodes.code"),
      cell: (row) => <span class="font-mono text-sm">{row.code}</span>,
      sortValue: (row) => row.code,
    },
    {
      key: "name",
      header: t("reasonCodes.name"),
      cell: (row) => (
        <div>
          <div>{row.display_name}</div>
          <Show when={row.display_name_translations?.vi}>
            {(vi) => <div class="text-sm text-ink-muted">{vi()}</div>}
          </Show>
        </div>
      ),
      sortValue: (row) => row.display_name,
    },
    {
      key: "appliesTo",
      header: t("reasonCodes.appliesTo"),
      cell: (row) => (
        <span class="text-sm text-ink-muted">
          {row.applies_to.map((action) => t(ACTION_LABEL[action])).join(", ")}
        </span>
      ),
    },
    {
      key: "standing",
      header: t("reasonCodes.standing"),
      cell: (row) => (
        <StatusBadge
          tone={row.active ? "active" : "archived"}
          label={row.active ? t("reasonCodes.active") : t("reasonCodes.inactive")}
        />
      ),
    },
    {
      key: "id",
      header: t("reasonCodes.id"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("reasonCodes.title")} description={t("reasonCodes.description")} />
      <RequireContext need="tenant">
        <Card
          title={t("reasonCodes.list")}
          actions={
            <div class="flex gap-2">
              <Button onClick={openCreate}>{t("reasonCodes.new")}</Button>
              <Button variant="secondary" disabled={loading()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            </div>
          }
        >
          <p class="mb-3 text-sm text-ink-muted">{t("reasonCodes.retireHint")}</p>
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={rows()}>
            {(loaded) => (
              <DataTable
                columns={columns()}
                rows={loaded()}
                searchText={(row) => `${row.code} ${row.display_name}`}
                pageSize={12}
                empty={
                  <EmptyState
                    title={t("reasonCodes.empty")}
                    description={t("reasonCodes.emptyHint")}
                  />
                }
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex flex-wrap gap-2">
                    <Button variant="secondary" onClick={() => openEdit(row)}>
                      {t("action.edit")}
                    </Button>
                    <Button
                      variant="secondary"
                      disabled={flipping() === row.id}
                      onClick={() => void setStanding(row, !row.active)}
                    >
                      {row.active ? t("reasonCodes.retire") : t("reasonCodes.restore")}
                    </Button>
                    <Button variant="danger" onClick={() => deletion.confirm(row)}>
                      {t("action.delete")}
                    </Button>
                  </div>
                )}
              />
            )}
          </Show>
        </Card>


        <Card title={t("reasonCodes.publishTitle")}>
          <p class="mb-3 text-sm text-ink-muted">{t("reasonCodes.publishHint")}</p>
          <Show
            when={storeId()}
            fallback={<p class="text-sm text-ink-muted">{t("reasonCodes.publishNeedsStore")}</p>}
          >
            <div class="flex flex-col gap-3">
              <p class="text-sm text-ink">
                {t("reasonCodes.publishTo", { store: storeName() })}
              </p>
              <div>
                <Button disabled={publishing.saving()} onClick={publish}>
                  {t("reasonCodes.publish")}
                </Button>
              </div>
              <Show when={uncovered()}>
                {(gaps) => (
                  <Show
                    when={gaps().length > 0}
                    fallback={<Banner tone="ok" message={t("reasonCodes.coverageComplete")} />}
                  >
                    {/* A notice, not a failure: an operator may have meant to leave an act with
                        no reason, which blocks it. The design system has `ok` and `danger` and
                        nothing between, and `danger` would call a deliberate choice an error — so
                        this is the neutral card shape, read by a screen reader as a status. */}
                    <div
                      role="status"
                      class="rounded-token border border-line bg-surface-raised px-3 py-2 text-sm text-ink"
                    >
                      {t("reasonCodes.coverageGaps", {
                        actions: gaps()
                          .map((action) => t(ACTION_LABEL[action]))
                          .join(", "),
                      })}
                    </div>
                  </Show>
                )}
              </Show>
            </div>
          </Show>
        </Card>

        <FormPanel
          crud={editor}
          createTitle={t("reasonCodes.new")}
          editTitle={t("reasonCodes.edit")}
          submitLabel={t("action.save")}
          onSubmit={save}
          dirty={() => code() !== "" || name() !== "" || actions().length > 0}
        >
          <div class="flex flex-col gap-4">
            <TextField
              label={t("reasonCodes.code")}
              value={code()}
              onInput={setCode}
              placeholder={t("reasonCodes.codePlaceholder")}
            />
            <p class="text-sm text-ink-muted">{t("reasonCodes.codeHint")}</p>
            <TextField
              label={t("reasonCodes.name")}
              value={name()}
              onInput={setName}
              placeholder={t("reasonCodes.namePlaceholder")}
            />
            <TextField
              label={t("reasonCodes.nameVi")}
              value={nameVi()}
              onInput={setNameVi}
              placeholder={t("reasonCodes.nameViPlaceholder")}
            />
            <p class="text-sm text-ink-muted">{t("reasonCodes.nameViHint")}</p>

            <fieldset class="border-t border-line pt-4">
              <legend class="mb-1 text-sm font-medium text-ink">
                {t("reasonCodes.appliesTo")}
              </legend>
              <p class="mb-2 text-sm text-ink-muted">{t("reasonCodes.appliesToHint")}</p>
              <div class="grid gap-2 sm:grid-cols-2">
                <For each={REASON_ACTIONS}>
                  {(action) => (
                    <CheckboxField
                      label={t(ACTION_LABEL[action])}
                      checked={actions().includes(action)}
                      onChange={(on) => toggleAction(action, on)}
                    />
                  )}
                </For>
              </div>
            </fieldset>

            <div class="border-t border-line pt-4">
              <CheckboxField
                label={t("reasonCodes.activeLabel")}
                checked={active()}
                onChange={setActive}
                hint={t("reasonCodes.activeHint")}
              />
            </div>
          </div>
        </FormPanel>

        <ConfirmDialog
          open={deletion.mode() === "confirming"}
          title={t("reasonCodes.deleteTitle")}
          message={t("reasonCodes.deleteMessage")}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={deletion.saving()}
          typeToConfirm={deletion.subject()?.code ?? ""}
          typePrompt={t("reasonCodes.deleteTypePrompt")}
          onConfirm={remove}
          onCancel={deletion.close}
        />
      </RequireContext>
    </div>
  );
}
