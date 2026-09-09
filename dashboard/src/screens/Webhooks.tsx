// Webhook endpoints (ADR-0032), on the authoring kit
// ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)): list a tenant's endpoints,
// register a new HTTPS destination for the store in context (the URL is vetted server-side by the
// SSRF guard), and delete one behind a type-to-confirm. The cursor shows how far delivery has
// reached; `disabled` marks an auto-disabled endpoint.
//
// Like the API-key token, the signing secret is shown **exactly once** — the cloud stores a hash and
// cannot show it again — so it lives in its own `Modal`, opened as the form closes. Same reasoning
// as `ApiKeys`: a form asks for input, this hands back something unrecoverable, and the two are not
// the same dialog.

import { createSignal, Show } from "solid-js";

import { api } from "../api/client";
import type { WebhookSummary } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, StatusBadge, TextField } from "../components/ui";
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

export function Webhooks() {
  const [rows, setRows] = createSignal<WebhookSummary[] | null>(null);
  const [url, setUrl] = createSignal("");
  // Held only until the operator dismisses it. Nothing persists it, here or anywhere.
  const [secret, setSecret] = createSignal("");
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  // Which endpoint is being re-enabled, so one slow request disables that row's button and not
  // every other row's (ADR-0121 §3 — the audit's "one shared busy flag disables every button").
  const [reenabling, setReenabling] = createSignal("");
  // Two lifecycles: deleting an endpoint must not freeze the registration form, or the reverse.
  const registration = useEntityCrud<WebhookSummary>();
  const deletion = useEntityCrud<WebhookSummary>();

  const load = async () => {
    setError("");
    setLoading(true);
    try {
      setRows(await api.listWebhooks(tenantId()));
    } catch (caught) {
      setError(apiMessage(caught));
    } finally {
      setLoading(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const openRegister = () => {
    setUrl("");
    registration.create();
  };

  const register = () => {
    if (!url().trim()) {
      registration.refuse(t("webhooks.urlRequired"));
      return;
    }
    void registration
      .run(async () => {
        const created = await api.registerWebhook(tenantId(), storeId(), url().trim());
        // Set inside the write, so the secret exists before `run` closes the panel and the second
        // dialog can open in the same tick. It is the only copy that will ever be shown.
        setSecret(created.signing_secret);
      })
      .then((registered) => {
        if (registered) {
          toast.ok(t("webhooks.registered"));
          void load();
        }
      });
  };

  const remove = () => {
    const endpoint = deletion.subject();
    if (!endpoint) {
      return;
    }
    void deletion.run(() => api.deleteWebhook(tenantId(), endpoint.id)).then((deleted) => {
      if (deleted) {
        toast.ok(t("webhooks.deleted"));
        void load();
      } else {
        // The confirm stays open carrying the refusal; a page banner would only say it twice.
        toast.error(deletion.error());
      }
    });
  };

  // Re-enable an endpoint the delivery task auto-disabled after a day of failures; delivery then
  // resumes from the endpoint's stored cursor, so nothing in the backlog is skipped (ADR-0032).
  const reenable = async (id: string) => {
    setError("");
    setReenabling(id);
    try {
      await api.enableWebhook(tenantId(), id);
      toast.ok(t("webhooks.reenabled"));
      await load();
    } catch (caught) {
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    } finally {
      setReenabling("");
    }
  };

  const columns = (): Column<WebhookSummary>[] => [
    {
      key: "url",
      header: t("webhooks.urlLabel"),
      cell: (row) => row.url,
      sortValue: (row) => row.url,
      class: "break-all",
    },
    {
      key: "status",
      header: t("webhooks.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.disabled ? "disabled" : "active"}
          label={row.disabled ? t("webhooks.disabled") : t("webhooks.active")}
        />
      ),
      sortValue: (row) => (row.disabled ? 1 : 0),
    },
    {
      key: "id",
      header: t("webhooks.id"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("webhooks.title")} description={t("webhooks.description")} />
      <RequireContext need="tenant">
        <Card
          title={t("webhooks.list")}
          actions={
            <div class="flex gap-2">
              {/* An endpoint follows one store's event log, so registering needs a store in
                  context. The banner below says why the button is out of reach. */}
              <Button disabled={!storeId()} onClick={openRegister}>
                {t("webhooks.register")}
              </Button>
              <Button variant="secondary" disabled={loading()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            </div>
          }
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={!storeId()}>
            <Banner tone="danger" message={t("context.storeRequired")} />
          </Show>
          <Show when={rows()}>
            {(loaded) => (
              <DataTable
                columns={columns()}
                rows={loaded()}
                searchText={(row) => row.url}
                pageSize={12}
                empty={<EmptyState title={t("webhooks.empty")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex shrink-0 gap-2">
                    <Show when={row.disabled}>
                      <Button
                        variant="secondary"
                        disabled={reenabling() === row.id}
                        onClick={() => void reenable(row.id)}
                      >
                        {t("webhooks.reenable")}
                      </Button>
                    </Show>
                    <Button
                      variant="danger"
                      disabled={deletion.saving()}
                      onClick={() => deletion.confirm(row)}
                    >
                      {t("action.delete")}
                    </Button>
                  </div>
                )}
              />
            )}
          </Show>
        </Card>

        <FormPanel
          crud={registration}
          createTitle={t("webhooks.register")}
          editTitle={t("webhooks.register")}
          submitLabel={t("action.register")}
          onSubmit={register}
          dirty={() => url() !== ""}
        >
          <TextField
            label={t("webhooks.urlLabel")}
            type="url"
            placeholder={t("webhooks.urlPlaceholder")}
            value={url()}
            onInput={setUrl}
            hint={t("webhooks.urlHint")}
          />
        </FormPanel>

        {/* The signing secret, once. Dismissing this drops the only copy the console will hold. */}
        <Modal
          open={secret() !== ""}
          title={t("webhooks.secretTitle")}
          closeLabel={t("action.close")}
          onClose={() => setSecret("")}
          footer={<Button onClick={() => setSecret("")}>{t("action.close")}</Button>}
        >
          <Banner tone="ok" message={t("webhooks.secretOnce")} />
          <code class="mt-3 block break-all rounded-token border border-line bg-surface-raised p-2 text-xs text-ink">
            {secret()}
          </code>
        </Modal>

        <ConfirmDialog
          open={deletion.mode() === "confirming"}
          title={t("webhooks.deleteTitle")}
          message={t("webhooks.deleteMessage")}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={deletion.saving()}
          // The endpoint's own URL must be typed out. `?? ""` rather than `!`: the dialog is mounted
          // permanently and `subject()` is null whenever it is shut.
          typeToConfirm={deletion.subject()?.url ?? ""}
          typePrompt={t("webhooks.deleteTypePrompt")}
          onConfirm={remove}
          onCancel={deletion.close}
        />
      </RequireContext>
    </div>
  );
}
