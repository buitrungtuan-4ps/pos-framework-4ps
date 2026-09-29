// The Integrations screen: which vendor serves this tenant, per family, and how to reach it
// ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)).
//
// An operator switches a store's e-invoice provider, adds a second delivery marketplace or points
// the ledger at a new ERP here, and nothing is released: a *connection* names a provider from the
// cloud's catalogue, the scope it serves, and the provider's settings. The settings form is drawn
// from the provider's own schema (`ProviderFields`), so a vendor added to the cloud needs no screen.
//
// Credentials are write-only. The cloud seals a secret the moment it arrives and never sends one
// back — a row says only which secrets are set — so the form never shows a stored password and an
// edit that leaves one blank keeps it. Nothing on this screen is personal data: vendor endpoints,
// merchant codes and sealed credentials are T2 configuration.

import { createMemo, createSignal, For, Show } from "solid-js";

import { ApiError, api } from "../api/client";
import type {
  Connection,
  ConnectionInput,
  ConnectionScopeLevel,
  IntegrationFamily,
  Provider,
  SettingValue,
} from "../api/types";
import { type MessageKey, t, tFromServer } from "../i18n";
import { withStaleReload } from "../lib/errors";
import { createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  PageHeader,
  SelectField,
  StatusBadge,
  TextField,
} from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  FormPanel,
  RowActions,
  TechnicalDetails,
} from "../components/kit";
import { ProviderFields } from "../components/ProviderFields";
import { useEntityCrud } from "../lib/entity-crud";
import { toast } from "../components/Toast";

/** The families in the order the screen lists them, each with its name and what it is for. */
const FAMILIES: readonly {
  readonly family: IntegrationFamily;
  readonly title: MessageKey;
  readonly hint: MessageKey;
}[] = [
  {
    family: "INTEGRATION_FAMILY_E_INVOICE",
    title: "integrations.family.einvoice",
    hint: "integrations.family.einvoiceHint",
  },
  {
    family: "INTEGRATION_FAMILY_QR_PAYMENT",
    title: "integrations.family.qr",
    hint: "integrations.family.qrHint",
  },
  {
    family: "INTEGRATION_FAMILY_CARD_TERMINAL",
    title: "integrations.family.card",
    hint: "integrations.family.cardHint",
  },
  {
    family: "INTEGRATION_FAMILY_DELIVERY",
    title: "integrations.family.delivery",
    hint: "integrations.family.deliveryHint",
  },
  {
    family: "INTEGRATION_FAMILY_COURIER",
    title: "integrations.family.courier",
    hint: "integrations.family.courierHint",
  },
  {
    family: "INTEGRATION_FAMILY_ERP",
    title: "integrations.family.erp",
    hint: "integrations.family.erpHint",
  },
];

/** A provider's name: its translation when this console ships one, else its id. */
function providerName(provider: Provider | undefined, providerId: string): string {
  return provider ? tFromServer(provider.name_key, provider.provider_id) : providerId;
}

