// Gateway endpoints (ADR-0198): the https targets a tenant's stores reach the internet through,
// on the authoring kit (ADR-0121). A store never calls the internet itself; its lookups name one
// of these by id and the cloud makes the call, behind the same guard as webhooks.
//
// The URL is vetted by the cloud when it is saved: https only, no credentials, public addresses
// only. Secret headers are write-only, as a connection's secrets are: typed once, sealed by the
// cloud, never shown again; a row says only which are set, an edit that types none keeps them, and
// each can be removed. Nothing here is personal data: endpoints and sealed credentials are T2
// configuration.

import { createSignal, For, Show } from "solid-js";

import {
  type GatewayEndpointInput,
  createGatewayEndpoint,
  deleteGatewayEndpoint,
  listGatewayEndpoints,
  updateGatewayEndpoint,
} from "../api/gateway";
import type { GatewayEndpoint } from "../api/schemas";
import { t } from "../i18n";
import { withStaleReload } from "../lib/errors";
import { fieldsFromText, fieldsToText, headersFromText } from "../lib/gateway-fields";
import { type AdminResource, createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import { useEntityCrud } from "../lib/entity-crud";
import { tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  NumberField,
  PageHeader,
  SelectField,
  Skeleton,
  StatusBadge,
  TextArea,
  TextField,
} from "../components/ui";
import { type Column, ConfirmDialog, DataTable, EmptyState, FormPanel, RowActions } from "../components/kit";
import { toast } from "../components/Toast";

const METHODS = [
  { value: "GATEWAY_METHOD_GET", label: "GET" },
  { value: "GATEWAY_METHOD_POST", label: "POST" },
] as const;

/** The table's columns. They read nothing but the row. */
const columns = (): Column<GatewayEndpoint>[] => [
  { key: "name", header: t("gateway.name"), cell: (row) => row.display_name, sortValue: (row) => row.display_name },
  {
    key: "url",
    header: t("gateway.url"),
    cell: (row) => `${row.method === "GATEWAY_METHOD_GET" ? "GET" : "POST"} ${row.url}`,
    class: "break-all",
  },
  { key: "deadline", header: t("gateway.deadline"), cell: (row) => t("gateway.deadlineMs", { ms: row.deadline_ms }) },
  {
    key: "secrets",
    header: t("gateway.secretHeaders"),
    cell: (row) => (row.secret_headers_set.length === 0 ? t("gateway.noSecrets") : row.secret_headers_set.join(", ")),
  },
  {
    key: "status",
    header: t("gateway.status"),
    cell: (row) => (
      <StatusBadge tone={row.enabled ? "active" : "disabled"} label={row.enabled ? t("gateway.enabled") : t("gateway.disabled")} />
    ),
  },
];

/** The form's state, as text where the operator types lists. */
interface Draft {
  name: string;
  url: string;
  method: string;
  deadline: number | null;
  idempotent: boolean;
  enabled: boolean;
  requestFields: string;
  responseFields: string;
  headers: string;
  clearing: readonly string[];
}

function draftOf(row: GatewayEndpoint | null): Draft {
  return {
    name: row?.display_name ?? "",
    url: row?.url ?? "",
    method: row?.method ?? "GATEWAY_METHOD_GET",
    deadline: row?.deadline_ms ?? 3000,
    idempotent: row?.idempotent ?? false,
    enabled: row?.enabled ?? true,
    requestFields: fieldsToText(row?.request_fields ?? []),
    responseFields: fieldsToText(row?.response_fields ?? []),
    headers: "",
    clearing: [],
  };
}

/** The request a draft makes, or the sentence saying which input it could not read. */
export function inputOf(draft: Draft): GatewayEndpointInput | string {
  const request = fieldsFromText(draft.requestFields);
  if ("badLine" in request) {
    return t("gateway.badField", { which: t("gateway.requestFields"), line: request.badLine });
  }
  const response = fieldsFromText(draft.responseFields);
  if ("badLine" in response) {
    return t("gateway.badField", { which: t("gateway.responseFields"), line: response.badLine });
  }
  const headers = headersFromText(draft.headers);
  if ("badLine" in headers) {
    return t("gateway.badHeader", { line: headers.badLine });
  }
  return {
    display_name: draft.name.trim(),
    url: draft.url.trim(),
    method: draft.method as GatewayEndpointInput["method"],
    deadline_ms: draft.deadline,
    idempotent: draft.idempotent,
    enabled: draft.enabled,
    request_fields: request.fields,
    response_fields: response.fields,
    secret_headers: headers.headers,
    clear_secret_headers: [...draft.clearing],
  };
}

/** The form's inputs, over one draft. */
function EndpointForm(props: { draft: Draft; set: (next: Draft) => void; secretsSet: readonly string[] }) {
  const change = <K extends keyof Draft>(key: K) => (value: Draft[K]) => props.set({ ...props.draft, [key]: value });
  return (
    <div class="flex flex-col gap-4">
      <TextField label={t("gateway.name")} value={props.draft.name} onInput={change("name")} />
      <TextField label={t("gateway.url")} type="url" value={props.draft.url} onInput={change("url")} hint={t("gateway.urlHint")} />
      <SelectField label={t("gateway.method")} value={props.draft.method} options={METHODS} onChange={change("method")} />
      <NumberField label={t("gateway.deadline")} value={props.draft.deadline} onChange={change("deadline")} min={100} max={10000} step={100} hint={t("gateway.deadlineHint")} />
      <CheckboxField label={t("gateway.idempotent")} checked={props.draft.idempotent} onChange={change("idempotent")} hint={t("gateway.idempotentHint")} />
      <CheckboxField label={t("gateway.enabledLabel")} checked={props.draft.enabled} onChange={change("enabled")} />
      <TextArea label={t("gateway.requestFields")} rows={4} value={props.draft.requestFields} onInput={change("requestFields")} hint={t("gateway.fieldsHint")} />
      <TextArea label={t("gateway.responseFields")} rows={4} value={props.draft.responseFields} onInput={change("responseFields")} hint={t("gateway.fieldsHint")} />
      <TextArea label={t("gateway.secretHeaders")} rows={3} value={props.draft.headers} onInput={change("headers")} hint={t("gateway.headersHint")} />
      <For each={props.secretsSet}>
        {(name) => (
          <CheckboxField
            label={t("gateway.removeHeader", { name })}
            checked={props.draft.clearing.includes(name)}
            onChange={(checked) =>
              change("clearing")(checked ? [...props.draft.clearing, name] : props.draft.clearing.filter((held) => held !== name))
            }
          />
        )}
      </For>
    </div>
  );
}

/** The list of endpoints, with their row actions. */
function EndpointTable(props: {
  endpoints: AdminResource<GatewayEndpoint[]>;
  onNew: () => void;
  onEdit: (row: GatewayEndpoint) => void;
  onDelete: (row: GatewayEndpoint) => void;
}) {
  return (
    <Card title={t("gateway.list")} actions={<Button onClick={() => props.onNew()}>{t("gateway.new")}</Button>}>
      <Show when={failureOf(props.endpoints)}>{(message) => <Banner tone="danger" message={message()} />}</Show>
      <Show when={props.endpoints.value()} fallback={<Skeleton label={t("common.loading")} rows={3} />}>
        {(rows) => (
          <DataTable
            columns={columns()}
            rows={rows()}
            searchText={(row) => `${row.display_name} ${row.url}`}
            pageSize={12}
            empty={<EmptyState title={t("gateway.empty")} description={t("gateway.emptyHint")} />}
            actionsHeader={t("common.actions")}
            actions={(row) => (
              <RowActions label={t("common.actions")}>
                <Button size="sm" class="justify-start" variant="ghost" onClick={() => props.onEdit(row)}>
                  {t("action.edit")}
                </Button>
                <Button size="sm" class="justify-start" variant="danger-ghost" onClick={() => props.onDelete(row)}>
                  {t("action.delete")}
                </Button>
              </RowActions>
            )}
          />
        )}
      </Show>
    </Card>
  );
}

export function GatewayEndpoints() {
  const endpoints = createAdminResource((tenant) => listGatewayEndpoints(tenant), { scope: "tenant" });
  const editor = useEntityCrud<GatewayEndpoint>();
  const deletion = useEntityCrud<GatewayEndpoint>();
  const [draft, setDraft] = createSignal<Draft>(draftOf(null));

  const open = (row: GatewayEndpoint | null) => {
    setDraft(draftOf(row));
    if (row) {
      editor.edit(row);
    } else {
      editor.create();
    }
  };

  // An edit is conditional on the version it was read at (ADR-0094): somebody else saving first is
  // a `412`, answered with a sentence and a reload.
  const conditional = <T,>(write: () => Promise<T>) =>
    withStaleReload(write, () => endpoints.refetch(), t("gateway.stale"));

  const save = () => {
    const input = inputOf(draft());
    if (typeof input === "string") {
      editor.refuse(input);
      return;
    }
    const target = editor.subject();
    void editor
      .run(() =>
        conditional(() =>
          target
            ? updateGatewayEndpoint(tenantId(), target.gateway_endpoint_id, target.etag, input)
            : createGatewayEndpoint(tenantId(), input),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(t("gateway.saved"));
          void endpoints.refetch();
        }
      });
  };

  const remove = () => {
    const row = deletion.subject();
    if (!row) {
      return;
    }
    void deletion.run(() => deleteGatewayEndpoint(tenantId(), row.gateway_endpoint_id)).then((deleted) => {
      if (deleted) {
        toast.ok(t("gateway.deleted"));
        void endpoints.refetch();
      } else {
        toast.error(deletion.error());
      }
    });
  };

  return (
    <div>
      <PageHeader title={t("gateway.title")} description={t("gateway.description")} />
      <RequireContext need="tenant">
        <EndpointTable
          endpoints={endpoints}
          onNew={() => open(null)}
          onEdit={open}
          onDelete={(row) => deletion.confirm(row)}
        />

        <FormPanel
          crud={editor}
          createTitle={t("gateway.new")}
          editTitle={t("gateway.edit")}
          submitLabel={t("action.save")}
          onSubmit={save}
          dirty={() => draft().name !== "" || draft().headers !== ""}
        >
          <EndpointForm draft={draft()} set={setDraft} secretsSet={editor.subject()?.secret_headers_set ?? []} />
        </FormPanel>

        <ConfirmDialog
          open={deletion.mode() === "confirming"}
          title={t("gateway.deleteTitle")}
          message={t("gateway.deleteMessage")}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={deletion.saving()}
          typeToConfirm={deletion.subject()?.display_name ?? ""}
          typePrompt={t("gateway.deleteTypePrompt")}
          onConfirm={remove}
          onCancel={deletion.close}
        />
      </RequireContext>
    </div>
  );
}
