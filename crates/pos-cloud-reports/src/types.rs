// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The reports' types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A day's activity for one store, derived from the event log.
///
/// The cloud's dashboards answer from rollups rather than by scanning the raw log
/// (`docs/roadmap.md` P7). This first rollup counts events per store trading day and per event
/// type — enough for an activity dashboard, and it uses only envelope fields (`business_date`,
/// `event_type`), so it needs no per-event-type decoding. Money- and order-shaped rollups, and the
/// materialised tables that make the <10 ms dashboard query real, land with the dashboard slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DailyRollup {
    /// The store's trading day, `YYYY-MM-DD` ([ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)).
    pub business_date: String,
    /// Every event recorded on that day.
    pub total_events: u64,
    /// Per-event-type counts, ordered by type so the output is deterministic.
    pub by_type: BTreeMap<String, u64>,
}

/// A day's **revenue** for one store, folded from the settlement and line events
/// ([ADR-0081](../../../docs/adr/0081-reports-and-analytics.md), Track O4).
///
/// Revenue is recognised from `billing.bill.settled` (the settled totals); the product mix is the
/// **gross ordered** mix from `sales.order_line.added` — units and value as ordered, **before** voids
/// and comps are netted (which the line events do not carry back to a menu item), so it reads menu
/// popularity, not per-item recognised revenue. All amounts are the store's single currency's minor
/// units (`docs/pos-spec.md` §19). Prices are **T2**: this rollup, and every route that serves it, is
/// gated behind `console.reports.revenue`. It carries no customer or employee identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DailyRevenue {
    /// The store's trading day, `YYYY-MM-DD`.
    pub business_date: String,
    /// ISO 4217 code of the store's currency, or empty until the first settled bill sets it.
    pub currency_code: String,
    /// Settled bills on the day.
    pub bills: u64,
    /// Sum of settled subtotals (before reductions), minor units.
    pub gross: i64,
    /// Sum of reductions (discounts + comps), minor units.
    pub reductions: i64,
    /// Sum of service charge, minor units.
    pub service_charge: i64,
    /// Sum of tax, minor units.
    pub tax: i64,
    /// Sum of `total_due` — what guests owed — minor units. The headline "revenue" figure.
    pub net: i64,
    /// Gross ordered mix, keyed by `menu_item_id`, ordered by key for determinism.
    pub by_item: BTreeMap<String, ItemMix>,
    /// The fees the day's settled bills charged, keyed by the fee's `code` and ordered by it
    /// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 4).
    ///
    /// Folded from each `billing.bill.settled`'s `fee_lines`, whose amounts sum to that bill's
    /// `service_charge`. So on a day whose every bill recorded its fee lines, these amounts sum to
    /// [`service_charge`](Self::service_charge). A bill settled by an edge from before fee lines
    /// records none, and adds to `service_charge` alone. There is no entry for that difference: a
    /// bucket for it would read as a fee nobody wrote, so the gap is left as the gap between the
    /// two figures.
    ///
    /// Bounded by the store's fee rules, not by its trade: one entry per code a rule charged that
    /// day, and the cloud refuses a second active fee under a code a store already runs
    /// (`pos_cloud::fees::shares_code_with`).
    ///
    /// `#[serde(default)]`, so a day stored before this field loads with no fees and gains them
    /// from the rollup's cursor forward. Resetting the rollup (the ADR-0036 reset-cursor-and-replay
    /// lever) folds every settled bill on the log into it.
    #[serde(default)]
    pub by_fee: BTreeMap<String, FeeTotal>,
    /// The tax the day's settled bills recorded by tax component, one entry per component name and
    /// rate, in order of name and then rate
    /// ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 5).
    ///
    /// Folded from each `billing.bill.settled`'s tax lines' `components` alone, never from its fee
    /// lines' `tax_components`, because a fee's tax is already inside the tax lines as its share of
    /// its class's tax, and folding both would count it twice.
    ///
    /// So on a day whose every bill recorded its components, these sum to [`tax`](Self::tax). A
    /// bill that recorded none adds to `tax` alone: one settled before its edge recorded them, one
    /// taxed at rates with no components, as every Vietnamese and Japanese bill is, and one whose
    /// names are not tokens, which reads as not split. There is no entry for that difference: a
    /// bucket for it would read as a component no bill named, so the gap is left as the gap between
    /// the two figures, as it is for [`by_fee`](Self::by_fee).
    ///
    /// Bounded by the tenant's tax table, not by the store's trade: one entry per component name and
    /// rate a row of the table in force named that day.
    ///
    /// Left out when empty, so a day with none serialises byte for byte as before this field, and
    /// `#[serde(default)]`, so a day stored before it loads with none and gains them from the
    /// rollup's cursor forward. Resetting the rollup re-folds every settled bill on the log into it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_tax_component: Vec<TaxComponentTotal>,
}

