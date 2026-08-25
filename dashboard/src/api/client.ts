// The typed HTTP client for pos_cloud's `/admin` surface (ADR-0060). Every call is one `fetch` to
// the same origin that served the dashboard, so the super-admin session cookie the server set on
// login rides along automatically and no base URL or token handling is needed. A refused request
// comes back as a non-2xx with a plain-text (or, for a rejected config publish, a JSON) reason;
// this surfaces it as an `ApiError` the screens show without guessing.

import type {
  ActivationCode,
  ApiKeySummary,
  Brand,
  CatalogItem,
  ChannelPrice,
  ConfigLevel,
  CreateApiKeyResponse,
  DailyRollup,
  Device,
  DeviceProposalSummary,
  EntityStatus,
  Enrolment,
  ItemCategory,
  ItemSubcategory,
  Json,
  Menu,
  MenuPlacement,
  PublishedConfig,
  RegisterWebhookResponse,
  Store,
  TaxClass,
  Tenant,
  TranslationGrid,
  WebhookSummary,
} from "./types";

export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }

  /** No session, or it expired — the shell sends the operator back to the login screen. */
  get isUnauthorized(): boolean {
    return this.status === 401;
  }

  /** A validation refusal (bad config, a URL the SSRF guard rejects) — the operator's to fix. */
  get isRejected(): boolean {
    return this.status === 400 || this.status === 409 || this.status === 422;
  }
}

async function failure(response: Response): Promise<ApiError> {
  const text = await response.text().catch(() => "");
  const trimmed = text.trim();
  // A rejected config publish answers `422 {"violations": [...]}`; join them into one message.
  if (trimmed.startsWith("{")) {
    try {
      const body = JSON.parse(trimmed) as { violations?: string[] };
      if (Array.isArray(body.violations) && body.violations.length > 0) {
        return new ApiError(response.status, body.violations.join("; "));
      }
    } catch {
      // Not the violations shape; fall through to the raw text.
    }
  }
  return new ApiError(response.status, trimmed || response.statusText);
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

const tenantQuery = (tenantId: string) => `tenant_id=${encodeURIComponent(tenantId)}`;

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

  // --- rollups (ADR-0060 admin read) ---
  dailyRollups: (tenantId: string, storeId: string) =>
    requestJson<DailyRollup[]>(
      "GET",
      `/admin/stores/${encodeURIComponent(storeId)}/rollups/daily?${tenantQuery(tenantId)}`,
    ),

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
  updateStore: (
    storeId: string,
    tenantId: string,
    fields: { name: string; status: EntityStatus; brandId: string | null },
  ) =>
    requestJson<Store>("PATCH", `/admin/stores/${encodeURIComponent(storeId)}`, {
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
    taxonomy: { itemCategoryId: string | null; itemSubcategoryId: string | null },
  ) =>
    requestJson<CatalogItem>("POST", "/admin/catalog/items", {
      tenant_id: tenantId,
      name,
      tax_class_id: taxClassId,
      item_category_id: taxonomy.itemCategoryId,
      item_subcategory_id: taxonomy.itemSubcategoryId,
    }),
  updateItem: (
    menuItemId: string,
    tenantId: string,
    fields: {
      name: string;
      taxClassId: string;
      itemCategoryId: string | null;
      itemSubcategoryId: string | null;
      status: EntityStatus;
    },
  ) =>
    requestJson<CatalogItem>("PATCH", `/admin/catalog/items/${encodeURIComponent(menuItemId)}`, {
      tenant_id: tenantId,
      name: fields.name,
      tax_class_id: fields.taxClassId,
      item_category_id: fields.itemCategoryId,
      item_subcategory_id: fields.itemSubcategoryId,
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
  ) =>
    requestVoid(
      "PUT",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/placements/${encodeURIComponent(menuItemId)}`,
      { tenant_id: tenantId, prices, available },
    ),
  deletePlacement: (tenantId: string, menuId: string, menuItemId: string) =>
    requestVoid(
      "DELETE",
      `/admin/catalog/menus/${encodeURIComponent(menuId)}/placements/${encodeURIComponent(menuItemId)}?${tenantQuery(tenantId)}`,
    ),
  publishMenu: (tenantId: string, storeId: string, menuId: string) =>
    requestJson<PublishedConfig>("POST", "/admin/catalog/publish", {
      tenant_id: tenantId,
      store_id: storeId,
      menu_id: menuId,
    }),
};
