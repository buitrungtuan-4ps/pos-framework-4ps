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
  readonly tax_class_id: string;
  readonly item_category_id: string | null;
  readonly item_subcategory_id: string | null;
  readonly status: EntityStatus;
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
