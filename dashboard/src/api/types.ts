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

/** A catalog item — the product master (ADR-0066), the source of a compiled `MenuEntry`. */
export interface CatalogItem {
  readonly menu_item_id: string;
  readonly tenant_id: string;
  readonly name: string;
  readonly tax_class_id: string;
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
  readonly prices: ChannelPrice[];
  readonly available: boolean;
}