/// One tax component's tax on a trading day, at one rate (part of [`DailyRevenue`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TaxComponentTotal {
    /// The component's name as the bills recorded it, such as `CGST`: a
    /// [`TaxComponentName`](pos_proto::locale::TaxComponentName) when it was folded. Text here, so
    /// a stored day loads whatever a later release lets a name be.
    pub component_name: String,
    /// Its part of the rate, in basis points: CGST at 2.5 % and at 9 % are two entries.
    pub rate_basis_points: u32,
    /// Sum of the tax the day's tax lines recorded under it, minor units.
    pub tax: i64,
}

/// One fee's total on a trading day, under its code (part of [`DailyRevenue`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
pub struct FeeTotal {
    /// The most recent name a settled bill recorded for the fee, for a human-readable report.
    pub name: String,
    /// Settled bills that charged the fee.
    pub bills: u64,
    /// Sum of what the fee charged, minor units.
    pub amount: i64,
    /// Sum of the tax on it, minor units.
    pub tax: i64,
}

/// One menu item's gross ordered contribution on a trading day (part of [`DailyRevenue`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
pub struct ItemMix {
    /// The most recent display name seen for the item, for a human-readable report.
    pub name: String,
    /// Sum of ordered quantity, in thousandths of a unit (`Quantity::milli`).
    pub ordered_qty_milli: i64,
    /// Sum of ordered line totals, minor units — gross, before voids/comps.
    pub ordered_value: i64,
}

/// A day's **cash-drawer** summary for one store, folded from the shift and drawer events
/// ([ADR-0081](../../../docs/adr/0081-reports-and-analytics.md), Track O4). Amounts are the store's
/// single currency's minor units. Part of an X/Z report; T2 (it exposes money).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
pub struct DailyCash {
    /// The store's trading day, `YYYY-MM-DD`.
    pub business_date: String,
    /// ISO 4217 code, or empty until a cash event on the day sets it.
    pub currency_code: String,
    /// Sum of opening floats across shifts opened on the day.
    pub opening_float: i64,
    /// Sum of paid-in movements.
    pub paid_in: i64,
    /// Sum of paid-out movements.
    pub paid_out: i64,
    /// Shifts opened on the day.
    pub shifts_opened: u64,
    /// Shifts closed on the day.
    pub shifts_closed: u64,
    /// Sum of expected drawer amounts across closes.
    pub expected: i64,
    /// Sum of counted amounts across closes (the blind counts).
    pub counted: i64,
    /// Sum of variance (counted − expected) across closes; negative is short.
    pub variance: i64,
    /// The same figures for each drawer, keyed by the till's `terminal_device_id`, or by
    /// [`STORE_DRAWER`] for the store's one drawer
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions 10 and 13).
    /// Each drawer's are the sums of its own events, which are the events the store's figures sum,
    /// so the drawers add up to the store.
    ///
    /// `#[serde(default)]`, so a day stored before this field loads with no drawers and gains them
    /// from the rollup's cursor forward. Resetting the rollup (the ADR-0036 reset-cursor-and-replay
    /// lever) folds every cash event on the log into it.
    #[serde(default)]
    pub by_drawer: BTreeMap<String, CashTotals>,
}

/// The key [`DailyCash::by_drawer`] files the store's one drawer under: the drawer of every cash
/// event that names no till, which is every event of a store that keeps one drawer (ADR-0167
/// decision 5).
///
/// It cannot be a till's: a till is keyed by its `terminal_device_id`, a 26-character ULID, and this
/// is not one. And it says what it is wherever the map is read, the stored rollup, the X/Z report
/// and the console, where an empty key would say nothing and a reserved ULID would pass for a till.
pub const STORE_DRAWER: &str = "store";

