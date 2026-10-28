// The wire shapes the dashboard exchanges with pos_cloud's `/admin` surface. Field names match the
// server's serde structs exactly (ADR-0060); where a payload is an open document — a composed
// config, a translation grid — it is typed as JSON rather than pinned, because the server treats it
// that way too.

import type {
  Ingredient,
  Money,
  Quantity,
  Supplier,
  TranslationImportReport,
} from "./schemas";

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

/**
 * The paper a printer takes (ADR-0160), as its wire token: 80 mm at 42 characters a line, 80 mm at
 * 48, or 58 mm at 32. The cloud refuses any other.
 */
export type PaperWidth =
  | "PAPER_WIDTH_MILLIMETRES_80"
  | "PAPER_WIDTH_MILLIMETRES_80_COLUMNS_48"
  | "PAPER_WIDTH_MILLIMETRES_58";

/** The papers an operator can pick, in the order the printer form offers them. */
export const PAPER_WIDTHS: readonly PaperWidth[] = [
  "PAPER_WIDTH_MILLIMETRES_80",
  "PAPER_WIDTH_MILLIMETRES_80_COLUMNS_48",
  "PAPER_WIDTH_MILLIMETRES_58",
];

/** The key `DailyCash.by_drawer` files the store's one drawer under (ADR-0167 decision 5). */
export const STORE_DRAWER = "store";

/**
 * The same rail's report for the **item master** (roadmap-v3 **F15**).
 *
 * Structurally identical because it is the same rail — one row per CSV row, in file order, with the
 * totals the review dialog headlines. What a row's `key` names differs: the item's id where the file
 * named one, otherwise the name it gave. No price appears anywhere in it, because the item CSV has
 * no price column: a price is a per-channel placement, not an item field.
 */
export type ItemImportReport = TranslationImportReport;

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

/** What a batch did at one member (ADR-0122 §6). Three outcomes, never two. */
export type BatchOutcome = "applied" | "skipped" | "failed";

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
  readonly channels?: readonly string[];
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

/**
 * Where a config release stands (`ReleaseStatus`, ADR-0125 §4).
 *
 * `partial` is the one the record exists for: N × M writes fail individually, and at forty stores an
 * operator needs the two shops that did not take it, not a red cross over the fleet. There is no
 * `rolled_back` — an applied write is a config version, and the way back is its version history.
 */
export type ReleaseStatus = "draft" | "scheduled" | "applying" | "applied" | "partial";