export function Integrations() {
  const catalogue = createAdminResource(() => api.listProviders(), { scope: "tenant" });
  const connections = createAdminResource((tenant) => api.listConnections(tenant), {
    scope: "tenant",
  });
  const providers = () => catalogue.value()?.providers ?? [];
  const providerById = createMemo(
    () => new Map(providers().map((provider) => [provider.provider_id, provider])),
  );

  const editor = useEntityCrud<Connection>();
  const deletion = useEntityCrud<Connection>();
  // Which family the editor is adding to — the provider picker lists only that family's vendors.
  const [family, setFamily] = createSignal<IntegrationFamily>("INTEGRATION_FAMILY_E_INVOICE");
  const [providerId, setProviderId] = createSignal("");
  const [name, setName] = createSignal("");
  const [scope, setScope] = createSignal<ConnectionScopeLevel>("CONNECTION_SCOPE_TENANT");
  const [scopeId, setScopeId] = createSignal<string | undefined>(undefined);
  const [enabled, setEnabled] = createSignal(true);
  const [values, setValues] = createSignal<Record<string, SettingValue>>({});
  const [secrets, setSecrets] = createSignal<Record<string, string>>({});
  const [clearing, setClearing] = createSignal<string[]>([]);

  const chosen = () => providerById().get(providerId());
  const inFamily = (wanted: IntegrationFamily) =>
    providers().filter((provider) => provider.family === wanted);
  const rowsIn = (wanted: IntegrationFamily) =>
    (connections.value() ?? []).filter(
      (row) => providerById().get(row.provider_id)?.family === wanted,
    );

  const resetForm = (row: Connection | null, forFamily: IntegrationFamily) => {
    setFamily(forFamily);
    setProviderId(row?.provider_id ?? inFamily(forFamily)[0]?.provider_id ?? "");
    setName(row?.display_name ?? "");
    setScope(row?.scope_level ?? "CONNECTION_SCOPE_TENANT");
    setScopeId(row?.scope_id);
    setEnabled(row?.enabled ?? true);
    setValues({ ...(row?.settings ?? {}) });
    setSecrets({});
    setClearing([]);
  };

  const openCreate = (forFamily: IntegrationFamily) => {
    resetForm(null, forFamily);
    editor.create();
  };

  const openEdit = (row: Connection) => {
    const rowFamily = providerById().get(row.provider_id)?.family ?? "INTEGRATION_FAMILY_E_INVOICE";
    resetForm(row, rowFamily);
    editor.edit(row);
  };

  // Switching vendor drops the old vendor's settings and secrets, as the cloud does: one vendor's
  // endpoint or key is never another's, even when both call the field the same thing.
  const chooseProvider = (next: string) => {
    setProviderId(next);
    setValues({});
    setSecrets({});
    setClearing([]);
  };

  const chooseScope = (next: string) => {
    if (next === "CONNECTION_SCOPE_STORE") {
      setScope("CONNECTION_SCOPE_STORE");
      setScopeId(storeId());
      return;
    }
    // A brand or another store the connection already served stays as it was; the picker offers
    // the tenant and the store in context, and an unchanged choice keeps the rest.
    const existing = editor.subject();
    if (existing && next === existing.scope_level) {
      setScope(existing.scope_level);
      setScopeId(existing.scope_id);
      return;
    }
    setScope("CONNECTION_SCOPE_TENANT");
    setScopeId(undefined);
  };

  const scopeOptions = () => {
    const options = [
      { value: "CONNECTION_SCOPE_TENANT", label: t("integrations.scope.tenant") },
    ];
    const existing = editor.subject();
    if (existing && existing.scope_level === "CONNECTION_SCOPE_BRAND") {
      options.push({ value: "CONNECTION_SCOPE_BRAND", label: t("integrations.scope.brand") });
    }
    if (storeId() || scope() === "CONNECTION_SCOPE_STORE") {
      options.push({
        value: "CONNECTION_SCOPE_STORE",
        label:
          scope() === "CONNECTION_SCOPE_STORE" && scopeId() && scopeId() !== storeId()
            ? t("integrations.scope.otherStore")
            : t("integrations.scope.store", { store: storeName() }),
      });
    }
    return options;
  };

  // An edit is conditional on the version it was read at (ADR-0094): somebody else saving first is
  // a `412`, answered with a sentence naming what changed and a reload.
  const conditional = <T,>(write: () => Promise<T>) =>
    withStaleReload(write, () => connections.refetch(), t("integrations.stale"));

  // A refusal that names settings fields reads as the fields' own labels, so the operator knows
  // which inputs to fix; any other refusal passes through as the server said it.
  const explained = async <T,>(write: () => Promise<T>): Promise<T> => {
    try {
      return await write();
    } catch (caught) {
      if (caught instanceof ApiError) {
        const fields = caught.details
          .map((detail) => detail.field)
          .filter((field): field is string => typeof field === "string")
          .filter((field) => field.startsWith("settings."))
          .map((field) => field.slice("settings.".length))
          .map((key) => {
            const field = chosen()?.fields.find((candidate) => candidate.key === key);
            return field ? tFromServer(field.label_key, field.key) : key;
          });
        if (fields.length > 0) {
          throw new Error(t("integrations.settingsRefused", { fields: fields.join(", ") }));
        }
      }
      throw caught;
    }
  };

  const save = () => {
    const label = name().trim();
    if (!chosen()) {
      editor.refuse(t("integrations.providerRequired"));
      return;
    }
    if (!label) {
      editor.refuse(t("integrations.nameRequired"));
      return;
    }
    const typedSecrets = Object.fromEntries(
      Object.entries(secrets()).filter(([, value]) => value !== ""),
    );
    const input: ConnectionInput = {
      provider_id: providerId(),
      scope_level: scope(),
      ...(scopeId() ? { scope_id: scopeId() } : {}),
      display_name: label,
      enabled: enabled(),
      settings: values(),
      secrets: typedSecrets,
      clear_secrets: clearing(),
    };
    const target = editor.subject();
    void editor
      .run(() =>
        conditional(() =>
          explained(() =>
            target
              ? api.updateConnection(tenantId(), target.connection_id, target.etag, input)
              : api.createConnection(tenantId(), input),
          ),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(t("integrations.saved"));
          void connections.refetch();
        }
      });
  };

  const remove = () => {
    const row = deletion.subject();
    if (!row) {
      return;
    }
    void deletion
      .run(() => api.deleteConnection(tenantId(), row.connection_id))
      .then((deleted) => {
        if (deleted) {
          toast.ok(t("integrations.deleted"));
          void connections.refetch();
        } else {
          toast.error(deletion.error());
        }
      });
  };

  const scopeLabel = (row: Connection) => {
    switch (row.scope_level) {
      case "CONNECTION_SCOPE_BRAND":
        return t("integrations.scope.brand");
      case "CONNECTION_SCOPE_STORE":
        return row.scope_id === storeId()
          ? t("integrations.scope.store", { store: storeName() })
          : t("integrations.scope.otherStore");
      default:
        return t("integrations.scope.tenant");
    }
  };

  const columns = (): Column<Connection>[] => [
    {
      key: "name",
      header: t("integrations.name"),
      cell: (row) => (
        <div>
          <div>{row.display_name}</div>
          <div class="text-sm text-ink-muted">
            {providerName(providerById().get(row.provider_id), row.provider_id)}
          </div>
        </div>
      ),
      sortValue: (row) => row.display_name,
    },
    {
      key: "scope",
      header: t("integrations.scope"),
      cell: (row) => <span class="text-sm">{scopeLabel(row)}</span>,
    },
    {
      key: "standing",
      header: t("integrations.standing"),
      cell: (row) => (
        <StatusBadge
          tone={row.enabled ? "active" : "archived"}
          label={row.enabled ? t("integrations.enabled") : t("integrations.disabled")}
        />
      ),
    },
    {
      key: "secrets",
      header: t("integrations.credentials"),
      cell: (row) => (
        <span class="text-sm text-ink-muted">
          {row.secrets_set.length > 0
            ? t("integrations.credentialsSet", { count: row.secrets_set.length })
            : t("integrations.credentialsNone")}
        </span>
      ),
    },
    {
      key: "id",
      header: t("integrations.id"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>
          {row.connection_id}
        </TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("integrations.title")} description={t("integrations.description")} />
      <RequireContext need="tenant">
        <Show when={failureOf(catalogue) ?? failureOf(connections)}>
          {(message) => <Banner tone="danger" message={message()} />}
        </Show>
        <For each={FAMILIES.filter((entry) => inFamily(entry.family).length > 0)}>
          {(entry) => (
            <Card
              title={t(entry.title)}
              actions={
                <Button onClick={() => openCreate(entry.family)}>{t("integrations.connect")}</Button>
              }
            >
              <p class="mb-3 text-sm text-ink-muted">{t(entry.hint)}</p>
              <DataTable
                columns={columns()}
                rows={rowsIn(entry.family)}
                searchText={(row) => `${row.display_name} ${row.provider_id}`}
                pageSize={8}
                empty={
                  <EmptyState
                    title={t("integrations.empty")}
                    description={t("integrations.emptyHint", {
                      providers: inFamily(entry.family)
                        .map((provider) => providerName(provider, provider.provider_id))
                        .join(", "),
                    })}
                  />
                }
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <RowActions label={t("common.actions")}>
                    <Button
                      size="sm"
                      class="justify-start"
                      variant="ghost"
                      onClick={() => openEdit(row)}
                    >
                      {t("action.edit")}
                    </Button>
                    <Button
                      size="sm"
                      class="justify-start"
                      variant="danger-ghost"
                      onClick={() => deletion.confirm(row)}
                    >
                      {t("action.delete")}
                    </Button>
                  </RowActions>
                )}
              />
            </Card>
          )}
        </For>

        <FormPanel
          crud={editor}
          createTitle={t("integrations.new")}
          editTitle={t("integrations.edit")}
          submitLabel={t("action.save")}
          onSubmit={save}
          dirty={() => name() !== "" || Object.keys(secrets()).length > 0}
        >
          <div class="flex flex-col gap-4">
            <SelectField
              label={t("integrations.provider")}
              value={providerId()}
              options={inFamily(family()).map((provider) => ({
                value: provider.provider_id,
                label: provider.sandbox
                  ? t("integrations.providerSandbox", {
                      name: providerName(provider, provider.provider_id),
                    })
                  : providerName(provider, provider.provider_id),
              }))}
              onChange={chooseProvider}
              hint={t("integrations.providerHint")}
            />
            <TextField
              label={t("integrations.name")}
              value={name()}
              onInput={setName}
              placeholder={t("integrations.namePlaceholder")}
            />
            <SelectField
              label={t("integrations.scope")}
              value={scope()}
              options={scopeOptions()}
              onChange={chooseScope}
              hint={t("integrations.scopeHint")}
            />
            <CheckboxField
              label={t("integrations.enabledLabel")}
              checked={enabled()}
              onChange={setEnabled}
              hint={t("integrations.enabledHint")}
            />
            <Show when={chosen()}>
              {(provider) => (
                <fieldset class="border-t border-line pt-4">
                  <legend class="mb-2 text-sm font-medium text-ink">
                    {t("integrations.settings")}
                  </legend>
                  <Show
                    when={provider().fields.length > 0}
                    fallback={<p class="text-sm text-ink-muted">{t("integrations.noSettings")}</p>}
                  >
                    <ProviderFields
                      fields={provider().fields}
                      values={values()}
                      onValue={(key, value) =>
                        setValues((current) => {
                          const next = { ...current };
                          if (value === undefined) {
                            delete next[key];
                          } else {
                            next[key] = value;
                          }
                          return next;
                        })
                      }
                      secrets={secrets()}
                      onSecret={(key, value) =>
                        setSecrets((current) => ({ ...current, [key]: value }))
                      }
                      secretsSet={
                        editor.subject()?.provider_id === providerId()
                          ? (editor.subject()?.secrets_set ?? [])
                          : []
                      }
                      clearing={clearing()}
                      onClear={(key, clear) =>
                        setClearing((current) =>
                          clear ? [...current, key] : current.filter((entry) => entry !== key),
                        )
                      }
                    />
                  </Show>
                </fieldset>
              )}
            </Show>
          </div>
        </FormPanel>

        <ConfirmDialog
          open={deletion.mode() === "confirming"}
          title={t("integrations.deleteTitle")}
          message={t("integrations.deleteMessage")}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={deletion.saving()}
          typeToConfirm={deletion.subject()?.display_name ?? ""}
          typePrompt={t("integrations.deleteTypePrompt")}
          onConfirm={remove}
          onCancel={deletion.close}
        />
      </RequireContext>
    </div>
  );
}
