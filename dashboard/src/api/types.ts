// The wire shapes the dashboard exchanges with pos_cloud's `/admin` surface. Field names match the
// server's serde structs exactly (ADR-0060); where a payload is an open document — a composed
// config, a translation grid — it is typed as JSON rather than pinned, because the server treats it
// that way too.

/** An arbitrary JSON document — a composed config, or one level of the config tree. */
export type Json =
  | null
  | boolean
  | number
  | string
  | Json[]
  | { [key: string]: Json };

/** The four levels of the config tree (ADR-0033), in inheritance order. */
export type ConfigLevel = "tenant" | "brand" | "store" | "device";
export const CONFIG_LEVELS: readonly ConfigLevel[] = ["tenant", "brand", "store", "device"];

/** One published config version from `GET /admin/stores/{id}/config/versions` (ADR-0069 G2). `at_ms`
 *  is read from the version ULID itself; `current` marks the version the store is on now. */
export interface ConfigVersion {
  readonly version_id: string;
  readonly at_ms: number;
  readonly current: boolean;
}

/** The one-time TOTP enrolment returned by `POST /admin/setup` (ADR-0034). */
export interface Enrolment {
  readonly otpauth_uri: string;
  readonly secret_base32: string;
}

/** A summary row from `GET /admin/api-keys` (the secret is never listed). */
export interface ApiKeySummary {
  readonly id: string;
  /** The one store this key acts for, or `null` for a tenant-wide key (S1). */
  readonly store_id: string | null;
  readonly scopes: string[];
  readonly revoked: boolean;
  /**
   * When the key stops working, in Unix milliseconds, or `null` if it never does. Served since the
   * key store was written and dropped by this type, so an expired key rendered as "Active"
   * (production-readiness **O4**).
   */
  readonly expires_at_ms: number | null;
}

/** The one-time response to `POST /admin/api-keys` — the token is shown once. */
export interface CreateApiKeyResponse {
  readonly id: string;
  readonly token: string;
}

/** A pending printer/KDS proposal from `GET /admin/devices/proposals` (ADR-0041). */
export interface DeviceProposalSummary {
  readonly id: string;
  readonly store_id: string;
  readonly kind: string;
  /**
   * The name the box read off the device, and where it is on the LAN. Both are what identify the
   * thing an operator is about to approve — the cloud has always served them, and this type dropped
   * them, so the approval screen asked people to approve a kind and a ULID (production-readiness
   * **O3**).
   */
  readonly name: string;
  readonly address: string;
  /** How it is attached, once approval recorded it; `null` while pending (ADR-0100). */
  readonly connection: string | null;
  /** The kitchen station it serves, once approval recorded it; `null` for a receipt printer. */
  readonly station_id: string | null;
  /**
   * The `terminal` whose agent writes this printer's bytes, if an operator has picked one
   * (ADR-0112). `null` — the ordinary case — means the edge opens the address itself.
   */
  readonly agent_device_id: string | null;
  /** `pending`, `approved` or `rejected`. */
  readonly status: string;
  /**
   * The version this row was read at, for a conditional write (ADR-0094). Opaque: the server mints
   * it and nothing here may reason about its shape — it is only ever echoed back in `If-Match`.
   */
  readonly version: string;
}

/** The id a freshly created terminal was given (ADR-0112). */
export interface CreateTerminalResponse {
  /** The terminal's device id (a ULID) — what a printer names to make it its agent. */
  readonly id: string;
  /** `approved`, always: a terminal is created resolved, because the console write *is* the decision. */
  readonly status: string;
}

/** A webhook endpoint row from `GET /admin/webhooks` (ADR-0032). */
export interface WebhookSummary {
  readonly id: string;
  readonly store_id: string;
  readonly url: string;
  readonly cursor: string | null;
  readonly disabled: boolean;
}

/** The one-time response to `POST /admin/webhooks` — the signing secret is shown once. */
export interface RegisterWebhookResponse {
  readonly id: string;
  readonly url: string;
  readonly signing_secret: string;
}

/** The version id a successful config publish produced. */
export interface PublishedConfig {
  readonly config_version_id: string;
}

/** One day's rollup for a store (ADR-0022) — counts only, no PII. */
export interface DailyRollup {
  readonly business_date: string;
  readonly total_events: number;
  readonly by_type: Record<string, number>;
}

/** One menu item's gross ordered contribution on a trading day (part of `DailyRevenue`, ADR-0081). */
export interface ItemMix {
  readonly name: string;
  /** Ordered quantity in thousandths of a unit. */
  readonly ordered_qty_milli: number;
  /** Ordered line total, minor units — gross, before voids/comps. */
  readonly ordered_value: number;
}

/**
 * One day's revenue rollup for a store (ADR-0081, Track O4). Amounts are the store's single
 * currency's minor units. Revenue is **T2** — served only behind `console.reports.revenue`.
 */
export interface DailyRevenue {
  readonly business_date: string;
  readonly currency_code: string;
  readonly bills: number;
  readonly gross: number;
  readonly reductions: number;
  readonly service_charge: number;
  readonly tax: number;
  /** `total_due` summed — the headline revenue figure. */
  readonly net: number;
  readonly by_item: Record<string, ItemMix>;
}

