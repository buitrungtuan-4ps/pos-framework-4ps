// The typed HTTP client for pos_cloud's `/admin` surface (ADR-0060). Every call is one `fetch` to
// the same origin that served the dashboard, so the super-admin session cookie the server set on
// login rides along automatically and no base URL or token handling is needed. A refused request
// comes back as a non-2xx with a plain-text (or, for a rejected config publish, a JSON) reason;
// this surfaces it as an `ApiError` the screens show without guessing.

import type {
  ActivationCode,
  AdminIdentity,
  AdminInvite,
  AdminRole,
  AdminSessionView,
  AdminStatus,
  Alert,
  ApiKeySummary,
  Assignment,
  Area,
  AuditEntry,
  AuditFilter,
  Brand,
  Campaign,
  CampaignInput,
  CampaignPreview,
  CapabilityCatalogue,
  CatalogItem,
  Country,
  FloorTable,
  RoutingRule,
  Station,
  TableQrSheet,
  ChannelPrice,
  ConfigLevel,
  ConfigVersion,
  CreateApiKeyResponse,
  CreatedId,
  DailyRevenue,
  DailyRollup,
  XzReport,
  Device,
  DeviceProposalSummary,
  DisplayCategory,
  DisplaySubcategory,
  Employee,
  EntityStatus,
  ETag,
  Enrolment,
  FleetStore,
  Ingredient,
  IngredientInput,
  InviteAdminResponse,
  ItemCategory,
  ItemSubcategory,
  Json,
  LayoutButton,
  Menu,
  MenuPlacement,
  MenuSection,
  ModifierGroup,
  PermissionInfo,
  RecoveryCodesResponse,
  RecoveryCodesStatus,
  SalesChannel,
  OtaPlacement,
  OtaRollout,
  PublishPlacementRequest,
  PublishRolloutRequest,
  ReconcileRun,
  Recipe,
  RecipeInput,
  QrGuardrails,
  VendorPolicy,
  PublishedConfig,
  RegisterWebhookResponse,
  RoleTemplate,
  ScheduledPublish,
  ScheduledPublishCreated,
  Store,
  Supplier,
  SupplierInput,
  TaskHealthReport,
  MediaSummary,
  SubjectExport,
  SubjectMeta,
  TaxClass,
  TaxRate,
  Tenant,
  TranslationGrid,
  TranslationImportReport,
  UploadedMedia,
  Voucher,
  WebhookSummary,
} from "./types";
import { setActingAdmin, setAuthed } from "../state/session";

/** One field-level reason from an AIP-193 error body: the offending field, and a stable reason for it. */
export interface ApiErrorDetail {
  readonly field: string;
  readonly reason: string;
}

export class ApiError extends Error {
  readonly status: number;

  /**
   * The AIP-193 canonical status token (`FAILED_PRECONDITION`, `NOT_FOUND`, …) when the server sent
   * an error envelope; `null` when it answered in plain text.
   *
   * Deliberately separate from `status`, which is the HTTP number. The token is the stable thing to
   * branch on: `409` carries both `ALREADY_EXISTS` and `FAILED_PRECONDITION`, and a caller that has
   * to tell a duplicate apart from a wrong-state refusal cannot do it from the number alone.
   */
  readonly canonical: string | null;

  /** Field-level detail, when the server named the offending fields. Empty otherwise. */
  readonly details: readonly ApiErrorDetail[];

  constructor(
    status: number,
    message: string,
    canonical: string | null = null,
    details: readonly ApiErrorDetail[] = [],
  ) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.canonical = canonical;
    this.details = details;
  }

  /** No session, or it expired — the shell sends the operator back to the login screen. */
  get isUnauthorized(): boolean {
    return this.status === 401;
  }

  /** A validation refusal (bad config, a URL the SSRF guard rejects) — the operator's to fix. */
  get isRejected(): boolean {
    return this.status === 400 || this.status === 409 || this.status === 422;
  }

  /**
   * Somebody else saved this record between the load and the save (ADR-0094). The screen's copy is
   * stale, so the only correct recovery is to reload and let the operator see what changed —
   * never a retry, which would re-apply the overwrite through a different door.
   */
  get isStale(): boolean {
    return this.status === 412;
  }
}

async function failure(response: Response): Promise<ApiError> {
  // A 401 means the session is gone or expired. Drop the client's authed flag so the shell's route
  // guard sends the operator back to the login screen, rather than stranding them on a view that can
  // no longer load anything (F0).
  if (response.status === 401) {
    setAuthed(false);
    setActingAdmin(null);
  }
  const text = await response.text().catch(() => "");
  const trimmed = text.trim();
  // Three body shapes reach here, and all three have to be readable.
  //
  // The cloud is being migrated onto the AIP-193 envelope — `{"error":{code,status,message,details}}`
  // — one group of handlers at a time (roadmap v3 Q3), so at any point in that migration some
  // handlers answer the envelope and the rest answer plain text. A rejected config publish is a
  // third shape again: `422 {"violations":[...]}`. Reading all three is what lets the conversion
  // land in reviewable slices instead of one 619-site commit: no intermediate state can strand the
  // console on a body it cannot parse.
  if (trimmed.startsWith("{")) {
    try {
      const body = JSON.parse(trimmed) as {
        violations?: string[];
        error?: { status?: unknown; message?: unknown; details?: unknown };
      };
      const envelope = body.error;
      if (envelope && typeof envelope.message === "string" && envelope.message !== "") {
        return new ApiError(
          response.status,
          envelope.message,
          typeof envelope.status === "string" ? envelope.status : null,
          errorDetails(envelope.details),
        );
      }
      if (Array.isArray(body.violations) && body.violations.length > 0) {
        return new ApiError(response.status, body.violations.join("; "));
      }
    } catch {
      // Neither known JSON shape; fall through to the raw text.
    }
  }
  return new ApiError(response.status, trimmed || response.statusText);
}

// Reads the envelope's `details` array, keeping only entries that carry both halves. Tolerant by
// design: this is an *error* path, and losing a server's message to a strict parse of a field we do
// not even need would replace useful information with none, at the worst possible moment.
function errorDetails(raw: unknown): ApiErrorDetail[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw.flatMap((entry) => {
    if (entry === null || typeof entry !== "object") {
      return [];
    }
    const { field, reason } = entry as Partial<ApiErrorDetail>;
    return typeof field === "string" && typeof reason === "string" ? [{ field, reason }] : [];
  });
}

async function requestJson<T>(method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(path, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    throw await failure(response);
  }
  return (await response.json()) as T;
}

