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
  readonly scopes: string[];
  readonly revoked: boolean;
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

/** A tenant from `GET /admin/tenants` (ADR-0065) — the root of the org tree. */
export interface Tenant {
  readonly tenant_id: string;
  readonly name: string;
  readonly status: EntityStatus;
}

/** A brand from `GET /admin/brands` — grouped under a tenant. */
export interface Brand {
  readonly brand_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly status: EntityStatus;
}

/** A store from `GET /admin/stores` — grouped under a tenant and, optionally, a brand. */
export interface Store {
  readonly store_id: string;
  readonly tenant_id: string;
  readonly brand_id: string | null;
  readonly name: string;
  readonly status: EntityStatus;
}

/** A device from `GET /admin/stores/{id}/devices` — the canonical device identity. */
export interface Device {
  readonly device_id: string;
  readonly tenant_id: string;
  readonly store_id: string;
  readonly name: string;
  readonly kind: string;
  readonly status: EntityStatus;
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
}

/** The authoring fields of a create/update — a `Campaign` without its server-owned id. */
export type CampaignInput = Omit<Campaign, "id">;

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
}

/** The authoring fields of an ingredient create/update — an `Ingredient` without its server-owned id. */
export type IngredientInput = Omit<Ingredient, "id">;

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
}

/** The authoring fields of a recipe upsert — the BOM and threshold; the item is the URL key. */
export type RecipeInput = Omit<Recipe, "item">;

/** A supplier reference (`PublishedSupplier`) — id and name only; purchasing lives in the ERP (§19). */
export interface Supplier {
  readonly id: string;
  readonly name: string;
}

/** The authoring fields of a supplier create/update — a `Supplier` without its server-owned id. */
export type SupplierInput = Omit<Supplier, "id">;

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
}

/** An item category — the operational taxonomy for reporting/kitchen grouping (ADR-0066 entity 2). */
export interface ItemCategory {
  readonly item_category_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly status: EntityStatus;
}

/** An item sub-category, nested under a category (ADR-0066 entity 3). */
export interface ItemSubcategory {
  readonly item_subcategory_id: string;
  readonly tenant_id: string;
  readonly item_category_id: string;
  readonly name: string;
  readonly status: EntityStatus;
}

/** A display category — the presentation taxonomy a screen groups by (ADR-0066 entity 11). */
export interface DisplayCategory {
  readonly display_category_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly status: EntityStatus;
}

/** A display sub-category, nested under a display category (ADR-0066 entity 11). */
export interface DisplaySubcategory {
  readonly display_subcategory_id: string;
  readonly tenant_id: string;
  readonly display_category_id: string;
  readonly name: string;
  readonly status: EntityStatus;
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
}

/** A menu section — an authoring grouping within a menu (ADR-0066 entity 7). Authoring-only. */
export interface MenuSection {
  readonly menu_section_id: string;
  readonly tenant_id: string;
  readonly menu_id: string;
  readonly name: string;
  readonly sort: number;
  readonly status: EntityStatus;
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
export interface AuditFilter {
  readonly tenantId?: string;
  readonly entityType?: string;
  readonly entityId?: string;
  readonly action?: string;
  readonly actorAdminId?: string;
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
}

/** A tenant's named role — a stored subset of the pos-core permission catalogue (§9, ADR-0070). */
export interface RoleTemplate {
  readonly role_template_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly permissions: readonly string[];
  readonly status: EntityStatus;
}

/** An employee's assignment to a store with a role (ADR-0070) — three ids, no PII. */
export interface Assignment {
  readonly assignment_id: string;
  readonly tenant_id: string;
  readonly employee_id: string;
  readonly store_id: string;
  readonly role_template_id: string;
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
 * (`store_offline`, `relay_backlog`, `webhook_disabled`, `projector_unhealthy`, `jetstream_capacity`);
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