/** One node of a release, and the arguments its compiler needs — the same pair a batch publish takes. */
export interface ReleaseNode {
  readonly node: string;
  readonly arguments: Json;
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

/**
 * An action a reason must be cited for (`ReasonAction`); wire tokens are prefixed `REASON_ACTION_`
 * (ADR-0115).
 *
 * Thirteen, one per event field in the catalogue that declares a `reason_code_id` —
 * `docs/pos-spec.md` §11 item 2 names six and the events demand seven more, so an operator can
 * author a reason for a cash paid-in, a stock correction, a waived fee or a drawer closed over or
 * short rather than leaving that field unfillable.
 */
export type ReasonAction =
  | "REASON_ACTION_VOID_LINE"
  | "REASON_ACTION_VOID_BILL"
  | "REASON_ACTION_DISCOUNT"
  | "REASON_ACTION_COMP"
  | "REASON_ACTION_REFUND"
  | "REASON_ACTION_DRAWER_OPEN"
  | "REASON_ACTION_REJECT_ORDER"
  | "REASON_ACTION_CASH_PAID_IN"
  | "REASON_ACTION_CASH_PAID_OUT"
  | "REASON_ACTION_STOCK_ADJUSTMENT"
  | "REASON_ACTION_STOCK_WASTE"
  | "REASON_ACTION_WAIVE_FEE"
  | "REASON_ACTION_CASH_VARIANCE";

/** Every action a reason can be tagged for, in the order the picker lists them. */
export const REASON_ACTIONS: readonly ReasonAction[] = [
  "REASON_ACTION_VOID_LINE",
  "REASON_ACTION_VOID_BILL",
  "REASON_ACTION_DISCOUNT",
  "REASON_ACTION_COMP",
  "REASON_ACTION_REFUND",
  "REASON_ACTION_DRAWER_OPEN",
  "REASON_ACTION_REJECT_ORDER",
  "REASON_ACTION_CASH_PAID_IN",
  "REASON_ACTION_CASH_PAID_OUT",
  "REASON_ACTION_STOCK_ADJUSTMENT",
  "REASON_ACTION_STOCK_WASTE",
  "REASON_ACTION_WAIVE_FEE",
  "REASON_ACTION_CASH_VARIANCE",
];

/**
 * One entry in a tenant's managed reason list (`PublishedReasonCode`) — the handle a report groups
 * by, the name staff read, its per-locale names, the actions it may be cited for, and whether it is
 * still offered.
 *
 * `active: false` is how an entry leaves service: a historic `sales.order_line.voided` names its id
 * forever, so it stays in the list and resolves for a report while appearing on no picker.
 */
export interface ReasonCode {
  readonly id: string;
  readonly code: string;
  readonly display_name: string;
  readonly display_name_translations?: Record<string, string>;
  readonly applies_to: ReasonAction[];
  readonly active: boolean;
  readonly etag: ETag;
  /**
   * When this entry was last written, in Unix milliseconds.
   *
   * Optional because a read from a cloud that predates the column reaching the wire omits it, and
   * the honest answer there is "not recorded" rather than a date the server did not send. The
   * publish bar takes the newest across the list to answer "is what I am looking at newer than
   * what the shop is running?", which before this it could only guess at within the session.
   */
  readonly updated_at_ms?: number;
}

/**
 * What a reason-code publish put on a store (ADR-0115) — how many entries went on, how many staff
 * may pick, and the acts the published list leaves with **no** reason to cite.
 *
 * `uncovered_actions` is the part that matters: a published list replaces the framework set, so an
 * action with no entry has an empty picker and cannot be recorded at all. That may be exactly what
 * the operator meant; the console says it out loud either way.
 */
export interface ReasonCodePublishResult {
  readonly config_version_id: string;
  readonly published: number;
  readonly active: number;
  readonly uncovered_actions: ReasonAction[];
}

/**
 * The authoring fields of a reason-code create/update — a `ReasonCode` without its server-minted id
 * or the version it was read at (a write sends that as `If-Match`, not in the body).
 */
export type ReasonCodeInput = Omit<ReasonCode, "id" | "etag" | "updated_at_ms">;

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

/** The authoring fields of a supplier create/update — a `Supplier` without its server-owned id. */
export type SupplierInput = Omit<Supplier, "id" | "etag">;

/**
 * A QR ordering guardrail node (`qr`, ADR-0080) as read/published from the config tree.
 *
 * `enabled` follows QR ordering's switch, {@link QR_ORDERING_SWITCH}, which the cloud writes into it
 * with every publish (ADR-0160 decision 5). This console never sends it: a value sent here is
 * written as the switch, which is for an older console and for a cohort copy made by one.
 */
export interface QrGuardrails {
  readonly enabled?: boolean;
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

/** The response to a media upload (ADR-0075): the id to reference the new asset by. */
export interface UploadedMedia {
  readonly media_id: string;
  readonly detail_bytes: number;
}

/** A console admin's least-privilege tier (ADR-0067). `owner` manages other admins; `viewer` is read-only. */
export type AdminRole = "owner" | "admin" | "ops" | "viewer";

/** The roles, in privilege order, for the console's role pickers. */
export const ADMIN_ROLES: readonly AdminRole[] = ["owner", "admin", "ops", "viewer"];

/** Whether a console admin can sign in (ADR-0067). A suspended admin keeps its history but cannot. */
export type AdminStatus = "active" | "suspended";

/**
 * One store's fleet row from `GET /admin/fleet` (ADR-0068 slice 3): identity + status joined with
 * liveness, config drift, and relay backlog. `online` and `config_current` are the server's read-time
 * verdicts; the raw instants are Unix ms (or `null` for a store never seen / never configured).
 */

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
 * Where an assignment grants its role (ADR-0158 decision 3): one store, a store group (ADR-0122), or
 * every store of the tenant. A kind this console does not know reads as unspecified.
 */
export type AssignmentScopeKind =
  | "ASSIGNMENT_SCOPE_UNSPECIFIED"
  | "ASSIGNMENT_SCOPE_STORE"
  | "ASSIGNMENT_SCOPE_STORE_GROUP"
  | "ASSIGNMENT_SCOPE_TENANT";

/** Where a new assignment grants its role, as the People screen's "Where" chooses it. */
export type AssignmentTarget =
  | { readonly kind: "store"; readonly storeId: string }
  | { readonly kind: "group"; readonly groupId: string }
  | { readonly kind: "tenant" };

// --- Floor & kitchen (ADR-0072, Track M2): per-store areas/tables and kitchen stations/routing ---

// --- Capabilities (ADR-0071, Track M8): the §10 flag catalogue the Config screen's form editor reads ---

/**
 * QR ordering's one switch (ADR-0160 decision 5): the capability flag the cloud's guest intake and
 * the edge read, off unless a store's configuration turns it on. `qr.enabled` and the QR sales
 * channel follow it. It is offered on Channels & payments, beside the QR guardrails, and nowhere
 * else, so the Config screen's capability form leaves it out.
 */
export const QR_ORDERING_SWITCH = "qr_ordering_enabled";

// --- Operational alerts (ADR-0073, Track O2): the alert engine's read model ---

/** How serious an alert is (ADR-0073), ordered least-to-most in the console. */
export type AlertSeverity = "info" | "warning" | "critical";

/**
 * A kind of external system the store works with (ADR-0153). The prefix of every provider id says
 * the same thing (`einvoice.…`); this is the enum spelling the admin API uses.
 */
export type IntegrationFamily =
  | "INTEGRATION_FAMILY_E_INVOICE"
  | "INTEGRATION_FAMILY_QR_PAYMENT"
  | "INTEGRATION_FAMILY_CARD_TERMINAL"
  | "INTEGRATION_FAMILY_DELIVERY"
  | "INTEGRATION_FAMILY_COURIER"
  | "INTEGRATION_FAMILY_ERP";

/** What a settings field holds, and so which input the console draws for it. */
export type SettingKind =
  | "SETTING_KIND_TEXT"
  | "SETTING_KIND_URL"
  | "SETTING_KIND_NUMBER"
  | "SETTING_KIND_CHOICE"
  | "SETTING_KIND_FLAG"
  | "SETTING_KIND_SECRET";

/** A setting's value as the operator typed it: the plain JSON the schema's kind implies. */
export type SettingValue = string | number | boolean;

/** Which layer of the config tree a connection serves. */
export type ConnectionScopeLevel =
  | "CONNECTION_SCOPE_TENANT"
  | "CONNECTION_SCOPE_BRAND"
  | "CONNECTION_SCOPE_STORE";

/**
 * A connection write. `secrets` holds only what this write sets: a secret left out keeps the stored
 * value, and `clear_secrets` removes one on purpose.
 */
export interface ConnectionInput {
  readonly provider_id: string;
  readonly scope_level: ConnectionScopeLevel;
  readonly scope_id?: string;
  readonly display_name: string;
  readonly enabled: boolean;
  readonly settings: Record<string, SettingValue>;
  readonly secrets: Record<string, string>;
  readonly clear_secrets: readonly string[];
}

/**
 * Where a setting may be written ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
 * decision 3): every store of the tenant, every store of one brand, every store in one store group,
 * or one store. A store runs the value of the most specific of these that sets one.
 */
export type SettingScope =
  | "SETTING_SCOPE_TENANT"
  | "SETTING_SCOPE_BRAND"
  | "SETTING_SCOPE_STORE_GROUP"
  | "SETTING_SCOPE_STORE";

/** How one store's settings publish went. Three outcomes, never two. */
export type SettingPublishOutcome =
  | "SETTING_PUBLISH_APPLIED"
  | "SETTING_PUBLISH_UNCHANGED"
  | "SETTING_PUBLISH_FAILED";

// --- Fees (ADR-0159) ------------------------------------------------------------------------------

/** Where a fee rule is written: every store of the tenant, every store of one brand, or one store. */
export type FeeScope = "FEE_SCOPE_TENANT" | "FEE_SCOPE_BRAND" | "FEE_SCOPE_STORE";

/** How a fee is charged: a share of its lines, an amount per bill, or an amount per unit. */
export type FeeKind = "FEE_KIND_PERCENT" | "FEE_KIND_AMOUNT_PER_BILL" | "FEE_KIND_AMOUNT_PER_UNIT";

/** Which lines a fee counts: every line, only those listed, or every line but those listed. */
export type FeeItems = "FEE_ITEMS_ALL" | "FEE_ITEMS_INCLUDE" | "FEE_ITEMS_EXCLUDE";

/** How a fee is taxed: not at all, the way its lines are, or at one named tax class. */
export type FeeTax = "FEE_TAX_NOT_TAXABLE" | "FEE_TAX_FOLLOW_LINES" | "FEE_TAX_TAX_CLASS";

/** An exact ratio (`pos-proto` `Ratio`): 5 % is `{ numerator: 5, denominator: 100 }`. Never a float. */
export interface Ratio {
  readonly numerator: number;
  readonly denominator: number;
}

/**
 * A fee rule's own fields, as `/admin/fees` reads and writes them: the wire `PublishedFee`, plus
 * the item categories an include or exclude list names, which the cloud compiles into items when it
 * publishes a store. A field left out reads as the owner's default.
 */
export interface FeeRuleFields {
  /** Absent on a create, which mints it. */
  readonly fee_id?: string;
  readonly code: string;
  readonly display_name: string;
  readonly display_name_translations?: Readonly<Record<string, string>>;
  readonly kind?: FeeKind | string;
  readonly rate?: Ratio;
  readonly amount?: Money;
  /** Empty, or absent, is every channel. */
  readonly channels?: readonly string[];
  readonly item_scope?: FeeItems | string;
  readonly menu_item_ids?: readonly string[];
  readonly item_category_ids?: readonly string[];
  readonly base_discounted?: boolean;
  readonly base_tax_inclusive?: boolean;
  readonly tax?: FeeTax | string;
  readonly tax_class_id?: string;
  readonly waivable?: boolean;
  readonly active?: boolean;
}

/** One rule as written, at one scope, from `GET /admin/fees`. */
export interface FeeRule {
  readonly scope: FeeScope | string;
  /** The tenant, brand or store the rule is written for. */
  readonly scope_id: string;
  readonly rule: FeeRuleFields & { readonly fee_id: string };
  /** RFC 3339. */
  readonly update_time: string;
  /** The console admin who last wrote it, by id. */
  readonly updated_by: string;
}

/** How one store's fee publish went. */
export type FeePublishOutcome =
  | "FEE_PUBLISH_APPLIED"
  | "FEE_PUBLISH_UNCHANGED"
  | "FEE_PUBLISH_REFUSED"
  | "FEE_PUBLISH_FAILED";

/** One fee as one store runs it, from `GET /admin/fees/effective`. */
export interface EffectiveFee {
  readonly fee_id: string;
  /** The scope the rule in force was written at, and what for. */
  readonly scope: FeeScope | string;
  readonly scope_id: string;
  /** The rule as written. */
  readonly rule: FeeRuleFields;
  /** The rule as the store is sent it, its categories compiled into items. */
  readonly published: FeeRuleFields;
  /** Why the store cannot apply it, when it cannot. */
  readonly faults?: readonly string[];
}

/** What one store runs, and whether it can be published to as it stands. */
export interface EffectiveFees {
  readonly store_id: string;
  readonly fees: readonly EffectiveFee[];
  readonly publishable: boolean;
}

/** One rule a preview writes in place, as a `PUT` sends it. */
export interface PreviewFeeRule {
  readonly scope: FeeScope;
  readonly scope_id: string;
  readonly rule: FeeRuleFields;
}

/** One line of a sample bill: an item on the store's menu, and how many. */
export interface SampleLineInput {
  readonly menu_item_id: string;
  readonly quantity: number;
}