/** One day's cash-drawer summary for a store (ADR-0081). Amounts are minor units. T2. */
export interface DailyCash {
  readonly business_date: string;
  readonly currency_code: string;
  readonly opening_float: number;
  readonly paid_in: number;
  readonly paid_out: number;
  readonly shifts_opened: number;
  readonly shifts_closed: number;
  readonly expected: number;
  readonly counted: number;
  readonly variance: number;
}

/**
 * An X or Z report for a store's trading day (ADR-0081, spec gap D10). `kind` is `"X"` (current,
 * non-resetting) or `"Z"` (a closed day, immutable). Bundles activity, revenue, and cash; T2.
 */
export interface XzReport {
  readonly kind: "X" | "Z";
  readonly business_date: string;
  readonly activity: DailyRollup;
  readonly revenue: DailyRevenue;
  readonly cash: DailyCash;
}

/** The one-time activation code returned by `POST /admin/activation-codes` (ADR-0050). */
export interface ActivationCode {
  readonly activation_code: string;
}

/**
 * A translation grid: `key → { locale → message }`. `en` must be present and non-empty for every
 * key (the server rejects a grid that breaks the fallback rule, ADR-0020/0043).
 */
export type TranslationGrid = Record<string, Record<string, string>>;

/** One row's fate in a CSV import dry-run (ADR-0075): create a new key, update an existing one, or
 *  reject it with a reason (a missing `en`, an empty key). */
export interface TranslationImportRow {
  readonly key: string;
  readonly action: "create" | "update" | "reject";
  readonly reason?: string;
}

/** The dry-run (and post-apply) report for a translation-grid CSV import (ADR-0075). */
export interface TranslationImportReport {
  readonly rows: readonly TranslationImportRow[];
  readonly create_count: number;
  readonly update_count: number;
  readonly reject_count: number;
}

/** Whether a registry entity is in use or retired (ADR-0065). Entities are archived, never deleted. */
export type EntityStatus = "active" | "archived";

/**
 * The version a record was read at (ADR-0094). Send it back in `If-Match` to write; a write against
 * a version the record no longer holds is refused with `412` instead of overwriting whoever edited
 * in between.
 *
 * Opaque: never parse it, never compare two for ordering, never build one. The server mints it and
 * the only correct thing to do with it is hand it back unchanged.
 */
export type ETag = string;