/// A day's cash figures for one drawer (part of [`DailyCash`]), the same as the store's.
///
/// The store's stay inline on [`DailyCash`] rather than in one of these under
/// `#[serde(flatten)]`, which would keep the wire as it is and does not pass the lints: the
/// flatten buffer carries `f32` and `f64`, which `clippy.toml` bans (money is never a float).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
pub struct CashTotals {
    /// Sum of opening floats across shifts opened on the day.
    pub opening_float: i64,
    /// Sum of paid-in movements.
    pub paid_in: i64,
    /// Sum of paid-out movements.
    pub paid_out: i64,
    /// Shifts opened on the day.
    pub shifts_opened: u64,
    /// Shifts closed on the day.
    pub shifts_closed: u64,
    /// Sum of expected drawer amounts across closes. Only a close carries one: the edge works out
    /// what a drawer should hold at its close, so a shift still open adds nothing here.
    pub expected: i64,
    /// Sum of counted amounts across closes (the blind counts).
    pub counted: i64,
    /// Sum of variance (counted − expected) across closes; negative is short.
    pub variance: i64,
}

/// One device a store has admitted, as the fleet console finally sees it
/// ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
///
/// Folded from `device.admission.granted` and `device.admission.revoked`. Until those events the
/// cloud's picture of a store's tills was **zero rows**, so a lost tablet meant sending somebody to
/// the store and a remote revocation had nothing to name.
///
/// # Not the console's named-device registry, and not confusable with it
///
/// `local_device_id` is minted by the **edge** at redemption (ADR-0030); the `devices` table's ids
/// are minted by the **console** when an admin names a terminal. Nothing joins the two, by decision
/// — a browser till presents no identity of its own and clearing its storage creates a new device —
/// so this roster sits beside that registry rather than inside it. A row here means *this store
/// admitted this thing*, which is the fact the registry has never been able to state.
///
/// **T3 (internal): device identifiers and instants, no money and no person.** Who admitted a
/// device is deliberately absent — that is recorded on the store's own disk and reaches no cloud
/// (ADR-0118 §5). `admitted_by_device_id` is a *device*, the till whose signed-in manager minted the
/// code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
pub struct AdmittedDevice {
    /// The edge-minted device id the store issued a token to.
    pub local_device_id: String,
    /// When it was admitted, Unix ms — the event's own instant, so a re-projection reproduces it.
    pub admitted_at_ms: i64,
    /// The paired till whose signed-in manager minted the code, where one did. `None` for the code a
    /// box announces at boot, which nothing authorised because nothing could.
    #[schema(required = true)]
    pub admitted_by_device_id: Option<String>,
    /// When the store retired it, Unix ms, or `None` while it is still admitted. A revocation does
    /// not delete the row: a roster that forgets what it retired cannot answer "was this tablet ever
    /// here", which is the first question asked about a device that turns up somewhere it should not.
    #[schema(required = true)]
    pub revoked_at_ms: Option<i64>,
}

/// An **X or Z report** for one store's trading day (ADR-0081, Track O4, resolving spec gap D10).
///
/// An **X** report is the current (open) day's running totals — non-resetting, recomputed each call.
/// A **Z** report is a closed day's totals — a day that no longer receives events, so the same read
/// returns the same figures verbatim thereafter. `kind` is `"X"` for the latest day present and
/// `"Z"` for any earlier (closed) day. It bundles the day's activity counts, revenue, and cash
/// summary; T2, so it is served only behind `console.reports.revenue`. Not a legal fiscal document —
/// that stays with the country module's invoice range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct XzReport {
    /// `"X"` (current, interim) or `"Z"` (closed, final).
    pub kind: String,
    /// The trading day, `YYYY-MM-DD`.
    pub business_date: String,
    /// The day's event-activity counts.
    pub activity: DailyRollup,
    /// The day's recognised revenue and product mix.
    pub revenue: DailyRevenue,
    /// The day's cash-drawer summary.
    pub cash: DailyCash,
    /// The name of each till among the day's drawers, keyed as [`DailyCash::by_drawer`] is, from the
    /// store's published `devices` node when the report is served (ADR-0167 decision 13). The
    /// store's drawer, and a till the node no longer lists, have none. Device names, never a person.
    #[serde(default)]
    pub till_names: BTreeMap<String, String>,
}