// A conditional write (ADR-0094): `if-match` carries the version the record was read at, as a strong
// entity-tag. The server requires it, so this is the only way to reach a mutating `/admin` route.
// `etag` is the opaque token as it arrived, and the quotes are added here so no caller has to know
// the header's grammar.
async function requestJsonIfMatch<T>(
  method: string,
  path: string,
  etag: ETag,
  body?: unknown,
): Promise<T> {
  const response = await fetch(path, {
    method,
    headers: {
      "if-match": `"${etag}"`,
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    throw await failure(response);
  }
  return (await response.json()) as T;
}

async function requestVoid(method: string, path: string, body?: unknown): Promise<void> {
  const response = await fetch(path, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    throw await failure(response);
  }
}

// `GET` for an effective config returns the composed document or `404` when nothing is published;
// the caller distinguishes "not found" (a fresh store) from a real error by the status.
async function requestJsonOrNull<T>(path: string): Promise<T | null> {
  const response = await fetch(path);
  if (response.status === 404) {
    return null;
  }
  if (!response.ok) {
    throw await failure(response);
  }
  return (await response.json()) as T;
}

// Uploads a raw binary body (an image) and reads the JSON reply (ADR-0075). The server re-encodes and
// bounds it, so the browser sends the file as-is; the `content-type` names the format for the decoder.
async function requestUpload<T>(path: string, file: Blob): Promise<T> {
  const response = await fetch(path, {
    method: "POST",
    headers: { "content-type": file.type || "application/octet-stream" },
    body: file,
  });
  if (!response.ok) {
    throw await failure(response);
  }
  return (await response.json()) as T;
}

// Fetches a CSV export (ADR-0075) and hands it to the browser as a file download. The session cookie
// rides the same-origin request, so a viewer without the domain's manage permission gets a `403` that
// surfaces as an `ApiError` for the caller to toast — no partial file is saved.
async function downloadCsv(path: string, filename: string): Promise<void> {
  const response = await fetch(path);
  if (!response.ok) {
    throw await failure(response);
  }
  const blob = await response.blob();
  const url = URL.createObjectURL(blob);
  try {
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = filename;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
  } finally {
    URL.revokeObjectURL(url);
  }
}

const tenantQuery = (tenantId: string) => `tenant_id=${encodeURIComponent(tenantId)}`;

/** `tenant_id` plus an optional rollup window (`from`/`to`/`limit`), as a query string. */
const rollupWindowQuery = (
  tenantId: string,
  window?: { from?: string; to?: string; limit?: number },
) => {
  const params = new URLSearchParams(tenantQuery(tenantId));
  if (window?.from) {
    params.set("from", window.from);
  }
  if (window?.to) {
    params.set("to", window.to);
  }
  if (window?.limit !== undefined) {
    params.set("limit", String(window.limit));
  }
  return params.toString();
};

export const api = {
  // --- session / enrolment (ADR-0034) ---
  session: () => requestVoid("GET", "/admin/session"),
  login: (password: string, totpCode: string) =>
    requestVoid("POST", "/admin/login", { password, totp_code: totpCode }),
  logout: () => requestVoid("POST", "/admin/logout"),
  setup: (setupToken: string, password: string) =>
    requestJson<Enrolment>("POST", "/admin/setup", {
      setup_token: setupToken,
      password,
    }),

  // --- console identity & RBAC (ADR-0067, Track G1) ---
  // The acting admin's own identity, so the console labels the operator and shows only the areas
  // their role grants. Role gating in the UI is convenience only — the server re-checks every route.
  whoami: () => requestJson<AdminIdentity>("GET", "/admin/whoami"),

  // Admin roster + role/status management. Listing/inviting need owner or admin; role/status changes
  // need owner (the server enforces this — the console only hides what a role cannot do).
  listAdmins: () => requestJson<AdminIdentity[]>("GET", "/admin/admins"),
  setAdminRole: (id: string, role: AdminRole) =>
    requestVoid("PATCH", `/admin/admins/${encodeURIComponent(id)}/role`, { role }),
  setAdminStatus: (id: string, status: AdminStatus) =>
    requestVoid("PATCH", `/admin/admins/${encodeURIComponent(id)}/status`, { status }),

  // Invitations: an owner/admin invites by email; the single-use token is returned once for the
  // inviter to hand over out-of-band (the invitee self-enrols with it, never an admin-set password).
  listInvites: () => requestJson<AdminInvite[]>("GET", "/admin/invites"),
  inviteAdmin: (email: string, name: string, role: AdminRole) =>
    requestJson<InviteAdminResponse>("POST", "/admin/invites", { email, name, role }),
  revokeInvite: (id: string) =>
    requestVoid("DELETE", `/admin/invites/${encodeURIComponent(id)}`),
  // Pre-auth self-enrolment: the invitee exchanges the single-use token and a chosen password for
  // their own credential; the server returns the one-time TOTP enrolment (never an admin-set password).
  acceptInvite: (token: string, password: string) =>
    requestJson<Enrolment>("POST", "/admin/invites/accept", { token, password }),

  // Self-service sessions: every admin lists and revokes their own; the current session is protected
  // from accidental self-revocation, and "sign out everywhere else" keeps only the current one.
  listSessions: () => requestJson<AdminSessionView[]>("GET", "/admin/sessions"),
  revokeSession: (id: string) =>
    requestVoid("DELETE", `/admin/sessions/${encodeURIComponent(id)}`),
  revokeOtherSessions: () => requestVoid("POST", "/admin/sessions/revoke-others"),

  // Self-service account security: re-enrol TOTP (re-confirms the password) and (re)generate the
  // one-time recovery codes (returned once, stored only as hashes).
  reenrolTotp: (password: string) =>
    requestJson<Enrolment>("POST", "/admin/totp", { password }),
  generateRecoveryCodes: () =>
    requestJson<RecoveryCodesResponse>("POST", "/admin/recovery-codes", {}),
  recoveryCodesStatus: () => requestJson<RecoveryCodesStatus>("GET", "/admin/recovery-codes"),

  // --- API keys (ADR-0037) ---
  listApiKeys: (tenantId: string) =>
    requestJson<ApiKeySummary[]>("GET", `/admin/api-keys?${tenantQuery(tenantId)}`),
  createApiKey: (tenantId: string, scopes: string[], expiresAtMs?: number) =>
    requestJson<CreateApiKeyResponse>("POST", "/admin/api-keys", {
      tenant_id: tenantId,
      scopes,
      ...(expiresAtMs === undefined ? {} : { expires_at_ms: expiresAtMs }),
    }),
  revokeApiKey: (id: string) => requestVoid("DELETE", `/admin/api-keys/${encodeURIComponent(id)}`),

  // --- config tree (ADR-0033) ---
  effectiveConfig: (tenantId: string, storeId: string) =>
    requestJsonOrNull<Json>(
      `/admin/stores/${encodeURIComponent(storeId)}/config?${tenantQuery(tenantId)}`,
    ),
  publishConfig: (tenantId: string, storeId: string, level: ConfigLevel, document: Json) =>
    requestJson<PublishedConfig>(
      "PUT",
      `/admin/stores/${encodeURIComponent(storeId)}/config/${level}?${tenantQuery(tenantId)}`,
      document,
    ),
  // Config version history (ADR-0069 G2): list the append-only versions, read one's effective
  // document for the diff view, and roll back (which appends a new current version).
  configVersions: (tenantId: string, storeId: string) =>
    requestJson<ConfigVersion[]>(
      "GET",
      `/admin/stores/${encodeURIComponent(storeId)}/config/versions?${tenantQuery(tenantId)}`,
    ),
  configVersionEffective: (tenantId: string, storeId: string, versionId: string) =>
    requestJson<Json>(
      "GET",
      `/admin/stores/${encodeURIComponent(storeId)}/config/versions/${encodeURIComponent(versionId)}?${tenantQuery(tenantId)}`,
    ),
  rollbackConfig: (tenantId: string, storeId: string, versionId: string) =>
    requestJson<PublishedConfig>(
      "POST",
      `/admin/stores/${encodeURIComponent(storeId)}/config/rollback?${tenantQuery(tenantId)}`,
      { version_id: versionId },
    ),

  // --- capabilities (ADR-0071, Track M8): the §10 flag catalogue + a form-driven capability publish ---
  // The catalogue is static §10 data behind console.data.read; publishing merges the flag booleans into
  // the store's config layer (never clobbering the other keys) and needs console.config.publish. The
  // server re-runs the §10 inter-flag rules, so an invalid combination is a 422, never a stored state.
  capabilityCatalogue: () => requestJson<CapabilityCatalogue>("GET", "/admin/capabilities"),
  publishCapabilities: (tenantId: string, storeId: string, flags: Record<string, boolean>) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/capabilities", {
      tenant_id: tenantId,
      store_id: storeId,
      flags,
    }),

  // --- rollups (ADR-0060 admin read; ADR-0081 windowing) ---
  // A window is optional: absent, the server returns the most recent 90 trading days, never the
  // store's entire history. `from`/`to` are inclusive YYYY-MM-DD business dates; `limit` caps the
  // days returned (newest kept).
  dailyRollups: (
    tenantId: string,
    storeId: string,
    window?: { from?: string; to?: string; limit?: number },
  ) => {
    const params = new URLSearchParams(tenantQuery(tenantId));
    if (window?.from) {
      params.set("from", window.from);
    }
    if (window?.to) {
      params.set("to", window.to);
    }
    if (window?.limit !== undefined) {
      params.set("limit", String(window.limit));
    }
    return requestJson<DailyRollup[]>(
      "GET",
      `/admin/stores/${encodeURIComponent(storeId)}/rollups/daily?${params.toString()}`,
    );
  },
  // Reset a store's materialised rollup so the projector rebuilds it from the event log — the
  // "reset-cursor-and-replay" recovery lever (ADR-0036), behind console.config.publish. Idempotent.
  resetRollups: (tenantId: string, storeId: string) =>
    requestVoid(
      "POST",
      `/admin/stores/${encodeURIComponent(storeId)}/rollups/reset?${tenantQuery(tenantId)}`,
    ),
  // Revenue & product-mix rollup (ADR-0081, Track O4) — prices are T2, so this is served only to
  // Owner/Admin (console.reports.revenue) and a non-holder gets a 403. Same window as dailyRollups.
  dailyRevenue: (
    tenantId: string,
    storeId: string,
    window?: { from?: string; to?: string; limit?: number },
  ) => {
    const params = new URLSearchParams(tenantQuery(tenantId));
    if (window?.from) {
      params.set("from", window.from);
    }
    if (window?.to) {
      params.set("to", window.to);
    }
    if (window?.limit !== undefined) {
      params.set("limit", String(window.limit));
    }
    return requestJson<DailyRevenue[]>(
      "GET",
      `/admin/stores/${encodeURIComponent(storeId)}/revenue/daily?${params.toString()}`,
    );
  },
  // X/Z report (ADR-0081, spec gap D10) — omit businessDate for the current day (an X); pass a past
  // day for its final Z. T2, so Owner/Admin only (console.reports.revenue); a non-holder gets 403.
  xzReport: (tenantId: string, storeId: string, businessDate?: string) => {
    const params = new URLSearchParams(tenantQuery(tenantId));
    if (businessDate) {
      params.set("business_date", businessDate);
    }
    return requestJson<XzReport>(
      "GET",
      `/admin/stores/${encodeURIComponent(storeId)}/reports/xz?${params.toString()}`,
    );
  },

  // --- device onboarding (ADR-0041) ---
  listProposals: (tenantId: string) =>
    requestJson<DeviceProposalSummary[]>(
      "GET",
      `/admin/devices/proposals?${tenantQuery(tenantId)}`,
    ),
  approveDevice: (tenantId: string, id: string) =>
    requestVoid(
      "POST",
      `/admin/devices/proposals/${encodeURIComponent(id)}/approve?${tenantQuery(tenantId)}`,
    ),
  rejectDevice: (tenantId: string, id: string) =>
    requestVoid(
      "POST",
      `/admin/devices/proposals/${encodeURIComponent(id)}/reject?${tenantQuery(tenantId)}`,
    ),

  // --- webhooks (ADR-0032) ---
  listWebhooks: (tenantId: string) =>
    requestJson<WebhookSummary[]>("GET", `/admin/webhooks?${tenantQuery(tenantId)}`),
  registerWebhook: (tenantId: string, storeId: string, url: string) =>
    requestJson<RegisterWebhookResponse>("POST", "/admin/webhooks", {
      tenant_id: tenantId,
      store_id: storeId,
      url,
    }),
  deleteWebhook: (tenantId: string, id: string) =>
    requestVoid("DELETE", `/admin/webhooks/${encodeURIComponent(id)}?${tenantQuery(tenantId)}`),
  enableWebhook: (tenantId: string, id: string) =>
    requestVoid("POST", `/admin/webhooks/${encodeURIComponent(id)}/enable?${tenantQuery(tenantId)}`),

  // --- translations (ADR-0043) ---
  getTranslations: (tenantId: string) =>
    requestJson<TranslationGrid>("GET", `/admin/translations?${tenantQuery(tenantId)}`),
  putTranslations: (tenantId: string, grid: TranslationGrid) =>
    requestVoid("PUT", `/admin/translations?${tenantQuery(tenantId)}`, grid),

  // --- activation codes (ADR-0050) ---
  issueActivation: (tenantId: string, storeId: string, deviceId: string) =>
    requestJson<ActivationCode>("POST", "/admin/activation-codes", {
      tenant_id: tenantId,
      store_id: storeId,
      device_id: deviceId,
    }),

  // --- people & access (ADR-0070, Track M1): employees, role templates, per-store assignments ---
  // Reads need only console.data.read; every write needs console.people.manage (Owner/Admin) — the
  // server re-checks, the console only hides what a role cannot do. A PIN is set/reset, never read: it
  // is hashed server-side and this client never sees the digits back (only whether one is set).
  listEmployees: (tenantId: string) =>
    requestJson<Employee[]>("GET", `/admin/employees?${tenantQuery(tenantId)}`),
  getEmployee: (tenantId: string, id: string) =>
    requestJson<Employee>(
      "GET",
      `/admin/employees/${encodeURIComponent(id)}?${tenantQuery(tenantId)}`,
    ),
  createEmployee: (tenantId: string, code: string, name: string) =>
    requestJson<CreatedId>("POST", "/admin/employees", { tenant_id: tenantId, code, name }),
  updateEmployee: (id: string, tenantId: string, fields: { name: string; status: EntityStatus }) =>
    requestVoid("PATCH", `/admin/employees/${encodeURIComponent(id)}`, {
      tenant_id: tenantId,
      name: fields.name,
      status: fields.status,
    }),
  setEmployeePin: (id: string, tenantId: string, pin: string) =>
    requestVoid("PUT", `/admin/employees/${encodeURIComponent(id)}/pin`, {
      tenant_id: tenantId,
      pin,
    }),

  listRoles: (tenantId: string) =>
    requestJson<RoleTemplate[]>("GET", `/admin/roles?${tenantQuery(tenantId)}`),
  createRole: (tenantId: string, name: string, permissions: string[]) =>
    requestJson<CreatedId>("POST", "/admin/roles", { tenant_id: tenantId, name, permissions }),
  updateRole: (
    id: string,
    tenantId: string,
    fields: { name: string; permissions: string[]; status: EntityStatus },
  ) =>
    requestVoid("PATCH", `/admin/roles/${encodeURIComponent(id)}`, {
      tenant_id: tenantId,
      name: fields.name,
      permissions: fields.permissions,
      status: fields.status,
    }),

  listAssignmentsByStore: (tenantId: string, storeId: string) =>
    requestJson<Assignment[]>(
      "GET",
      `/admin/assignments?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  createAssignment: (
    tenantId: string,
    employeeId: string,
    storeId: string,
    roleTemplateId: string,
  ) =>
    requestJson<CreatedId>("POST", "/admin/assignments", {
      tenant_id: tenantId,
      employee_id: employeeId,
      store_id: storeId,
      role_template_id: roleTemplateId,
    }),
  removeAssignment: (tenantId: string, id: string) =>
    requestVoid("DELETE", `/admin/assignments/${encodeURIComponent(id)}?${tenantQuery(tenantId)}`),

  // The pos-core permission catalogue (§9) the role editor offers, so the console never invents a
  // permission string — it presents these and stores a chosen subset.
  permissionCatalogue: () => requestJson<PermissionInfo[]>("GET", "/admin/people/permissions"),

  // Compile a store's people + roles + assignments into its `permissions` config node and version it
  // through the config tree, so the edge applies the published set. Needs console.people.manage.
  publishPermissions: (tenantId: string, storeId: string) =>
    requestJson<PublishedConfig>("POST", "/admin/people/publish", {
      tenant_id: tenantId,
      store_id: storeId,
    }),

  // --- floor & kitchen (ADR-0072, Track M2): per-store areas/tables + kitchen stations/routing ---
  // Reads need console.data.read; every write needs console.floor.manage (Owner/Admin) — the server
  // re-checks. A floor is per-store, so lists pass both tenant_id and store_id. None of it is PII.
  listAreas: (tenantId: string, storeId: string) =>
    requestJson<Area[]>(
      "GET",
      `/admin/floor/areas?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  createArea: (tenantId: string, storeId: string, name: string) =>
    requestJson<CreatedId>("POST", "/admin/floor/areas", {
      tenant_id: tenantId,
      store_id: storeId,
      name,
    }),
  updateArea: (
    areaId: string,
    tenantId: string,
    fields: { name: string; status: EntityStatus },
  ) =>
    requestVoid("PATCH", `/admin/floor/areas/${encodeURIComponent(areaId)}`, {
      tenant_id: tenantId,
      name: fields.name,
      status: fields.status,
    }),
  listTables: (tenantId: string, storeId: string) =>
    requestJson<FloorTable[]>(
      "GET",
      `/admin/floor/tables?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  createTable: (
    tenantId: string,
    storeId: string,
    fields: {
      areaId: string;
      name: string;
      seats: number;
      gridColumn: number | null;
      gridRow: number | null;
    },
  ) =>
    requestJson<CreatedId>("POST", "/admin/floor/tables", {
      tenant_id: tenantId,
      store_id: storeId,
      area_id: fields.areaId,
      name: fields.name,
      seats: fields.seats,
      grid_column: fields.gridColumn,
      grid_row: fields.gridRow,
    }),
  updateTable: (
    tableId: string,
    tenantId: string,
    fields: {
      areaId: string;
      name: string;
      seats: number;
      gridColumn: number | null;
      gridRow: number | null;
      status: EntityStatus;
    },
  ) =>
    requestVoid("PATCH", `/admin/floor/tables/${encodeURIComponent(tableId)}`, {
      tenant_id: tenantId,
      area_id: fields.areaId,
      name: fields.name,
      seats: fields.seats,
      grid_column: fields.gridColumn,
      grid_row: fields.gridRow,
      status: fields.status,
    }),
  listStations: (tenantId: string, storeId: string) =>
    requestJson<Station[]>(
      "GET",
      `/admin/kitchen/stations?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  createStation: (
    tenantId: string,
    storeId: string,
    fields: { name: string; backupStationId: string | null; isDefault: boolean },
  ) =>
    requestJson<CreatedId>("POST", "/admin/kitchen/stations", {
      tenant_id: tenantId,
      store_id: storeId,
      name: fields.name,
      backup_station_id: fields.backupStationId,
      is_default: fields.isDefault,
    }),
  updateStation: (
    stationId: string,
    tenantId: string,
    fields: {
      name: string;
      backupStationId: string | null;
      isDefault: boolean;
      status: EntityStatus;
    },
  ) =>
    requestVoid("PATCH", `/admin/kitchen/stations/${encodeURIComponent(stationId)}`, {
      tenant_id: tenantId,
      name: fields.name,
      backup_station_id: fields.backupStationId,
      is_default: fields.isDefault,
      status: fields.status,
    }),
  listRoutingRules: (tenantId: string, storeId: string) =>
    requestJson<RoutingRule[]>(
      "GET",
      `/admin/kitchen/routing?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  createRoutingRule: (
    tenantId: string,
    storeId: string,
    fields: {
      stationId: string;
      menuItemId: string | null;
      courseId: string | null;
      sort: number;
    },
  ) =>
    requestJson<CreatedId>("POST", "/admin/kitchen/routing", {
      tenant_id: tenantId,
      store_id: storeId,
      station_id: fields.stationId,
      menu_item_id: fields.menuItemId,
      course_id: fields.courseId,
      sort: fields.sort,
    }),
  removeRoutingRule: (tenantId: string, ruleId: string) =>
    requestVoid(
      "DELETE",
      `/admin/kitchen/routing/${encodeURIComponent(ruleId)}?${tenantQuery(tenantId)}`,
    ),
  // Compile the store's areas/tables + stations/routing into its `floor`/`stations` config nodes and
  // version them through the config tree, so the edge applies the real floor plan. Needs
  // console.config.publish; an inconsistent plan (a rule to an unknown station) answers 422.
  publishFloor: (tenantId: string, storeId: string) =>
    requestJson<PublishedConfig>("POST", "/admin/floor/publish", {
      tenant_id: tenantId,
      store_id: storeId,
    }),
  // Mint the signed QR token for each of a store's active tables, for a printable sheet (ADR-0072).
  // Only available when the cloud has a table-token secret configured (else the route is absent).
  tableQrTokens: (tenantId: string, storeId: string) =>
    requestJson<TableQrSheet>(
      "GET",
      `/admin/floor/qr?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),

  // --- org registry (ADR-0065): named Tenant/Brand/Store/Device, so a picker never shows a ULID ---
  listTenants: () => requestJson<Tenant[]>("GET", "/admin/tenants"),
  createTenant: (name: string) => requestJson<Tenant>("POST", "/admin/tenants", { name }),
  listBrands: (tenantId: string) =>
    requestJson<Brand[]>("GET", `/admin/brands?${tenantQuery(tenantId)}`),
  createBrand: (tenantId: string, name: string) =>
    requestJson<Brand>("POST", "/admin/brands", { tenant_id: tenantId, name }),
  listStores: (tenantId: string) =>
    requestJson<Store[]>("GET", `/admin/stores?${tenantQuery(tenantId)}`),
  createStore: (tenantId: string, name: string, brandId?: string) =>
    requestJson<Store>("POST", "/admin/stores", {
      tenant_id: tenantId,
      name,
      ...(brandId === undefined ? {} : { brand_id: brandId }),
    }),
  // `etag` is the version the caller read the store at (ADR-0094). A save against a version the
  // store no longer holds is refused with `412` (`ApiError.isStale`) rather than overwriting
  // whoever edited in between.
  updateStore: (
    storeId: string,
    tenantId: string,
    fields: { name: string; status: EntityStatus; brandId: string | null },
    etag: ETag,
  ) =>
    requestJsonIfMatch<Store>("PATCH", `/admin/stores/${encodeURIComponent(storeId)}`, etag, {
      tenant_id: tenantId,
      name: fields.name,
      status: fields.status,
      brand_id: fields.brandId,
    }),
  listDevices: (tenantId: string, storeId: string) =>
    requestJson<Device[]>(
      "GET",
      `/admin/stores/${encodeURIComponent(storeId)}/devices?${tenantQuery(tenantId)}`,
    ),
  createDevice: (tenantId: string, storeId: string, name: string, kind: string) =>
    requestJson<Device>("POST", `/admin/stores/${encodeURIComponent(storeId)}/devices`, {
      tenant_id: tenantId,
      name,
      kind,
    }),

  // --- catalog authoring (ADR-0066): items, tax classes, menus with inheritance, and placements ---
  listTaxClasses: (tenantId: string) =>
    requestJson<TaxClass[]>("GET", `/admin/catalog/tax-classes?${tenantQuery(tenantId)}`),
  createTaxClass: (tenantId: string, name: string) =>
    requestJson<TaxClass>("POST", "/admin/catalog/tax-classes", {
      tenant_id: tenantId,
      name,
    }),
  updateTaxClass: (
    taxClassId: string,
    tenantId: string,
    fields: { name: string; status: EntityStatus },
  ) =>
    requestJson<TaxClass>(
      "PATCH",
      `/admin/catalog/tax-classes/${encodeURIComponent(taxClassId)}`,
      { tenant_id: tenantId, name: fields.name, status: fields.status },
    ),
  // Tax rates (ADR-0074, Track M4): the per-(tax class × channel) rate the edge applies. `set`
  // replaces the tenant's whole table (behind console.catalog.manage); the read is behind
  // console.data.read.
  listTaxRates: (tenantId: string) =>
    requestJson<TaxRate[]>("GET", `/admin/catalog/tax-rates?${tenantQuery(tenantId)}`),
  setTaxRates: (tenantId: string, rates: readonly TaxRate[]) =>
    requestJson<TaxRate[]>("PUT", "/admin/catalog/tax-rates", { tenant_id: tenantId, rates }),
  // Publish the tenant's authored tax rates to one store's `tax` config node (ADR-0074), behind
  // console.config.publish; the edge applies it to its session's rate table.
  publishTax: (tenantId: string, storeId: string) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/tax", {
      tenant_id: tenantId,
      store_id: storeId,
    }),
  // Campaigns & scheduling (ADR-0077, Track M3). Per-campaign CRUD is behind console.campaigns.manage
  // (read behind console.data.read); the id is server-owned so create/update never send one. The
  // publish/preview/schedule routes push the tenant's authored campaigns to one store's `campaigns`
  // config node behind console.config.publish; the edge applies it.
  listCampaigns: (tenantId: string) =>
    requestJson<Campaign[]>("GET", `/admin/campaigns?${tenantQuery(tenantId)}`),
  createCampaign: (tenantId: string, input: CampaignInput) =>
    requestJson<Campaign>("POST", "/admin/campaigns", { tenant_id: tenantId, ...input }),
  updateCampaign: (tenantId: string, id: string, input: CampaignInput) =>
    requestJson<Campaign>("PUT", `/admin/campaigns/${encodeURIComponent(id)}`, {
      tenant_id: tenantId,
      ...input,
    }),
  deleteCampaign: (tenantId: string, id: string) =>
    requestVoid("DELETE", `/admin/campaigns/${encodeURIComponent(id)}?${tenantQuery(tenantId)}`),
  // Vouchers: mint a batch of codes for a voucher-kind campaign (returned once), or list a campaign's
  // codes. Both behind console.campaigns.manage — a code carries redeemable value.
  generateVouchers: (tenantId: string, campaignId: string, count: number) =>
    requestJson<Voucher[]>("POST", `/admin/campaigns/${encodeURIComponent(campaignId)}/vouchers`, {
      tenant_id: tenantId,
      count,
    }),
  listVouchers: (tenantId: string, campaignId: string) =>
    requestJson<Voucher[]>(
      "GET",
      `/admin/campaigns/${encodeURIComponent(campaignId)}/vouchers?${tenantQuery(tenantId)}`,
    ),
  // Preview the merge patch a campaigns publish would apply (no version minted, nothing saved), then
  // publish, or schedule the snapshot to publish at a future instant (the Tết-menu case).
  previewCampaigns: (tenantId: string, storeId: string) =>
    requestJson<CampaignPreview>("POST", "/admin/config/campaigns/preview", {
      tenant_id: tenantId,
      store_id: storeId,
    }),
  publishCampaigns: (tenantId: string, storeId: string) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/campaigns", {
      tenant_id: tenantId,
      store_id: storeId,
    }),
  scheduleCampaigns: (tenantId: string, storeId: string, effectiveAtMs: number) =>
    requestJson<ScheduledPublishCreated>("POST", "/admin/config/campaigns/schedule", {
      tenant_id: tenantId,
      store_id: storeId,
      effective_at_ms: effectiveAtMs,
    }),
  listScheduled: (tenantId: string, storeId: string) =>
    requestJson<ScheduledPublish[]>(
      "GET",
      `/admin/config/scheduled?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  cancelScheduled: (tenantId: string, id: string) =>
    requestVoid(
      "DELETE",
      `/admin/config/scheduled/${encodeURIComponent(id)}?${tenantQuery(tenantId)}`,
    ),
  // Channels & payments (ADR-0080, Track M7). Per-store settings nodes read and published through the
  // config tree behind console.config.publish; an absent node (null) means "no restriction". The edge
  // applies channels/tender as gates and qr as its staff-confirmation source; vendor policy is
  // authored here, its live marketplace loop deferred.
  readChannels: (tenantId: string, storeId: string) =>
    requestJson<{ enabled: SalesChannel[] } | null>(
      "GET",
      `/admin/config/channels?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  publishChannels: (tenantId: string, storeId: string, enabled: SalesChannel[]) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/channels", {
      tenant_id: tenantId,
      store_id: storeId,
      enabled,
    }),
  readTender: (tenantId: string, storeId: string) =>
    requestJson<{ accepted: string[] } | null>(
      "GET",
      `/admin/config/tender?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  publishTender: (tenantId: string, storeId: string, accepted: string[]) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/tender", {
      tenant_id: tenantId,
      store_id: storeId,
      accepted,
    }),
  readQrGuardrails: (tenantId: string, storeId: string) =>
    requestJson<QrGuardrails | null>(
      "GET",
      `/admin/config/qr?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  publishQrGuardrails: (tenantId: string, storeId: string, guardrails: QrGuardrails) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/qr", {
      tenant_id: tenantId,
      store_id: storeId,
      ...guardrails,
    }),
  readVendorPolicies: (tenantId: string, storeId: string) =>
    requestJson<{ policies: VendorPolicy[] } | null>(
      "GET",
      `/admin/config/vendors?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  publishVendorPolicies: (tenantId: string, storeId: string, policies: VendorPolicy[]) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/vendors", {
      tenant_id: tenantId,
      store_id: storeId,
      policies,
    }),
  // Inventory & suppliers (ADR-0079, Track M6). Per-record CRUD behind console.inventory.manage (read
  // behind console.data.read). An ingredient's and a supplier's id is server-owned, so create/update
  // never send one; a recipe's key is the menu item it makes, so it is a `PUT` upsert keyed by that id.
  listIngredients: (tenantId: string) =>
    requestJson<Ingredient[]>("GET", `/admin/inventory/ingredients?${tenantQuery(tenantId)}`),
  createIngredient: (tenantId: string, input: IngredientInput) =>
    requestJson<Ingredient>("POST", "/admin/inventory/ingredients", {
      tenant_id: tenantId,
      ...input,
    }),
  updateIngredient: (tenantId: string, id: string, input: IngredientInput) =>
    requestJson<Ingredient>("PUT", `/admin/inventory/ingredients/${encodeURIComponent(id)}`, {
      tenant_id: tenantId,
      ...input,
    }),
  deleteIngredient: (tenantId: string, id: string) =>
    requestVoid(
      "DELETE",
      `/admin/inventory/ingredients/${encodeURIComponent(id)}?${tenantQuery(tenantId)}`,
    ),
  listRecipes: (tenantId: string) =>
    requestJson<Recipe[]>("GET", `/admin/inventory/recipes?${tenantQuery(tenantId)}`),
  // Create or replace the recipe for one item (the item is the URL key, not a body field).
  upsertRecipe: (tenantId: string, item: string, input: RecipeInput) =>
    requestJson<Recipe>("PUT", `/admin/inventory/recipes/${encodeURIComponent(item)}`, {
      tenant_id: tenantId,
      ...input,
    }),
  deleteRecipe: (tenantId: string, item: string) =>
    requestVoid(
      "DELETE",
      `/admin/inventory/recipes/${encodeURIComponent(item)}?${tenantQuery(tenantId)}`,
    ),
  listSuppliers: (tenantId: string) =>
    requestJson<Supplier[]>("GET", `/admin/inventory/suppliers?${tenantQuery(tenantId)}`),
  createSupplier: (tenantId: string, input: SupplierInput) =>
    requestJson<Supplier>("POST", "/admin/inventory/suppliers", {
      tenant_id: tenantId,
      ...input,
    }),
  updateSupplier: (tenantId: string, id: string, input: SupplierInput) =>
    requestJson<Supplier>("PUT", `/admin/inventory/suppliers/${encodeURIComponent(id)}`, {
      tenant_id: tenantId,
      ...input,
    }),
  deleteSupplier: (tenantId: string, id: string) =>
    requestVoid(
      "DELETE",
      `/admin/inventory/suppliers/${encodeURIComponent(id)}?${tenantQuery(tenantId)}`,
    ),
  // Assemble the tenant's authored inventory into one store's `inventory` config node, behind
  // console.config.publish; the edge applies it to build its RecipeBook and auto-86 thresholds.
  publishInventory: (tenantId: string, storeId: string) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/inventory", {
      tenant_id: tenantId,
      store_id: storeId,
    }),
  // Countries & locales (ADR-0074): read-only master data compiled into the cloud — the currency
  // picker and the translation grid's locale catalogue. Global reads, behind console.data.read.
  listCountries: () => requestJson<Country[]>("GET", "/admin/countries"),
  listLocales: () => requestJson<string[]>("GET", "/admin/locales"),
  // Publish a store's locale settings (ADR-0074) as its `locale` config node, behind
  // console.config.publish; the edge applies the currency, timezone, and business-date cutoff.
  publishLocale: (
    tenantId: string,
    storeId: string,
    settings: {
      currency_code: string;
      timezone: string;
      cutoff_hour: number;
      display_language?: string;
    },
  ) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/locale", {
      tenant_id: tenantId,
      store_id: storeId,
      currency_code: settings.currency_code,
      timezone: settings.timezone,
      cutoff_hour: settings.cutoff_hour,
      display_language: settings.display_language ?? null,
    }),

  // --- media (ADR-0075) ---
  // Upload an image; the server re-encodes it to two bounded renditions and returns the new id.
  uploadMedia: (tenantId: string, file: Blob) =>
    requestUpload<UploadedMedia>(`/admin/media?${tenantQuery(tenantId)}`, file),
  listMedia: (tenantId: string) =>
    requestJson<MediaSummary[]>("GET", `/admin/media?${tenantQuery(tenantId)}`),
  deleteMedia: (tenantId: string, mediaId: string) =>
    requestVoid(
      "DELETE",
      `/admin/media/${encodeURIComponent(mediaId)}?${tenantQuery(tenantId)}`,
    ),
  // --- CSV export rail (ADR-0075, Track M5) ---
  // Each export is permission-gated and audited server-side; the download carries the session cookie.
  exportItemsCsv: (tenantId: string) =>
    downloadCsv(`/admin/catalog/export/items?${tenantQuery(tenantId)}`, "items.csv"),
  exportTranslationsCsv: (tenantId: string) =>
    downloadCsv(`/admin/translations/export?${tenantQuery(tenantId)}`, "translations.csv"),
  // Reports CSV exports (ADR-0081, Track O4), windowed like the reads. Revenue is T2 (Owner/Admin).
  exportRollupsCsv: (
    tenantId: string,
    storeId: string,
    window?: { from?: string; to?: string; limit?: number },
  ) =>
    downloadCsv(
      `/admin/stores/${encodeURIComponent(storeId)}/rollups/export?${rollupWindowQuery(tenantId, window)}`,
      "rollups.csv",
    ),
  exportRevenueCsv: (
    tenantId: string,
    storeId: string,
    window?: { from?: string; to?: string; limit?: number },
  ) =>
    downloadCsv(
      `/admin/stores/${encodeURIComponent(storeId)}/revenue/export?${rollupWindowQuery(tenantId, window)}`,
      "revenue.csv",
    ),
  // Dry-run classifies every row and writes nothing; apply merges the valid rows on confirm.
  dryRunTranslationsCsv: (tenantId: string, file: Blob) =>
    requestUpload<TranslationImportReport>(
      `/admin/translations/import/dry-run?${tenantQuery(tenantId)}`,
      file,
    ),
  applyTranslationsCsv: (tenantId: string, file: Blob) =>
    requestUpload<TranslationImportReport>(
      `/admin/translations/import/apply?${tenantQuery(tenantId)}`,
      file,
    ),

  // --- subject-request tooling (ADR-0076, owner-only, per-subject, audited) ---
  // A 404 (no such subject for this tenant) surfaces as an ApiError the caller distinguishes by status.
  lookupSubject: (tenantId: string, subjectId: string) =>
    requestJson<SubjectMeta>(
      "GET",
      `/admin/subjects/${encodeURIComponent(subjectId)}?${tenantQuery(tenantId)}`,
    ),
  exportSubject: (tenantId: string, subjectId: string) =>
    requestJson<SubjectExport>(
      "GET",
      `/admin/subjects/${encodeURIComponent(subjectId)}/export?${tenantQuery(tenantId)}`,
    ),
  eraseSubject: (tenantId: string, subjectId: string) =>
    requestJson<{ readonly erased: boolean; readonly already_masked: boolean }>(
      "POST",
      `/admin/subjects/${encodeURIComponent(subjectId)}/erase?${tenantQuery(tenantId)}`,
    ),

  // The `<img src>` URL for a rendition; the browser fetches it with the session cookie.
  mediaThumbnailUrl: (tenantId: string, mediaId: string) =>
    `/admin/media/${encodeURIComponent(mediaId)}/thumbnail?${tenantQuery(tenantId)}`,
  mediaDetailUrl: (tenantId: string, mediaId: string) =>
    `/admin/media/${encodeURIComponent(mediaId)}/detail?${tenantQuery(tenantId)}`,

  listItemCategories: (tenantId: string) =>
    requestJson<ItemCategory[]>("GET", `/admin/catalog/item-categories?${tenantQuery(tenantId)}`),
  createItemCategory: (tenantId: string, name: string) =>
    requestJson<ItemCategory>("POST", "/admin/catalog/item-categories", {
      tenant_id: tenantId,
      name,
    }),
  updateItemCategory: (
    itemCategoryId: string,
    tenantId: string,
    fields: { name: string; status: EntityStatus },
  ) =>
    requestJson<ItemCategory>(
      "PATCH",
      `/admin/catalog/item-categories/${encodeURIComponent(itemCategoryId)}`,
      { tenant_id: tenantId, name: fields.name, status: fields.status },
    ),
  listItemSubcategories: (tenantId: string) =>
    requestJson<ItemSubcategory[]>(
      "GET",
      `/admin/catalog/item-subcategories?${tenantQuery(tenantId)}`,
    ),
  createItemSubcategory: (tenantId: string, itemCategoryId: string, name: string) =>
    requestJson<ItemSubcategory>("POST", "/admin/catalog/item-subcategories", {
      tenant_id: tenantId,
      item_category_id: itemCategoryId,
      name,
    }),
  updateItemSubcategory: (
    itemSubcategoryId: string,
    tenantId: string,
    fields: { itemCategoryId: string; name: string; status: EntityStatus },
  ) =>
    requestJson<ItemSubcategory>(
      "PATCH",
      `/admin/catalog/item-subcategories/${encodeURIComponent(itemSubcategoryId)}`,
      {
        tenant_id: tenantId,
        item_category_id: fields.itemCategoryId,
        name: fields.name,
        status: fields.status,
      },
    ),
  listItems: (tenantId: string) =>
    requestJson<CatalogItem[]>("GET", `/admin/catalog/items?${tenantQuery(tenantId)}`),
  createItem: (
    tenantId: string,
    name: string,
    taxClassId: string,
    taxonomy: {
      itemCategoryId: string | null;
      itemSubcategoryId: string | null;
      nameTranslations?: Record<string, string>;
      imageRef?: string | null;
    },
  ) =>
    requestJson<CatalogItem>("POST", "/admin/catalog/items", {
      tenant_id: tenantId,
      name,
      name_translations: taxonomy.nameTranslations ?? {},
      tax_class_id: taxClassId,
      item_category_id: taxonomy.itemCategoryId,
      item_subcategory_id: taxonomy.itemSubcategoryId,
      image_ref: taxonomy.imageRef ?? null,
    }),
  updateItem: (
    menuItemId: string,
    tenantId: string,
    fields: {
      name: string;
      nameTranslations?: Record<string, string>;
      taxClassId: string;
      itemCategoryId: string | null;
      itemSubcategoryId: string | null;
      imageRef?: string | null;
      status: EntityStatus;
    },
  ) =>
    requestJson<CatalogItem>("PATCH", `/admin/catalog/items/${encodeURIComponent(menuItemId)}`, {
      tenant_id: tenantId,
      name: fields.name,
      name_translations: fields.nameTranslations ?? {},
      tax_class_id: fields.taxClassId,
      item_category_id: fields.itemCategoryId,
      item_subcategory_id: fields.itemSubcategoryId,
      image_ref: fields.imageRef ?? null,
      status: fields.status,
    }),
  listMenus: (tenantId: string) =>
    requestJson<Menu[]>("GET", `/admin/catalog/menus?${tenantQuery(tenantId)}`),
  createMenu: (tenantId: string, name: string, parentMenuId?: string) =>
    requestJson<Menu>("POST", "/admin/catalog/menus", {
      tenant_id: tenantId,
      name,
      parent_menu_id: parentMenuId ?? null,
    }),
  updateMenu: (
    menuId: string,
    tenantId: string,
    fields: { name: string; parentMenuId: string | null; status: EntityStatus },
  ) =>
    requestJson<Menu>("PATCH", `/admin/catalog/menus/${encodeURIComponent(menuId)}`, {
      tenant_id: tenantId,
      name: fields.name,
      parent_menu_id: fields.parentMenuId,
      status: fields.status,
    }),
  listPlacements: (tenantId: string, menuId: string) =>
    requestJson<MenuPlacement[]>(
      "GET",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/placements?${tenantQuery(tenantId)}`,
    ),
  setPlacement: (
    tenantId: string,
    menuId: string,
    menuItemId: string,
    prices: ChannelPrice[],
    available: boolean,
    menuSectionId: string | null,
  ) =>
    requestVoid(
      "PUT",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/placements/${encodeURIComponent(menuItemId)}`,
      { tenant_id: tenantId, menu_section_id: menuSectionId, prices, available },
    ),
  deletePlacement: (tenantId: string, menuId: string, menuItemId: string) =>
    requestVoid(
      "DELETE",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/placements/${encodeURIComponent(menuItemId)}?${tenantQuery(tenantId)}`,
    ),

  // --- menu sections (ADR-0066 entity 7): authoring groupings within a menu ---
  listMenuSections: (tenantId: string, menuId: string) =>
    requestJson<MenuSection[]>(
      "GET",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/sections?${tenantQuery(tenantId)}`,
    ),
  createMenuSection: (tenantId: string, menuId: string, name: string, sort: number) =>
    requestJson<MenuSection>(
      "POST",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/sections`,
      { tenant_id: tenantId, name, sort },
    ),
  updateMenuSection: (
    tenantId: string,
    menuId: string,
    menuSectionId: string,
    fields: { name: string; sort: number; status: EntityStatus },
  ) =>
    requestJson<MenuSection>(
      "PATCH",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/sections/${encodeURIComponent(menuSectionId)}`,
      { tenant_id: tenantId, name: fields.name, sort: fields.sort, status: fields.status },
    ),
  publishMenu: (tenantId: string, storeId: string, menuId: string) =>
    requestJson<PublishedConfig>("POST", "/admin/catalog/publish", {
      tenant_id: tenantId,
      store_id: storeId,
      menu_id: menuId,
    }),

  // --- modifier groups (ADR-0066 entities 4/5): a selection rule + members, attached to items ---
  listModifierGroups: (tenantId: string) =>
    requestJson<ModifierGroup[]>("GET", `/admin/catalog/modifier-groups?${tenantQuery(tenantId)}`),
  createModifierGroup: (
    tenantId: string,
    fields: {
      name: string;
      minSelect: number;
      maxSelect: number;
      memberItemIds: string[];
      attachedItemIds: string[];
    },
  ) =>
    requestJson<ModifierGroup>("POST", "/admin/catalog/modifier-groups", {
      tenant_id: tenantId,
      name: fields.name,
      min_select: fields.minSelect,
      max_select: fields.maxSelect,
      member_item_ids: fields.memberItemIds,
      attached_item_ids: fields.attachedItemIds,
    }),
  updateModifierGroup: (
    modifierGroupId: string,
    tenantId: string,
    fields: {
      name: string;
      minSelect: number;
      maxSelect: number;
      memberItemIds: string[];
      attachedItemIds: string[];
      status: EntityStatus;
    },
  ) =>
    requestJson<ModifierGroup>(
      "PATCH",
      `/admin/catalog/modifier-groups/${encodeURIComponent(modifierGroupId)}`,
      {
        tenant_id: tenantId,
        name: fields.name,
        min_select: fields.minSelect,
        max_select: fields.maxSelect,
        member_item_ids: fields.memberItemIds,
        attached_item_ids: fields.attachedItemIds,
        status: fields.status,
      },
    ),

  // --- presentation tier (ADR-0066): display taxonomy + per-channel layout buttons ---
  listDisplayCategories: (tenantId: string) =>
    requestJson<DisplayCategory[]>(
      "GET",
      `/admin/catalog/display-categories?${tenantQuery(tenantId)}`,
    ),
  createDisplayCategory: (tenantId: string, name: string) =>
    requestJson<DisplayCategory>("POST", "/admin/catalog/display-categories", {
      tenant_id: tenantId,
      name,
    }),
  updateDisplayCategory: (
    displayCategoryId: string,
    tenantId: string,
    fields: { name: string; status: EntityStatus },
  ) =>
    requestJson<DisplayCategory>(
      "PATCH",
      `/admin/catalog/display-categories/${encodeURIComponent(displayCategoryId)}`,
      { tenant_id: tenantId, name: fields.name, status: fields.status },
    ),
  listDisplaySubcategories: (tenantId: string) =>
    requestJson<DisplaySubcategory[]>(
      "GET",
      `/admin/catalog/display-subcategories?${tenantQuery(tenantId)}`,
    ),
  createDisplaySubcategory: (tenantId: string, displayCategoryId: string, name: string) =>
    requestJson<DisplaySubcategory>("POST", "/admin/catalog/display-subcategories", {
      tenant_id: tenantId,
      display_category_id: displayCategoryId,
      name,
    }),
  updateDisplaySubcategory: (
    displaySubcategoryId: string,
    tenantId: string,
    fields: { displayCategoryId: string; name: string; status: EntityStatus },
  ) =>
    requestJson<DisplaySubcategory>(
      "PATCH",
      `/admin/catalog/display-subcategories/${encodeURIComponent(displaySubcategoryId)}`,
      {
        tenant_id: tenantId,
        display_category_id: fields.displayCategoryId,
        name: fields.name,
        status: fields.status,
      },
    ),
  listLayoutButtons: (tenantId: string) =>
    requestJson<LayoutButton[]>("GET", `/admin/catalog/layout-buttons?${tenantQuery(tenantId)}`),
  setLayoutButton: (
    tenantId: string,
    salesChannel: SalesChannel,
    menuItemId: string,
    fields: {
      displayCategoryId: string;
      displaySubcategoryId: string | null;
      label: string;
      gridColumn: number | null;
      gridRow: number | null;
      sort: number;
    },
  ) =>
    requestJson<LayoutButton>(
      "PUT",
      `/admin/catalog/layout-buttons/${encodeURIComponent(salesChannel)}/${encodeURIComponent(menuItemId)}`,
      {
        tenant_id: tenantId,
        display_category_id: fields.displayCategoryId,
        display_subcategory_id: fields.displaySubcategoryId,
        label: fields.label,
        grid_column: fields.gridColumn,
        grid_row: fields.gridRow,
        sort: fields.sort,
      },
    ),
  removeLayoutButton: (tenantId: string, salesChannel: SalesChannel, menuItemId: string) =>
    requestVoid(
      "DELETE",
      `/admin/catalog/layout-buttons/${encodeURIComponent(salesChannel)}/${encodeURIComponent(menuItemId)}?${tenantQuery(tenantId)}`,
    ),

  // --- fleet liveness + background-task health (ADR-0068, Track O1) ---
  // The fleet read is tenant-scoped (a store's liveness is its tenant's data); the task-health read is
  // fleet-wide server state (the loops run once per cloud). Both are behind console.data.read.
  listFleet: (tenantId: string) =>
    requestJson<FleetStore[]>("GET", `/admin/fleet?${tenantQuery(tenantId)}`),
  fleetStore: (tenantId: string, storeId: string) =>
    requestJson<FleetStore>(
      "GET",
      `/admin/fleet/${encodeURIComponent(storeId)}?${tenantQuery(tenantId)}`,
    ),
  taskHealth: () => requestJson<TaskHealthReport>("GET", "/admin/health/tasks"),

  // --- OTA rollout levers (ADR-0078, Track O3) ---
  // The published rollout is a store's `fleet_update` config node. Reading it is behind
  // console.data.read; publishing a rollout or flipping its kill switch is behind console.ota.publish
  // (Owner/Admin only) and audited. Publish composes the node from typed fields and validates it the
  // same way the generic config publish did; halt loads the published rollout, flips `halted`, and
  // re-publishes without re-typing it.
  getOtaRollout: (tenantId: string, storeId: string) =>
    requestJson<OtaRollout | null>(
      "GET",
      `/admin/config/ota?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  publishOtaRollout: (request: PublishRolloutRequest) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/ota", request),
  haltOtaRollout: (tenantId: string, storeId: string, halted: boolean) =>
    requestJson<PublishedConfig>("POST", "/admin/config/ota/halt", {
      tenant_id: tenantId,
      store_id: storeId,
      halted,
    }),

  // The placement half (ADR-0052): a rollout says which devices are eligible, a placement says where
  // this store sits — its ring and its stable canary bucket. A store that has never been placed reads
  // `null` and installs nothing, so both halves have to be authored for a fleet to move. Same
  // permissions as the rollout: read behind console.data.read, publish behind console.ota.publish.
  getOtaPlacement: (tenantId: string, storeId: string) =>
    requestJson<OtaPlacement | null>(
      "GET",
      `/admin/config/ota/placement?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  publishOtaPlacement: (request: PublishPlacementRequest) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/ota/placement", request),

  // --- reconciliation run history (ADR-0078, Track O3) ---
  // The trail of reconciliation diffs (ADR-0040): counts and a timestamp per run, newest first,
  // behind console.data.read. Tenant-scoped; an optional storeId narrows to one store.
  listReconcileRuns: (tenantId: string, storeId?: string) => {
    const params = new URLSearchParams({ tenant_id: tenantId });
    if (storeId) params.set("store_id", storeId);
    return requestJson<ReconcileRun[]>("GET", `/admin/reconcile?${params.toString()}`);
  },

  // --- console audit trail (ADR-0069, Track G2) ---
  // A fleet-wide, filterable read of who changed what, behind console.data.read. Every filter is
  // optional; an absent `tenantId` reads across every tenant (including tenant-global entries).
  listAudit: (filter: AuditFilter = {}) => {
    const params = new URLSearchParams();
    if (filter.tenantId) params.set("tenant_id", filter.tenantId);
    if (filter.entityType) params.set("entity_type", filter.entityType);
    if (filter.entityId) params.set("entity_id", filter.entityId);
    if (filter.action) params.set("action", filter.action);
    if (filter.actorAdminId) params.set("actor_admin_id", filter.actorAdminId);
    if (filter.limit !== undefined) params.set("limit", String(filter.limit));
    const query = params.toString();
    return requestJson<AuditEntry[]>("GET", query ? `/admin/audit?${query}` : "/admin/audit");
  },

  // --- operational alerts (ADR-0073, Track O2) ---
  // A fleet-wide read of the alerts the evaluator maintains, behind console.data.read: the active set
  // by default, or recent history (active + resolved) with `recent`. Acknowledge/resolve need
  // console.alerts.manage (Owner/Admin/Ops) and are audited; both are idempotent.
  listAlerts: (recent = false, limit?: number) => {
    const params = new URLSearchParams();
    if (recent) params.set("recent", "true");
    if (limit !== undefined) params.set("limit", String(limit));
    const query = params.toString();
    return requestJson<Alert[]>("GET", query ? `/admin/alerts?${query}` : "/admin/alerts");
  },
  acknowledgeAlert: (id: string) =>
    requestVoid("POST", `/admin/alerts/${encodeURIComponent(id)}/ack`),
  resolveAlert: (id: string) =>
    requestVoid("POST", `/admin/alerts/${encodeURIComponent(id)}/resolve`),
};