/** A tenant from `GET /admin/tenants` (ADR-0065) — the root of the org tree. */
export interface Tenant {
  readonly tenant_id: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A brand from `GET /admin/brands` — grouped under a tenant. */
export interface Brand {
  readonly brand_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A store from `GET /admin/stores` — grouped under a tenant and, optionally, a brand. */
export interface Store {
  readonly store_id: string;
  readonly tenant_id: string;
  readonly brand_id: string | null;
  readonly name: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A device from `GET /admin/stores/{id}/devices` — the canonical device identity. */
export interface Device {
  readonly device_id: string;
  readonly tenant_id: string;
  readonly store_id: string;
  readonly name: string;
  readonly kind: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/**
 * A sales channel wire token (ADR-0066, `pos-proto` `SalesChannel`). The value is the full
 * `wire_enum!` token — `SALES_CHANNEL_DINE_IN`, not `DINE_IN` — because the server round-trips it
 * through `Open<SalesChannel>`, which serialises the whole prefixed token.
 */
export type SalesChannel =
  | "SALES_CHANNEL_DINE_IN"
  | "SALES_CHANNEL_TAKEAWAY"
  | "SALES_CHANNEL_DELIVERY"
  | "SALES_CHANNEL_QR"
  | "SALES_CHANNEL_API";

/** The channels an operator can price, in the order they appear on the placement editor. */
export const SALES_CHANNELS: readonly SalesChannel[] = [
  "SALES_CHANNEL_DINE_IN",
  "SALES_CHANNEL_TAKEAWAY",
  "SALES_CHANNEL_DELIVERY",
  "SALES_CHANNEL_QR",
  "SALES_CHANNEL_API",
];

/** Integer money (`pos-proto` `Money`): a currency and an amount in that currency's smallest unit. */
export interface Money {
  readonly currency_code: string;
  readonly amount_minor: number;
}

/** A tax class — a named bucket an item belongs to (ADR-0066 entity 10). Its id is the item's `tax_class_id`. */
export interface TaxClass {
  readonly tax_class_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/**
 * One authored tax rate from `GET /admin/catalog/tax-rates` (ADR-0074, Track M4): the rate a tax class
 * resolves to on a sales channel, in **basis points** (10% is `1000`, the reduced 8% is `800`).
 * `sales_channel` is the full wire token. `PUT /admin/catalog/tax-rates` replaces a tenant's whole
 * table with a list of these; the edge reprices against it.
 */
export interface TaxRate {
  readonly tax_class_id: string;
  readonly sales_channel: SalesChannel;
  readonly rate_bps: number;
  /**
   * How the rate is broken out on the invoice, when a country requires it
   * ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)). The parts must sum to
   * `rate_bps`, and the server refuses a save where they do not.
   *
   * Empty is the ordinary case and means the invoice prints one line. India's 5 % restaurant GST is
   * `[CGST 2.5 %, SGST 2.5 %]`, because the halves go to different governments.
   */
  readonly components: readonly TaxComponent[];
}

/** One named part of a tax rate, in basis points. */
export interface TaxComponent {
  readonly name: string;
  readonly rate_bps: number;
}

// --- Campaigns & scheduling (ADR-0077, Track M3) --------------------------------------------------

/** The five campaign kinds, in the §7 evaluation order. Wire mirror of `PublishedCampaignKind`. */
export type CampaignKind = "item_level" | "combo" | "bill_level" | "voucher" | "manual";

/** The kinds in authoring order, for the form's select. */
export const CAMPAIGN_KINDS: readonly CampaignKind[] = [
  "item_level",
  "combo",
  "bill_level",
  "voucher",
  "manual",
];

/** An exact rational rate (`pos-proto` `Ratio`): 10% is `{ numerator: 10, denominator: 100 }`. */
export interface Ratio {
  readonly numerator: number;
  readonly denominator: number;
}

/** What a campaign takes off — a percentage of, or a fixed amount off, the base. Serde-tagged on `type`. */
export type CampaignAction =
  | { readonly type: "percentage"; readonly rate: Ratio }
  | { readonly type: "amount_off"; readonly amount: Money };

/**
 * A weekly window (`PublishedSchedule`): a 7-bit weekday mask (Monday = bit 0) and a half-open
 * minute-of-day range that may wrap past midnight (`start_minute > end_minute`).
 */
export interface CampaignSchedule {
  readonly days: number;
  readonly start_minute: number;
  readonly end_minute: number;
}

/** The conditions a campaign requires before it applies; every field absent means unrestricted. */
export interface CampaignConditions {
  readonly min_bill?: Money;
  readonly channels?: readonly SalesChannel[];
  readonly schedule?: CampaignSchedule;
}

/** One authored campaign from `GET /admin/campaigns` (ADR-0077). The `id` is server-owned. */
export interface Campaign {
  readonly id: string;
  readonly name: string;
  readonly kind: CampaignKind;
  readonly priority: number;
  readonly exclusion_group?: number;
  readonly action: CampaignAction;
  readonly conditions: CampaignConditions;
  readonly quota_remaining?: number;
  readonly etag: ETag;
}

/** The authoring fields of a create/update — a `Campaign` without its server-owned id. */
export type CampaignInput = Omit<Campaign, "id" | "etag">;

/**
 * One page of a list, and the size of the set it came from (ADR-0098).
 *
 * `total` counts everything that matched, not what this page holds, so a pager can say "1–25 of
 * 812". `limit` and `offset` are echoed back so a pager can build the next request without
 * remembering what it sent — which matters the moment a page is reloaded or a link is shared.
 *
 * Only routes asked for a page answer this shape. A list read **without** `limit` still returns a
 * bare array of every row, permanently: that is the read a picker and a compiler make, and it is not
 * a legacy form to migrate off.
 */
export interface Page<T> {
  readonly items: readonly T[];
  readonly total: number;
  readonly limit: number;
  readonly offset: number;
}

/** What a caller asks of a page: how many rows, from where. */
export interface PageRequest {
  readonly limit: number;
  readonly offset?: number;
}

/** A voucher's lifecycle (`VoucherStatus`, snake_case on the wire). */
export type VoucherStatus = "active" | "redeemed" | "void";

/** One minted or listed voucher (`VoucherView`) — the id, the redeemable code, and its status. */
export interface Voucher {
  readonly voucher_id: string;
  readonly code: string;
  readonly status: VoucherStatus;
}

/** A scheduled publish's lifecycle (`ScheduledPublishStatus`, snake_case on the wire). */
export type ScheduledPublishStatus = "pending" | "applied" | "cancelled";

/** A pending/applied/cancelled scheduled publish (`ScheduledPublishView`) — metadata only, no payload. */
export interface ScheduledPublish {
  readonly id: string;
  readonly node_key: string;
  readonly effective_at_ms: number;
  readonly status: ScheduledPublishStatus;
  readonly created_at_ms: number;
}

/** The `201` response of a schedule request — the new row's id and when it fires. */
export interface ScheduledPublishCreated {
  readonly id: string;
  readonly effective_at_ms: number;
}

/** A quantity in thousandths of a unit (`Quantity`) — 1.5 kg is `{ milli: 1500 }`. */
export interface Quantity {
  readonly milli: number;
}

/** A unit of measure (`UnitOfMeasure`); wire tokens are prefixed `UNIT_OF_MEASURE_` (ADR-0079). */
export type UnitOfMeasure =
  | "UNIT_OF_MEASURE_GRAM"
  | "UNIT_OF_MEASURE_KILOGRAM"
  | "UNIT_OF_MEASURE_MILLILITER"
  | "UNIT_OF_MEASURE_LITER"
  | "UNIT_OF_MEASURE_PIECE";

/** The units of measure an ingredient can be stocked in, for the picker. */
export const UNITS: readonly UnitOfMeasure[] = [
  "UNIT_OF_MEASURE_GRAM",
  "UNIT_OF_MEASURE_KILOGRAM",
  "UNIT_OF_MEASURE_MILLILITER",
  "UNIT_OF_MEASURE_LITER",
  "UNIT_OF_MEASURE_PIECE",
];

/** One ingredient held in stock (`PublishedIngredient`) — id, display name, and the unit it is counted in. */
export interface Ingredient {
  readonly id: string;
  readonly name: string;
  readonly unit: UnitOfMeasure;
  readonly etag: ETag;
}

/**
 * The authoring fields of an ingredient create/update — an `Ingredient` without its server-owned id
 * or the version it was read at (a write sends that as `If-Match`, not in the body).
 */
export type IngredientInput = Omit<Ingredient, "id" | "etag">;

/** One bill-of-materials line (`PublishedRecipeLine`) — an ingredient and the amount one unit consumes. */
export interface RecipeLine {
  readonly ingredient: string;
  readonly per_unit: Quantity;
}

/**
 * The bill of materials for one makeable thing — a menu item or a modifier — plus its auto-86 threshold
 * (`PublishedRecipe`). The item is the recipe's key; an empty `lines` means it is never stock-limited.
 */
export interface Recipe {
  readonly item: string;
  readonly lines: RecipeLine[];
  readonly auto_86_threshold: number;
  readonly etag: ETag;
}

/** The authoring fields of a recipe write — the BOM and threshold; the item is the URL key. */
export type RecipeInput = Omit<Recipe, "item" | "etag">;

/** A supplier reference (`PublishedSupplier`) — id and name only; purchasing lives in the ERP (§19). */
export interface Supplier {
  readonly id: string;
  readonly name: string;
  readonly etag: ETag;
}

/** The authoring fields of a supplier create/update — a `Supplier` without its server-owned id. */
export type SupplierInput = Omit<Supplier, "id" | "etag">;

/** A QR ordering guardrail node (`qr`, ADR-0080) as read/published from the config tree. */
export interface QrGuardrails {
  readonly enabled: boolean;
  readonly staff_confirmation_required: boolean;
  readonly per_table_limit: number;
  readonly rate_window_secs: number;
  readonly business_hours?: {
    readonly open_hour: number;
    readonly close_hour: number;
    readonly tz_offset_minutes: number;
  } | null;
}

/** A store's availability to one delivery marketplace (`VendorAvailability`, wire-token prefixed). */
export type VendorAvailability =
  | "VENDOR_AVAILABILITY_OPEN"
  | "VENDOR_AVAILABILITY_BUSY"
  | "VENDOR_AVAILABILITY_CLOSED";

/** The availabilities offered in the vendor-policy editor. */
export const VENDOR_AVAILABILITIES: readonly VendorAvailability[] = [
  "VENDOR_AVAILABILITY_OPEN",
  "VENDOR_AVAILABILITY_BUSY",
  "VENDOR_AVAILABILITY_CLOSED",
];

/** One per-marketplace policy (`PublishedVendorPolicy`, ADR-0080). */
export interface VendorPolicy {
  readonly vendor: string;
  readonly enabled: boolean;
  readonly availability: VendorAvailability;
  readonly prep_minutes: number;
  readonly suppressed_items: string[];
}

/**
 * A publish dry-run from `POST /admin/config/campaigns/preview` (ADR-0077): the RFC 7386 merge patch
 * a publish would apply to the store's effective config, the version it diffs against (`null` if the
 * store has none yet), and whether nothing would change.
 */
export interface CampaignPreview {
  readonly from_version_id: string | null;
  readonly diff: Json;
  readonly unchanged: boolean;
}

/**
 * One compiled country module from `GET /admin/countries` (ADR-0074, Track M4) — read-only master
 * data: the code, human name, currency, preferred language, number format, and default retention
 * period. Feeds the currency picker and locale surfaces. `GET /admin/locales` returns the content
 * locales the platform can serve (BCP-47 tags), which the translation grid uses for its columns.
 */
export interface Country {
  readonly code: string;
  readonly display_name: string;
  readonly currency_code: string;
  readonly default_language: string;
  readonly decimal_separator: string;
  readonly group_separator: string;
  readonly digits_per_group: number;
  readonly default_retention_days: number;
  /** Whether this country's menu prices already contain their tax (ADR-0104): Japan and India do. */
  readonly prices_include_tax: boolean;
  /** What the total rounds to in cash, in minor units, or `null` for no rounding (ADR-0105). */
  readonly cash_rounding_increment: number | null;
  /** The notes a guest hands over, ascending, in minor units. Empty means the exact amount only. */
  readonly cash_denominations: readonly number[];
}

/** An item category — the operational taxonomy for reporting/kitchen grouping (ADR-0066 entity 2). */
export interface ItemCategory {
  readonly item_category_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** An item sub-category, nested under a category (ADR-0066 entity 3). */
export interface ItemSubcategory {
  readonly item_subcategory_id: string;
  readonly tenant_id: string;
  readonly item_category_id: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A display category — the presentation taxonomy a screen groups by (ADR-0066 entity 11). */
export interface DisplayCategory {
  readonly display_category_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A display sub-category, nested under a display category (ADR-0066 entity 11). */
export interface DisplaySubcategory {
  readonly display_subcategory_id: string;
  readonly tenant_id: string;
  readonly display_category_id: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A button's grid slot on a POS terminal (`pos-proto` `GridPosition`). */
export interface GridPosition {
  readonly column: number;
  readonly row: number;
}

/** An item's button in a per-channel layout (ADR-0066 entity 12). */
export interface LayoutButton {
  readonly tenant_id: string;
  readonly sales_channel: SalesChannel | null;
  readonly display_category_id: string;
  readonly display_subcategory_id: string | null;
  readonly menu_item_id: string;
  readonly label: string;
  readonly position: GridPosition | null;
  readonly sort: number;
  readonly etag: ETag;
}

/** A modifier group — a min/max selection rule with member modifiers, attached to items (ADR-0066 4/5). */
export interface ModifierGroup {
  readonly modifier_group_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly min_select: number;
  readonly max_select: number;
  readonly member_item_ids: string[];
  readonly attached_item_ids: string[];
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A catalog item — the product master (ADR-0066), the source of a compiled `MenuEntry`. */
export interface CatalogItem {
  readonly menu_item_id: string;
  readonly tenant_id: string;
  readonly name: string;
  /** Per-locale names keyed by locale code ("vi", "en", …); `name` is the fallback (ADR-0074). */
  readonly name_translations: Readonly<Record<string, string>>;
  readonly tax_class_id: string;
  readonly item_category_id: string | null;
  readonly item_subcategory_id: string | null;
  /** The item's photo — a media id (ADR-0075), or null. */
  readonly image_ref: string | null;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A media asset as listed (ADR-0075) — its id and size, never the bytes. */
export interface MediaSummary {
  readonly media_id: string;
  readonly content_type: string;
  readonly detail_bytes: number;
  readonly created_at_ms: number;
}

/** The response to a media upload (ADR-0075): the id to reference the new asset by. */
export interface UploadedMedia {
  readonly media_id: string;
  readonly detail_bytes: number;
}

/** A data subject as looked up (ADR-0076): existence and status, without the personal field values. */
export interface SubjectMeta {
  readonly subject_id: string;
  readonly collected_at_ms: number;
  /** Whether the personal data has already been masked (erased). */
  readonly masked: boolean;
  readonly field_count: number;
}

/** A data subject's exported record (ADR-0076) — the portability/access payload, with field values. */
export interface SubjectExport {
  readonly subject_id: string;
  readonly collected_at_ms: number;
  readonly masked: boolean;
  readonly fields: Record<string, string>;
}

/** A menu — a named set of placements that may inherit from a parent menu (ADR-0066). */
export interface Menu {
  readonly menu_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly parent_menu_id: string | null;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A menu section — an authoring grouping within a menu (ADR-0066 entity 7). Authoring-only. */
export interface MenuSection {
  readonly menu_section_id: string;
  readonly tenant_id: string;
  readonly menu_id: string;
  readonly name: string;
  readonly sort: number;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** One channel's price for a placement. `sales_channel` is the full wire token; `null` if unknown. */
export interface ChannelPrice {
  readonly sales_channel: SalesChannel | null;
  readonly unit_price: Money;
}

/** An item placed in a menu, with its per-channel prices and its published availability floor. */
export interface MenuPlacement {
  readonly tenant_id: string;
  readonly menu_id: string;
  readonly menu_item_id: string;
  readonly menu_section_id: string | null;
  readonly prices: ChannelPrice[];
  readonly available: boolean;
  readonly etag: ETag;
}

/** A console admin's least-privilege tier (ADR-0067). `owner` manages other admins; `viewer` is read-only. */
export type AdminRole = "owner" | "admin" | "ops" | "viewer";

/** The roles, in privilege order, for the console's role pickers. */
export const ADMIN_ROLES: readonly AdminRole[] = ["owner", "admin", "ops", "viewer"];

/** Whether a console admin can sign in (ADR-0067). A suspended admin keeps its history but cannot. */
export type AdminStatus = "active" | "suspended";

/**
 * A console admin as `GET /admin/whoami` and `GET /admin/admins` list it (ADR-0067): identity and
 * role only — never a password hash or a TOTP secret.
 */
export interface AdminIdentity {
  readonly id: string;
  readonly email: string;
  readonly name: string;
  readonly role: AdminRole;
  readonly status: AdminStatus;
}

/** A pending invitation from `GET /admin/invites` (ADR-0067) — carries no token, only what it grants. */
export interface AdminInvite {
  readonly id: string;
  readonly email: string;
  readonly name: string;
  readonly role: AdminRole;
  readonly invited_by: string;
  readonly accepted: boolean;
}

/** The one-time response to `POST /admin/invites` — the single-use token is shown exactly once. */
export interface InviteAdminResponse {
  readonly invite_id: string;
  readonly token: string;
  readonly expires_at_ms: number;
}

/**
 * One of the acting admin's live sessions from `GET /admin/sessions` (ADR-0067). `id` is the opaque
 * revocation handle (a hash, never the token); `current` marks the session making the request.
 */
export interface AdminSessionView {
  readonly id: string;
  readonly ip: string | null;
  readonly user_agent: string | null;
  readonly created_at_ms: number;
  readonly expires_at_ms: number;
  readonly current: boolean;
}

/** The freshly-generated recovery codes from `POST /admin/recovery-codes` — returned exactly once. */
export interface RecoveryCodesResponse {
  readonly codes: string[];
  readonly remaining: number;
}

/** How many unused recovery codes remain, from `GET /admin/recovery-codes` (never the codes). */
export interface RecoveryCodesStatus {
  readonly remaining: number;
}

/**
 * One store's fleet row from `GET /admin/fleet` (ADR-0068 slice 3): identity + status joined with
 * liveness, config drift, and relay backlog. `online` and `config_current` are the server's read-time
 * verdicts; the raw instants are Unix ms (or `null` for a store never seen / never configured).
 */
/**
 * One bound print agent on a store, as the fleet console sees it (ADR-0112).
 *
 * Two opaque identifiers and a duration — no document, no line, no name. Nothing a ticket *says*
 * helps diagnose a stalled agent, so nothing a ticket says crosses this wire.
 */
export interface FleetPrintAgent {
  /** The terminal a console admin created and a manager bound at the till. */
  readonly agent_device_id: string;
  /** The paired device answering for it — the box somebody has to walk to. */
  readonly paired_device_id: string;
  /**
   * How long the oldest still-unacknowledged job has waited, or absent when nothing is waiting.
   * Absent is the healthy answer and is deliberately not zero.
   */
  readonly oldest_unacknowledged_secs?: number;
}

export interface FleetStore {
  readonly store_id: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly online: boolean;
  readonly last_seen_at_ms: number | null;
  readonly last_config_pull_at_ms: number | null;
  readonly config_version_held: string | null;
  readonly config_version_published: string | null;
  readonly config_current: boolean;
  readonly relay_backlog: number;
  readonly relay_oldest_pending_at_ms: number | null;
  /** The binary version the store last reported running (ADR-0078), or `null` if it never reported. */
  readonly installed_version: string | null;
  /** Whether the store's last post-install self-test passed, or `null`. */
  readonly self_test_ok: boolean | null;
  /** Unix ms of the store's most recent OTA report, or `null`. */
  readonly reported_at_ms: number | null;
  /**
   * How many events the store had committed and not yet published as of its last heartbeat, or
   * `null` if it has never reported one. The mirror of `relay_backlog`: that counts orders held
   * *for* the store, this counts sales held *at* it. `null` is not zero — a store that never said
   * is not a store that is caught up — so the screen renders the two differently.
   */
  readonly outbox_depth: number | null;
  /** Unix ms of the heartbeat that reported `outbox_depth`, or `null`. */
  readonly outbox_reported_at_ms: number | null;
  /**
   * The print agents the store last reported (ADR-0112), or `null` if it never has.
   *
   * `null` and `[]` are different answers and the screen renders them differently: `null` is a store
   * that has said nothing about agents — every store whose edge runs in the shop — while `[]` is a
   * store that looked and has none.
   */
  readonly print_agents: readonly FleetPrintAgent[] | null;
  /** Unix ms of the heartbeat that reported them, or `null`. */
  readonly print_agents_reported_at_ms: number | null;
  /**
   * The lease generation the box last reported holding (ADR-0108), or `null` if it never said.
   * Read together with `lease_generation_authoritative`: the pair is what tells a **replaced** box
   * from a merely quiet one.
   */
  readonly lease_generation_held: number | null;
  /** Unix ms of the heartbeat that reported it, or `null`. */
  readonly lease_reported_at_ms: number | null;
  /** The store's authoritative lease generation, or `null` if the cloud never issued it one. */
  readonly lease_generation_authoritative: number | null;
  /**
   * Whether this box has been superseded — it holds a generation the store has moved past, so it
   * refuses over-the-air updates and is no longer the machine the store runs on. The server derives
   * it, so the console does not re-implement `lease_standing`.
   */
  readonly lease_superseded: boolean;
  /** Where the machine holding the authoritative generation runs (ADR-0110), or `null`.
   *
   *  `null` is not a mode. It means either that the cloud has never bumped this store, or that the
   *  stored token is one the server could not decode — and the server deliberately does not say
   *  which, because the difference matters for alert *severity*, which the server has already
   *  applied, and not for what a reader should display. Never `EDGE_PLACEMENT_UNSPECIFIED`: on the
   *  wire that token means "this message did not say". */
  readonly edge_placement: string | null;
  /**
   * The generation the last bump displaced and nothing has yet proved drained, or `null` (ADR-0110).
   *
   * The number a settle has to name: it says *which machine* the handover is about, where
   * `handover` says what is happening to it.
   */
  readonly lease_superseded_generation: number | null;
  /**
   * Which state this store's machine handover is in — `"taking-over"`, `"settled"`, `"retired"` —
   * or `null` when it has never had one (ADR-0110).
   *
   * `null` is a real answer and the commonest one: a store on generation 0 has never been replaced,
   * and a store with no lease row has never been issued one. Neither is a handover in any state, so
   * the console renders no badge rather than a reassuring one. The server derives this, including
   * the rule that an outbox depth and a lease generation only count together when one heartbeat
   * carried both — which is why the console must not recompute it from the fields beside it.
   */
  readonly handover: "taking-over" | "settled" | "retired" | null;
  /** Unix ms of the decision that this handover's old machine is no longer needed, or `null`. */
  readonly retired_at_ms: number | null;
  /** The deciding admin's id, or `null`. An id, not an email — the address lives in the audit trail. */
  readonly retired_by: string | null;
}

/**
 * A store's authoritative lease generation from `GET /admin/config/lease` (ADR-0108), or `null`
 * inside when the cloud has never issued this store a lease — which is "no lease in force", not
 * generation zero.
 */
export interface StoreLease {
  readonly generation: number | null;
}

/**
 * A store's currently-published OTA rollout — the `fleet_update` config node — from
 * `GET /admin/config/ota` (ADR-0078, Track O3), or `null` when nothing is published. `halted` is the
 * kill switch; `min_ring` names the slowest ring the rollout has reached.
 */
export interface OtaRollout {
  readonly target_version: string;
  readonly min_ring: string;
  readonly rollout_percent: number;
  readonly signing_key_id: string;
  readonly revoked_key_ids?: readonly string[];
  readonly halted?: boolean;
}

/** A `PUT /admin/config/ota` body: publish a rollout from typed fields (no `halted` — a fresh publish
 *  is live; the kill switch is a separate route). */
export interface PublishRolloutRequest {
  readonly tenant_id: string;
  readonly store_id: string;
  readonly target_version: string;
  readonly min_ring: string;
  readonly rollout_percent: number;
  readonly signing_key_id: string;
  readonly revoked_key_ids: readonly string[];
}

/**
 * A store's OTA placement — the `device_ota` config node — from `GET /admin/config/ota/placement`
 * (ADR-0052), or `null` when the store has never been placed. The rollout says which devices are
 * eligible; the placement says where this store sits. A store with no placement installs nothing.
 *
 * The placement is per store, not per terminal: a config tree is keyed by store and its Device layer
 * is one document its terminals share, so they all take the same ring and bucket (ADR-0052
 * Correction 1).
 */
export interface OtaPlacement {
  readonly ring: string;
  readonly canary_bucket: number;
}

/** A `PUT /admin/config/ota/placement` body: place a store in the rollout. */
export interface PublishPlacementRequest {
  readonly tenant_id: string;
  readonly store_id: string;
  readonly ring: string;
  readonly canary_bucket: number;
}

/**
 * One reconciliation run from `GET /admin/reconcile` (ADR-0078, Track O3): counts and a timestamp per
 * diff (ADR-0040), never event contents. `missing_found` of zero means the store was fully in sync.
 */
export interface ReconcileRun {
  readonly run_id: string;
  readonly store_id: string;
  readonly candidates_offered: number;
  readonly missing_found: number;
  readonly ran_at_ms: number;
}

/** One background loop's health from `GET /admin/health/tasks` (ADR-0068 slice 4). */
export interface TaskHealthEntry {
  readonly task: string;
  readonly expected: boolean;
  readonly healthy: boolean;
  readonly last_tick_at_ms: number | null;
  readonly seconds_since: number | null;
  readonly detail: Json;
}

/** The whole-fleet-of-loops report: an overall verdict plus per-loop entries. */
export interface TaskHealthReport {
  readonly healthy: boolean;
  readonly tasks: readonly TaskHealthEntry[];
}

/** One console audit entry from `GET /admin/audit` (ADR-0069, Track G2). Ids are strings (the screen
 *  shows the action and actor, and hides the ULID behind Technical details); the actor snapshot is
 *  flattened; `at_ms` is Unix milliseconds. */
export interface AuditEntry {
  readonly id: string;
  readonly tenant_id: string | null;
  readonly actor_admin_id: string;
  readonly actor_email: string;
  readonly actor_role: string;
  readonly action: string;
  readonly entity_type: string;
  readonly entity_id: string;
  readonly before: Json | null;
  readonly after: Json | null;
  readonly request_id: string | null;
  readonly at_ms: number;
}

/** The filters `GET /admin/audit` accepts. Every field is optional; an absent field does not filter.
 *  `tenantId` absent is the fleet-wide read (every tenant, including tenant-global entries). */
/**
 * How `GET /admin/catalog/items` orders a page. The server refuses anything else by name.
 *
 * `newest` is the order the unpaged read has always used and the default here too, so a screen that
 * does not sort keeps what it had.
 */
export type ItemSort = "newest" | "name" | "status";

/** What a page of the item master should contain and how it should be ordered (ADR-0098 B3-3). */
export interface ItemListFilter {
  /** Case-insensitive substring the name or any per-locale name must contain. */
  readonly q?: string;
  readonly sort?: ItemSort;
  readonly order?: "asc" | "desc";
}

/**
 * Which end of the trail a page of `GET /admin/audit` starts from. The server refuses anything else
 * by name.
 *
 * Named for the trail, not `asc`/`desc` like the item read: there the direction is relative to a
 * named `sort` field, and this route has none — so "ascending" would mean something different on
 * each route while spelling the same. `newest` is what the read has always returned and the default
 * here too, so a caller that does not ask keeps what it had.
 *
 * Only the *paged* read takes it. On the windowed read `limit` already means "the most recent this
 * many", so an order there would have two readings; the server refuses it rather than guessing.
 */
export type TrailOrder = "newest" | "oldest";

export interface AuditFilter {
  readonly tenantId?: string;
  readonly entityType?: string;
  readonly entityId?: string;
  readonly action?: string;
  readonly actorAdminId?: string;
  /**
   * The most recent this many entries — a *window*, not a page size (ADR-0069).
   *
   * The server defaults it to 200 and clamps it at 500. `listAuditPage` reuses the same field as
   * the page size, which is why the paged form on this route is asked for by naming an offset:
   * `?limit=` was already spoken for here before paging existed (ADR-0098).
   */
  readonly limit?: number;
}

// --- People & access (ADR-0070, Track M1) ---

/**
 * An employee as `GET /admin/employees` lists it: identity, code, status, and whether a sign-in PIN
 * is set — never the PIN or its hash. This is the console's first T1 Restricted data (ADR-0070).
 */
export interface Employee {
  readonly employee_id: string;
  readonly tenant_id: string;
  readonly code: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly has_pin: boolean;
  readonly etag: ETag;
}

/** A tenant's named role — a stored subset of the pos-core permission catalogue (§9, ADR-0070). */
export interface RoleTemplate {
  readonly role_template_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly permissions: readonly string[];
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** An employee's assignment to a store with a role (ADR-0070) — three ids, no PII. */
export interface Assignment {
  readonly assignment_id: string;
  readonly tenant_id: string;
  readonly employee_id: string;
  readonly store_id: string;
  readonly role_template_id: string;
  /**
   * The assigned person's name, resolved by the server as it reads (ADR-0098, B3-4).
   *
   * `null` when no employee row matches `employee_id` — nothing in the schema forbids an assignment
   * outliving the person's record, and a grant that still works is worth showing unlabelled rather
   * than hiding. Render `employee_id` in that case.
   */
  readonly employee_name: string | null;
  /** The assigned person's staff code, `null` on the same terms as the name. */
  readonly employee_code: string | null;
}

/** One entry of the pos-core permission catalogue the role editor offers (ADR-0070, §9). */
export interface PermissionInfo {
  readonly id: string;
  readonly group: string;
  readonly risk: string;
  readonly pin_required: boolean;
  readonly description: string;
}

/** The `201 { id }` body a people create returns (employee / role / assignment). */
export interface CreatedId {
  readonly id: string;
}

// --- Floor & kitchen (ADR-0072, Track M2): per-store areas/tables and kitchen stations/routing ---

/** A floor area from `GET /admin/floor/areas` — a named region of one store's floor. */
export interface Area {
  readonly area_id: string;
  readonly tenant_id: string;
  readonly store_id: string;
  readonly name: string;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A floor table — belongs to an area, optionally placed on the visual editor's grid (`position` is
 *  omitted by the server when the table is unplaced, so it arrives as `null` here). */
export interface FloorTable {
  readonly table_id: string;
  readonly tenant_id: string;
  readonly store_id: string;
  readonly area_id: string;
  readonly label: string;
  readonly seats: number;
  readonly position: GridPosition | null;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** A kitchen station from `GET /admin/kitchen/stations` — with an optional backup (printer failover)
 *  and a catch-all `is_default` flag. */
export interface Station {
  readonly station_id: string;
  readonly tenant_id: string;
  readonly store_id: string;
  readonly name: string;
  readonly backup_station_id: string | null;
  readonly is_default: boolean;
  readonly status: EntityStatus;
  readonly etag: ETag;
}

/** An item→station routing rule (ADR-0072) — matches a fired line by item or by course (exactly one).
 *  `sort` orders rules within their tier. */
export interface RoutingRule {
  readonly rule_id: string;
  readonly tenant_id: string;
  readonly store_id: string;
  readonly station_id: string;
  readonly menu_item_id: string | null;
  readonly course_id: string | null;
  readonly sort: number;
}

/** One table's printable QR from `GET /admin/floor/qr` (ADR-0072/ADR-0057): the label and the signed
 *  token the guest's QR carries. The token binds tenant/store/table — no PII — and is the public value
 *  printed on the code. */
export interface TableQrToken {
  readonly table_id: string;
  readonly label: string;
  readonly token: string;
}

/** A store's table QR tokens, for the console's printable sheet. */
export interface TableQrSheet {
  readonly store_id: string;
  readonly tokens: readonly TableQrToken[];
}

// --- Capabilities (ADR-0071, Track M8): the §10 flag catalogue the Config screen's form editor reads ---

/**
 * One capability flag from `GET /admin/capabilities` (§10): its config key, default, and one-line
 * description. The console renders a labelled toggle per flag from this — never a hand-kept list, so
 * the framework's own catalogue stays the single source of truth (ADR-0071).
 */
export interface CapabilityFlag {
  readonly key: string;
  readonly default_on: boolean;
  readonly description: string;
}

/** One capability preset (§10) — a named starting profile, given as the flag keys it turns on. */
export interface CapabilityPreset {
  readonly id: string;
  readonly keys: readonly string[];
}

/**
 * One inter-flag rule (§10) the console previews before publish, so a conflict shows the moment it is
 * created rather than as a `422` on publish. The console mirrors only the boolean check; this
 * description is the framework's own wording, and the server re-runs the real rules on publish.
 */
export interface CapabilityRule {
  readonly id: string;
  readonly description: string;
}

/** The whole capability catalogue `GET /admin/capabilities` serves for the form editor. */
export interface CapabilityCatalogue {
  readonly flags: readonly CapabilityFlag[];
  readonly presets: readonly CapabilityPreset[];
  readonly rules: readonly CapabilityRule[];
}

// --- Operational alerts (ADR-0073, Track O2): the alert engine's read model ---

/** How serious an alert is (ADR-0073), ordered least-to-most in the console. */
export type AlertSeverity = "info" | "warning" | "critical";

/**
 * One operational alert from `GET /admin/alerts` (ADR-0073, Track O2). `tenant_id` is null for a
 * server-wide condition; timestamps are Unix ms; `kind` is the stable wire token the console localizes
 * (`store_offline`, `relay_backlog`, `webhook_disabled`, `projector_unhealthy`, `jetstream_capacity`,
 * `print_agent_stalled`);
 * `detail` is a small JSON object of the numbers behind the alert. `resolved_at_ms` is null while the
 * alert is active; `acknowledged_at_ms` is null until an operator acknowledges it.
 */
export interface Alert {
  readonly id: string;
  readonly tenant_id: string | null;
  readonly kind: string;
  readonly dedup_key: string;
  readonly severity: AlertSeverity;
  readonly summary: string;
  readonly detail: Json;
  readonly first_seen_at_ms: number;
  readonly last_seen_at_ms: number;
  readonly resolved_at_ms: number | null;
  readonly acknowledged_at_ms: number | null;
}
