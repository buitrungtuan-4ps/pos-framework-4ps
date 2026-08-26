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
  DisplayCategory,
  DisplaySubcategory,
  EntityStatus,
  Enrolment,
  ItemCategory,
  ItemSubcategory,
  Json,
  LayoutButton,
  Menu,
  MenuPlacement,
  MenuSection,
  ModifierGroup,
  SalesChannel,
  PublishedConfig,
  RegisterWebhookResponse,
  Store,
  TaxClass,
  Tenant,
  TranslationGrid,
  WebhookSummary,
} from "./types";
import { setAuthed } from "../state/session";

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
  // A 401 means the session is gone or expired. Drop the client's authed flag so the shell's route
  // guard sends the operator back to the login screen, rather than stranding them on a view that can
  // no longer load anything (F0).
  if (response.status === 401) {
    setAuthed(false);
  }
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
};
