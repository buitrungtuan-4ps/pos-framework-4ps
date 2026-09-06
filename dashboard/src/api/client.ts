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
  CreateTerminalResponse,
  DeviceProposalSummary,
  DisplayCategory,
  DisplaySubcategory,
  Employee,
  EntityStatus,
  ETag,
  Enrolment,
  FleetStore,
  StoreLease,
  Ingredient,
  IngredientInput,
  InviteAdminResponse,
  ItemCategory,
  ItemListFilter,
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
  TrailOrder,
  TranslationGrid,
  TranslationImportReport,
  Page,
  PageRequest,
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
  // Two body shapes reach here, and both have to be readable.
  //
  // The cloud is being migrated onto the AIP-193 envelope — `{"error":{code,status,message,details}}`
  // — one group of handlers at a time (roadmap v3 Q3), so at any point in that migration some
  // handlers answer the envelope and the rest answer plain text. Reading both is what lets the
  // conversion land in reviewable slices instead of one 619-site commit: no intermediate state can
  // strand the console on a body it cannot parse.
  //
  // There used to be a third branch here, for a rejected config publish's `{"violations":[...]}`,
  // and the comment above it claimed three shapes when there were four: the translation grid's
  // `{"missing_fallback":[...]}` had no branch at all, so an operator saw raw JSON in a dialog.
  // ADR-0096 removed the cause rather than adding a fourth parser — those refusals now carry
  // `UNPROCESSABLE` in the envelope, with the offending keys in `details`.
  if (trimmed.startsWith("{")) {
    try {
      const body = JSON.parse(trimmed) as {
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
    } catch {
      // Not the envelope; fall through to the raw text.
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
  return requestJsonIfMatchRaw<T>(method, path, `"${etag}"`, body);
}

// The same conditional write with the header value formed by the caller, for the config tree
// (ADR-0095). Its precondition is not always an entity-tag: a store that has never been published
// to is asserted with `If-Match: *`, which the record-shaped routes above refuse and this one
// requires. Passing the formed value through keeps that grammar in one place — `configPrecondition`
// — instead of teaching this helper a second shape.
async function requestJsonIfMatchRaw<T>(
  method: string,
  path: string,
  ifMatch: string,
  body?: unknown,
): Promise<T> {
  const response = await fetch(path, {
    method,
    headers: {
      "if-match": ifMatch,
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    throw await failure(response);
  }
  return (await response.json()) as T;
}

// A layout button's presentation, as the two write routes take it. Named because the create and the
// update carry exactly the same fields — only the identity differs, and it moves from the body to
// the path (ADR-0095).
interface LayoutButtonFields {
  displayCategoryId: string;
  displaySubcategoryId: string | null;
  label: string;
  gridColumn: number | null;
  gridRow: number | null;
  sort: number;
}

// The wire body for those fields, so the two routes cannot drift on a field name.
function layoutButtonBody(fields: LayoutButtonFields): Record<string, unknown> {
  return {
    display_category_id: fields.displayCategoryId,
    display_subcategory_id: fields.displaySubcategoryId,
    label: fields.label,
    grid_column: fields.gridColumn,
    grid_row: fields.gridRow,
    sort: fields.sort,
  };
}

// The `If-Match` a config publish, a rollback, or a whole-collection save carries: the version it was
// read at, or `*` for a tree or collection nothing has been saved into yet. `*` is an assertion, not
// a waiver — the server refuses it once a version exists (ADR-0095).
function precondition(version: string | null): string {
  return version === null ? "*" : `"${version}"`;
}

// A read that also hands back the `ETag` the response carried. Collections put their version in the
// header rather than the body, because a JSON array has nowhere to put a field (ADR-0095); `null`
// means nothing has been saved yet, which is what the next write asserts with `*`.
async function requestJsonWithEtag<T>(path: string): Promise<{ value: T; etag: string | null }> {
  const response = await fetch(path, { headers: { accept: "application/json" } });
  if (!response.ok) {
    throw await failure(response);
  }
  const raw = response.headers.get("etag");
  return {
    value: (await response.json()) as T,
    etag: raw === null ? null : raw.replace(/^"|"$/g, ""),
  };
}

// The `requestJsonIfMatchRaw` shape for a route that answers `204`.
async function requestVoidIfMatchRaw(
  method: string,
  path: string,
  ifMatch: string,
  body?: unknown,
): Promise<void> {
  const response = await fetch(path, {
    method,
    headers: {
      "if-match": ifMatch,
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    throw await failure(response);
  }
}

// The same conditional write as `requestJsonIfMatch`, for the routes that answer `204` rather than
// the updated record — areas, tables, stations, employees and role templates, whose PATCH payloads
// do not carry every field, so the server has no representation to return that it did not invent
// (ADR-0094). The new version comes back in the `ETag` header; every screen reloads after a write,
// so nothing here reads it.
async function requestVoidIfMatch(
  method: string,
  path: string,
  etag: ETag,
  body?: unknown,
): Promise<void> {
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

/**
 * `tenant_id` plus the paging bounds, when a caller wants a page (ADR-0098).
 *
 * Omitting `page` omits `limit` entirely, which is what asks for the whole set — the cloud has no
 * default limit and will not invent one, so a picker that needs every row simply does not pass this.
 */
const tenantPageQuery = (tenantId: string, page?: PageRequest) => {
  const parts = [tenantQuery(tenantId)];
  if (page) {
    parts.push(`limit=${encodeURIComponent(String(page.limit))}`);
    if (page.offset) {
      parts.push(`offset=${encodeURIComponent(String(page.offset))}`);
    }
  }
  return parts.join("&");
};

/**
 * The audit filters as query parameters, shared by the windowed and paged reads.
 *
 * One builder so a filter cannot be sent by one read and dropped by the other — which would make a
 * page a window onto a different set than its own total counts.
 */
const auditFilterParams = (filter: AuditFilter) => {
  const params = new URLSearchParams();
  if (filter.tenantId) params.set("tenant_id", filter.tenantId);
  if (filter.entityType) params.set("entity_type", filter.entityType);
  if (filter.entityId) params.set("entity_id", filter.entityId);
  if (filter.action) params.set("action", filter.action);
  if (filter.actorAdminId) params.set("actor_admin_id", filter.actorAdminId);
  if (filter.limit !== undefined) params.set("limit", String(filter.limit));
  return params;
};

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
  // `storeId` binds the key to one store (S1). A store's own credential — the key its edge presents
  // on `/sync/stores/{id}/…` — must carry it: those routes serve one store's configuration, employee
  // roster included, and refuse a key that names another store or none. Omit it for an integration
  // key that reads a whole tenant.
  createApiKey: (tenantId: string, scopes: string[], storeId?: string, expiresAtMs?: number) =>
    requestJson<CreateApiKeyResponse>("POST", "/admin/api-keys", {
      tenant_id: tenantId,
      scopes,
      ...(storeId === undefined || storeId === "" ? {} : { store_id: storeId }),
      ...(expiresAtMs === undefined ? {} : { expires_at_ms: expiresAtMs }),
    }),
  revokeApiKey: (id: string) => requestVoid("DELETE", `/admin/api-keys/${encodeURIComponent(id)}`),

  // --- config tree (ADR-0033) ---
  effectiveConfig: (tenantId: string, storeId: string) =>
    requestJsonOrNull<Json>(
      `/admin/stores/${encodeURIComponent(storeId)}/config?${tenantQuery(tenantId)}`,
    ),
  // `version` is the config version the tree was read at, or `null` for a store that has never been
  // published to (ADR-0095). Both are preconditions: a publish made against a version the tree no
  // longer holds is refused with `412`, and so is a "nothing here yet" claim about a store that has
  // since been published to. The ten *node* publishes below carry no precondition — they write one
  // key of a layer and the server retries them around a competing write of a different key.
  publishConfig: (
    tenantId: string,
    storeId: string,
    level: ConfigLevel,
    document: Json,
    version: string | null,
  ) =>
    requestJsonIfMatchRaw<PublishedConfig>(
      "PUT",
      `/admin/stores/${encodeURIComponent(storeId)}/config/${level}?${tenantQuery(tenantId)}`,
      precondition(version),
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
  // `version` is the current config version as the screen last read it (ADR-0095). A rollback
  // composes on what it read — it restores an *earlier* document over the current one — so a publish
  // that landed while the operator was choosing makes the rollback a clobber, and it is refused.
  rollbackConfig: (tenantId: string, storeId: string, versionId: string, version: string | null) =>
    requestJsonIfMatchRaw<PublishedConfig>(
      "POST",
      `/admin/stores/${encodeURIComponent(storeId)}/config/rollback?${tenantQuery(tenantId)}`,
      precondition(version),
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
  // The same read, narrowed to one store and one status — what the print-agent picker needs
  // (ADR-0112). Both halves of a binding are `approved` and stand in the same shop: the printer
  // whose bytes are being redirected, and the terminal that will write them. The onboarding queue's
  // read can show neither, which is why the parameters exist.
  listStoreDevices: (tenantId: string, storeId: string, status: string) =>
    requestJson<DeviceProposalSummary[]>(
      "GET",
      `/admin/devices/proposals?${tenantQuery(tenantId)}` +
        `&store_id=${encodeURIComponent(storeId)}&status=${encodeURIComponent(status)}`,
    ),
  // Approving carries the two facts discovery cannot find (ADR-0100): how the device is attached,
  // which decides whether a cash drawer may be opened at all, and the kitchen station it serves —
  // omitted for the counter's receipt printer, which serves the bill rather than a station. The
  // route refuses an approval with no connection rather than guessing one.
  approveDevice: (tenantId: string, id: string, connection: string, stationId?: string) =>
    requestVoid(
      "POST",
      `/admin/devices/proposals/${encodeURIComponent(id)}/approve?${tenantQuery(tenantId)}` +
        `&connection=${encodeURIComponent(connection)}` +
        (stationId ? `&station_id=${encodeURIComponent(stationId)}` : ""),
    ),
  rejectDevice: (tenantId: string, id: string) =>
    requestVoid(
      "POST",
      `/admin/devices/proposals/${encodeURIComponent(id)}/reject?${tenantQuery(tenantId)}`,
    ),
  // A terminal is *created*, never proposed (ADR-0112): nothing on a LAN announces itself as a
  // till, so there is no discovery for an operator to approve and the console write is itself the
  // decision. It carries a name and no address — the agent dials out to the edge, and nothing dials
  // a terminal.
  createTerminal: (tenantId: string, storeId: string, name: string) =>
    requestJson<CreateTerminalResponse>("POST", "/admin/devices/terminals", {
      tenant_id: tenantId,
      store_id: storeId,
      name,
    }),
  // Picking (or clearing) the terminal whose agent writes a printer's bytes, conditional on the
  // version the row was read at (ADR-0094). Two managers picking different agents for one printer
  // is the ordinary race, and last-write-wins would leave the loser believing a decision that is
  // not in the database. `null` hands the printer back to the edge, which opens the address itself.
  setPrintAgent: (
    tenantId: string,
    id: string,
    agentDeviceId: string | null,
    version: string,
  ) =>
    requestVoidIfMatch(
      "POST",
      `/admin/devices/proposals/${encodeURIComponent(id)}/agent`,
      version,
      { tenant_id: tenantId, agent_device_id: agentDeviceId },
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
  // The grid is one collection the console edits whole, so its version rides on the `ETag` of the
  // read and comes back as `If-Match` on the save (ADR-0095). `etag: null` means this tenant has
  // authored no grid yet. The CSV import needs no version: it merges, so the server retries it.
  getTranslations: (tenantId: string) =>
    requestJsonWithEtag<TranslationGrid>(`/admin/translations?${tenantQuery(tenantId)}`),
  putTranslations: (tenantId: string, grid: TranslationGrid, version: string | null) =>
    requestVoidIfMatchRaw(
      "PUT",
      `/admin/translations?${tenantQuery(tenantId)}`,
      precondition(version),
      grid,
    ),

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
  // The whole roster, unpaged — what the publish path needs: the permission node is compiled from
  // every employee, and a node built from a page would be missing whoever fell off it (ADR-0098).
  listEmployees: (tenantId: string) =>
    requestJson<Employee[]>("GET", `/admin/employees?${tenantQuery(tenantId)}`),
  // One page of the roster. This is T1 personal data either way (ADR-0070) — paging does not change
  // the fields or the gate, it sends fewer rows per response than the read above it.
  //
  // `search`, when given, narrows on the person's name or staff code and narrows `total` with it.
  // That is what the assign picker calls: it asks for a short page of whoever matches what the
  // operator typed, instead of holding the roster in memory to filter locally. The server refuses a
  // search without a limit rather than answering with the whole roster, so the two travel together.
  //
  // `sort`/`order` are the roster's orders (`newest`, `name`, `code`) and `asc`/`desc`. The People
  // table calls this with all four; the assign picker calls it with a search and a short limit.
  listEmployeesPage: (
    tenantId: string,
    page: PageRequest,
    search?: string,
    sort?: string,
    order?: string,
  ) =>
    requestJson<Page<Employee>>(
      "GET",
      `/admin/employees?${tenantPageQuery(tenantId, page)}${
        search ? `&q=${encodeURIComponent(search)}` : ""
      }${sort ? `&sort=${encodeURIComponent(sort)}` : ""}${
        order ? `&order=${encodeURIComponent(order)}` : ""
      }`,
    ),
  getEmployee: (tenantId: string, id: string) =>
    requestJson<Employee>(
      "GET",
      `/admin/employees/${encodeURIComponent(id)}?${tenantQuery(tenantId)}`,
    ),
  createEmployee: (tenantId: string, code: string, name: string) =>
    requestJson<CreatedId>("POST", "/admin/employees", { tenant_id: tenantId, code, name }),
  updateEmployee: (
    id: string,
    tenantId: string,
    fields: { name: string; status: EntityStatus },
    etag: ETag,
  ) =>
    requestVoidIfMatch("PATCH", `/admin/employees/${encodeURIComponent(id)}`, etag, {
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
    etag: ETag,
  ) =>
    requestVoidIfMatch("PATCH", `/admin/roles/${encodeURIComponent(id)}`, etag, {
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
    etag: ETag,
  ) =>
    requestVoidIfMatch("PATCH", `/admin/floor/areas/${encodeURIComponent(areaId)}`, etag, {
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
    etag: ETag,
  ) =>
    requestVoidIfMatch("PATCH", `/admin/floor/tables/${encodeURIComponent(tableId)}`, etag, {
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
    etag: ETag,
  ) =>
    requestVoidIfMatch("PATCH", `/admin/kitchen/stations/${encodeURIComponent(stationId)}`, etag, {
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
  // Rename or archive a tenant (ADR-0065, production-readiness O2). `etag` is the version the caller
  // read it at (ADR-0094): a save against a version the registry no longer holds is refused `412`
  // rather than overwriting whoever edited in between.
  updateTenant: (tenantId: string, fields: { name: string; status: EntityStatus }, etag: ETag) =>
    requestJsonIfMatch<Tenant>("PATCH", `/admin/tenants/${encodeURIComponent(tenantId)}`, etag, {
      name: fields.name,
      status: fields.status,
    }),
  listBrands: (tenantId: string) =>
    requestJson<Brand[]>("GET", `/admin/brands?${tenantQuery(tenantId)}`),
  createBrand: (tenantId: string, name: string) =>
    requestJson<Brand>("POST", "/admin/brands", { tenant_id: tenantId, name }),
  updateBrand: (
    brandId: string,
    tenantId: string,
    fields: { name: string; status: EntityStatus },
    etag: ETag,
  ) =>
    requestJsonIfMatch<Brand>("PATCH", `/admin/brands/${encodeURIComponent(brandId)}`, etag, {
      tenant_id: tenantId,
      name: fields.name,
      status: fields.status,
    }),
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
  // Rename, re-kind, or archive a registry device (ADR-0065, production-readiness O2). Archiving is
  // how a device that was replaced leaves the roster — it does not retire a paired *till*, which is
  // the store server's own `POST /api/pair/revoke`, a different credential in a different tier.
  updateDevice: (
    deviceId: string,
    tenantId: string,
    storeId: string,
    fields: { name: string; kind: string; status: EntityStatus },
    etag: ETag,
  ) =>
    requestJsonIfMatch<Device>(
      "PATCH",
      `/admin/stores/${encodeURIComponent(storeId)}/devices/${encodeURIComponent(deviceId)}`,
      etag,
      { tenant_id: tenantId, name: fields.name, kind: fields.kind, status: fields.status },
    ),

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
    etag: ETag,
  ) =>
    requestJsonIfMatch<TaxClass>(
      "PATCH",
      `/admin/catalog/tax-classes/${encodeURIComponent(taxClassId)}`,
      etag,
      { tenant_id: tenantId, name: fields.name, status: fields.status },
    ),
  // Tax rates (ADR-0074, Track M4): the per-(tax class × channel) rate the edge applies. `set`
  // replaces the tenant's whole table (behind console.catalog.manage); the read is behind
  // console.data.read.
  // The whole class × channel grid is one collection: the read carries its version as an `ETag` and
  // the save carries it back as `If-Match`, so two operators editing different cells cannot silently
  // lose one of the edits (ADR-0095). `etag: null` means this tenant has never saved rates.
  listTaxRates: (tenantId: string) =>
    requestJsonWithEtag<TaxRate[]>(`/admin/catalog/tax-rates?${tenantQuery(tenantId)}`),
  setTaxRates: (tenantId: string, rates: readonly TaxRate[], version: string | null) =>
    requestJsonIfMatchRaw<TaxRate[]>("PUT", "/admin/catalog/tax-rates", precondition(version), {
      tenant_id: tenantId,
      rates,
    }),
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
  // The update is conditional (ADR-0095): `etag` is the version the campaign was read at, and the
  // server applies the write only if it is still the stored one — so two operators editing the same
  // promotion cannot silently overwrite each other.
  updateCampaign: (tenantId: string, id: string, etag: ETag, input: CampaignInput) =>
    requestJsonIfMatch<Campaign>("PUT", `/admin/campaigns/${encodeURIComponent(id)}`, etag, {
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
  // A page of a campaign's codes. There is deliberately no unpaged helper here: the *route* still
  // serves the whole set without `?limit=` and always will (ADR-0098 — an operator printing a flyer
  // run needs every code), but no console screen asks for it, and a typed helper with no caller is
  // the shape of finding #273. Add it back the day a screen needs it.
  listVouchersPage: (tenantId: string, campaignId: string, page: PageRequest) =>
    requestJson<Page<Voucher>>(
      "GET",
      `/admin/campaigns/${encodeURIComponent(campaignId)}/vouchers?${tenantPageQuery(tenantId, page)}`,
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
  // Which other origins a store's edge answers (ADR-0111). Same read/publish shape as the settings
  // nodes above; an absent node (null) means same-origin only, which is how every store behaved
  // before this existed. The cloud refuses an entry the edge would refuse, so a `400` here is the
  // operator's typo and not a store that will quietly ignore what they saved.
  readOrigins: (tenantId: string, storeId: string) =>
    requestJson<{ allowed: string[] } | null>(
      "GET",
      `/admin/config/origins?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  publishOrigins: (tenantId: string, storeId: string, allowed: string[]) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/origins", {
      tenant_id: tenantId,
      store_id: storeId,
      allowed,
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
  // never send one; a recipe's key is the menu item it makes, so a create names that item in the body
  // and the update takes it from the path. Every update is conditional on the version it was read at
  // (ADR-0095), which is why each of these types carries an `etag`.
  listIngredients: (tenantId: string) =>
    requestJson<Ingredient[]>("GET", `/admin/inventory/ingredients?${tenantQuery(tenantId)}`),
  createIngredient: (tenantId: string, input: IngredientInput) =>
    requestJson<Ingredient>("POST", "/admin/inventory/ingredients", {
      tenant_id: tenantId,
      ...input,
    }),
  updateIngredient: (tenantId: string, id: string, etag: ETag, input: IngredientInput) =>
    requestJsonIfMatch<Ingredient>(
      "PUT",
      `/admin/inventory/ingredients/${encodeURIComponent(id)}`,
      etag,
      { tenant_id: tenantId, ...input },
    ),
  deleteIngredient: (tenantId: string, id: string) =>
    requestVoid(
      "DELETE",
      `/admin/inventory/ingredients/${encodeURIComponent(id)}?${tenantQuery(tenantId)}`,
    ),
  listRecipes: (tenantId: string) =>
    requestJson<Recipe[]>("GET", `/admin/inventory/recipes?${tenantQuery(tenantId)}`),
  // Add a recipe for an item that has none. Refused with `409` if it already has one — the `PUT`
  // this replaced was a create-or-replace, so "add a recipe" silently discarded the bill of
  // materials already there (ADR-0095).
  createRecipe: (tenantId: string, item: string, input: RecipeInput) =>
    requestJson<Recipe>("POST", "/admin/inventory/recipes", {
      tenant_id: tenantId,
      item_id: item,
      ...input,
    }),
  // Edit an existing recipe, at the version it was read at.
  updateRecipe: (tenantId: string, item: string, etag: ETag, input: RecipeInput) =>
    requestJsonIfMatch<Recipe>(
      "PUT",
      `/admin/inventory/recipes/${encodeURIComponent(item)}`,
      etag,
      { tenant_id: tenantId, ...input },
    ),
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
  updateSupplier: (tenantId: string, id: string, etag: ETag, input: SupplierInput) =>
    requestJsonIfMatch<Supplier>(
      "PUT",
      `/admin/inventory/suppliers/${encodeURIComponent(id)}`,
      etag,
      { tenant_id: tenantId, ...input },
    ),
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
  // console.config.publish; the edge applies the currency, timezone, and business-date cutoff — and,
  // since ADR-0105, the quoting posture, the cash-rounding increment and the till's quick-cash notes.
  publishLocale: (
    tenantId: string,
    storeId: string,
    settings: {
      currency_code: string;
      timezone: string;
      cutoff_hour: number;
      display_language?: string;
      prices_include_tax: boolean;
      cash_rounding_increment: number | null;
      cash_denominations: readonly number[];
    },
  ) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/locale", {
      tenant_id: tenantId,
      store_id: storeId,
      currency_code: settings.currency_code,
      timezone: settings.timezone,
      cutoff_hour: settings.cutoff_hour,
      display_language: settings.display_language ?? null,
      prices_include_tax: settings.prices_include_tax,
      cash_rounding_increment: settings.cash_rounding_increment,
      cash_denominations: settings.cash_denominations,
    }),

  // Publish a store's registered identity (ADR-0106) as its `store_profile` config node, behind
  // console.config.publish. The edge composes every receipt from it, which is what turns the store's
  // paper into a document a Japanese or Indian auditor accepts.
  publishStoreProfile: (
    tenantId: string,
    storeId: string,
    profile: {
      legal_name: string;
      trading_name?: string;
      address_lines: readonly string[];
      tax_registration_number?: string;
      tax_registration_label?: string;
      contact_lines: readonly string[];
      footer_lines: readonly string[];
      country_code?: string;
    },
  ) =>
    requestJson<PublishedConfig>("PUT", "/admin/config/store-profile", {
      tenant_id: tenantId,
      store_id: storeId,
      ...profile,
      trading_name: profile.trading_name ?? null,
      tax_registration_number: profile.tax_registration_number ?? null,
      tax_registration_label: profile.tax_registration_label ?? null,
      country_code: profile.country_code ?? null,
    }),

  // --- media (ADR-0075) ---
  // Upload an image; the server re-encodes it to two bounded renditions and returns the new id.
  uploadMedia: (tenantId: string, file: Blob) =>
    requestUpload<UploadedMedia>(`/admin/media?${tenantQuery(tenantId)}`, file),
  // The whole library, unpaged — what the item image picker needs: you cannot find the photograph
  // you want in the first twenty-four of eight hundred (ADR-0098). Permanent, not a legacy shape.
  listMedia: (tenantId: string) =>
    requestJson<MediaSummary[]>("GET", `/admin/media?${tenantQuery(tenantId)}`),
  // One page of the library, for the Media screen's grid. Paged because the grid mounts an `<img>`
  // per asset and each fetches a rendition, so an unpaged grid of a large library is hundreds of
  // requests on open — a bigger cost than the JSON it arrives in.
  listMediaPage: (tenantId: string, page: PageRequest) =>
    requestJson<Page<MediaSummary>>("GET", `/admin/media?${tenantPageQuery(tenantId, page)}`),
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
    etag: ETag,
  ) =>
    requestJsonIfMatch<ItemCategory>(
      "PATCH",
      `/admin/catalog/item-categories/${encodeURIComponent(itemCategoryId)}`,
      etag,
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
    etag: ETag,
  ) =>
    requestJsonIfMatch<ItemSubcategory>(
      "PATCH",
      `/admin/catalog/item-subcategories/${encodeURIComponent(itemSubcategoryId)}`,
      etag,
      {
        tenant_id: tenantId,
        item_category_id: fields.itemCategoryId,
        name: fields.name,
        status: fields.status,
      },
    ),
  // One page of the item master, searched and ordered by the server (ADR-0098 B3-3). The table is
  // the only consumer that wants a window; `listItems` below stays for the pickers and the compiler.
  listItemsPage: (tenantId: string, page: PageRequest, filter: ItemListFilter = {}) => {
    const params = new URLSearchParams(tenantPageQuery(tenantId, page));
    if (filter.q) params.set("q", filter.q);
    if (filter.sort) params.set("sort", filter.sort);
    if (filter.order) params.set("order", filter.order);
    return requestJson<Page<CatalogItem>>(
      "GET",
      `/admin/catalog/items?${params.toString()}`,
    );
  },
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
    etag: ETag,
  ) =>
    requestJsonIfMatch<CatalogItem>("PATCH", `/admin/catalog/items/${encodeURIComponent(menuItemId)}`, etag, {
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
    etag: ETag,
  ) =>
    requestJsonIfMatch<Menu>("PATCH", `/admin/catalog/menus/${encodeURIComponent(menuId)}`, etag, {
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
  // Put an item on a menu. A placement's identity is the caller-supplied `(menu, item)` pair, so a
  // second create at the same pair is refused with `409` rather than repricing the one already there
  // — and because the per-channel prices are the price-change journal (ADR-0069), that overwrite was
  // recorded as a set with no `before` to compare against (ADR-0095).
  createPlacement: (
    tenantId: string,
    menuId: string,
    menuItemId: string,
    prices: ChannelPrice[],
    available: boolean,
    menuSectionId: string | null,
  ) =>
    requestVoid("POST", `/admin/catalog/menus/${encodeURIComponent(menuId)}/placements`, {
      tenant_id: tenantId,
      menu_id: menuId,
      menu_item_id: menuItemId,
      menu_section_id: menuSectionId,
      prices,
      available,
    }),
  // Reprice, re-section or 86 an item already on the menu, at the version it was read at.
  updatePlacement: (
    tenantId: string,
    menuId: string,
    menuItemId: string,
    etag: ETag,
    prices: ChannelPrice[],
    available: boolean,
    menuSectionId: string | null,
  ) =>
    requestVoidIfMatch(
      "PUT",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/placements/${encodeURIComponent(menuItemId)}`,
      etag,
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
    etag: ETag,
  ) =>
    requestJsonIfMatch<MenuSection>(
      "PATCH",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/sections/${encodeURIComponent(menuSectionId)}`,
      etag,
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
    etag: ETag,
  ) =>
    requestJsonIfMatch<ModifierGroup>(
      "PATCH",
      `/admin/catalog/modifier-groups/${encodeURIComponent(modifierGroupId)}`,
      etag,
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
    etag: ETag,
  ) =>
    requestJsonIfMatch<DisplayCategory>(
      "PATCH",
      `/admin/catalog/display-categories/${encodeURIComponent(displayCategoryId)}`,
      etag,
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
    etag: ETag,
  ) =>
    requestJsonIfMatch<DisplaySubcategory>(
      "PATCH",
      `/admin/catalog/display-subcategories/${encodeURIComponent(displaySubcategoryId)}`,
      etag,
      {
        tenant_id: tenantId,
        display_category_id: fields.displayCategoryId,
        name: fields.name,
        status: fields.status,
      },
    ),
  listLayoutButtons: (tenantId: string) =>
    requestJson<LayoutButton[]>("GET", `/admin/catalog/layout-buttons?${tenantQuery(tenantId)}`),
  // Place a button. A button's identity is its `(channel, item)` slot and comes from the caller, so a
  // second create at the same slot is refused with `409` rather than relabelling and re-positioning
  // the button already there (ADR-0095).
  createLayoutButton: (
    tenantId: string,
    salesChannel: SalesChannel,
    menuItemId: string,
    fields: LayoutButtonFields,
  ) =>
    requestJson<LayoutButton>("POST", "/admin/catalog/layout-buttons", {
      tenant_id: tenantId,
      sales_channel: salesChannel,
      menu_item_id: menuItemId,
      ...layoutButtonBody(fields),
    }),
  // Relabel or move a button already on that channel, at the version it was read at.
  updateLayoutButton: (
    tenantId: string,
    salesChannel: SalesChannel,
    menuItemId: string,
    etag: ETag,
    fields: LayoutButtonFields,
  ) =>
    requestJsonIfMatch<LayoutButton>(
      "PUT",
      `/admin/catalog/layout-buttons/${encodeURIComponent(salesChannel)}/${encodeURIComponent(menuItemId)}`,
      etag,
      { tenant_id: tenantId, ...layoutButtonBody(fields) },
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

  // --- the store's lease (ADR-0108) ---
  // Reading the authoritative generation is behind console.data.read. Bumping it is behind
  // console.stores.manage and audited: it is the act of saying a different machine is the store now,
  // which supersedes whatever box holds the previous generation and stops it taking updates. There is
  // deliberately no setter — the only write advances the counter by one.
  getStoreLease: (tenantId: string, storeId: string) =>
    requestJson<StoreLease>(
      "GET",
      `/admin/config/lease?${tenantQuery(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
    ),
  /** Issues a store's next lease generation (ADR-0108), optionally *moving* it (ADR-0110).
   *
   *  `heldGeneration` is the authoritative generation the caller read, and it is **required** — the
   *  route is a conditional write (ADR-0094). Pass `null` for a store that has never been issued a
   *  lease, which sends `If-Match: *`. Two admins bumping one store at once would otherwise both be
   *  told they succeeded while only one placement stuck.
   *
   *  `edgePlacement` names where the new machine runs and is a **move**. Omitting it is ADR-0003's
   *  swap of the machine in place and keeps whatever the store had — so a caller with nothing to say
   *  must send nothing, never `EDGE_PLACEMENT_UNSPECIFIED`, which the server refuses precisely
   *  because it would make a request that looks like a move quietly not be one.
   *
   *  `acknowledgeUndrained` names a generation whose machine still holds events this cloud has never
   *  seen. Without it the route refuses such a bump (`422`), because moving a store off that machine
   *  abandons a night's trading; with it, the abandonment is a recorded decision with a name on it. */
  bumpStoreLease: (
    tenantId: string,
    storeId: string,
    heldGeneration: number | null,
    edgePlacement?: string,
    acknowledgeUndrained?: number,
  ) =>
    requestJsonIfMatchRaw<PublishedConfig>(
      "POST",
      "/admin/config/lease/bump",
      heldGeneration === null ? "*" : `"${heldGeneration}"`,
      {
        tenant_id: tenantId,
        store_id: storeId,
        ...(edgePlacement ? { edge_placement: edgePlacement } : {}),
        ...(acknowledgeUndrained === undefined
          ? {}
          : { acknowledge_undrained: acknowledgeUndrained }),
      },
    ),

  /**
   * Attests that a superseded machine holds no events, closing a handover that will never close
   * itself — a box already powered off, or one whose disk an operator has read (ADR-0110).
   *
   * `supersededGeneration` is the generation whose machine was checked, and it is the whole
   * precondition, which is why there is no `If-Match` here. Any bump necessarily changes the value
   * this write tests, so a concurrent bump makes the server refuse on its own. Naming a generation
   * the row does not hold is a 422, not a quiet success: this records a person asserting a fact
   * about one specific machine.
   */
  settleHandover: (
    tenantId: string,
    storeId: string,
    supersededGeneration: number,
  ) =>
    requestVoid("POST", "/admin/config/lease/settle", {
      tenant_id: tenantId,
      store_id: storeId,
      superseded_generation: supersededGeneration,
    }),

  /**
   * Records that a settled handover's outgoing machine, its database and its hosting are no longer
   * needed (ADR-0110).
   *
   * No generation, and the asymmetry with `settleHandover` is deliberate: settle is an attestation
   * about one named machine's disk, retire is a decision about the handover the row currently
   * describes. The server refuses (422) while a handover is still in flight, and refuses a second
   * retirement rather than overwriting the first decision's who and when.
   */
  retireHandover: (tenantId: string, storeId: string) =>
    requestVoid("POST", "/admin/config/lease/retire", {
      tenant_id: tenantId,
      store_id: storeId,
    }),

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
  // The windowed read ADR-0069 shipped: the newest `limit` matching entries as a bare array, with
  // no count. What the per-entity audit panel wants — the last few changes to one thing.
  listAudit: (filter: AuditFilter = {}) => {
    const query = auditFilterParams(filter).toString();
    return requestJson<AuditEntry[]>("GET", query ? `/admin/audit?${query}` : "/admin/audit");
  },
  // One page of the same filtered set, plus how many matched. On this route the *offset* is what
  // asks for a page: `limit` already meant "the newest this many" before paging existed, and making
  // it change the response shape would break a request already in flight (ADR-0098).
  //
  // `order` is sent only when it is not the default, so a caller that does not sort produces the
  // same request it produced before the order existed.
  listAuditPage: (filter: AuditFilter, page: PageRequest, order?: TrailOrder) => {
    const params = auditFilterParams({ ...filter, limit: page.limit });
    params.set("offset", String(page.offset ?? 0));
    if (order && order !== "newest") {
      params.set("order", order);
    }
    return requestJson<Page<AuditEntry>>("GET", `/admin/audit?${params.toString()}`);
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
