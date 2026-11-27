// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store's delivery reconciliation: for each trading day and delivery vendor, the orders, what
//! they took, the commission at the vendor's rate and the net the store expects to be paid out
//! ([ADR-0248](../../../../docs/adr/0248-a-delivery-order-is-priced-paid-and-reconciled-as-the-app-sold-it.md)
//! decision 3).
//!
//! The days come from the materialised rollup; the vendor's name and rate from the `vendors` node
//! the store runs now. The bill and its tax stay gross: the commission is recorded apart, here,
//! and never reaches a bill. Revenue is T2, so the read needs `console.reports.revenue`.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_cloud_reports::refusal::{rollup_error_response, window_refusal};
use pos_cloud_reports::rollups::AdminRollupWindowQuery;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::money::{CurrencyCode, Money, Ratio, Rounding};
use pos_proto::vendors::PublishedVendors;
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore};
use crate::dashboard::{RollupStore, RollupWindow, vendor_days};
use crate::http::{CloudApp, Json, strip_tree_version};

/// One vendor's orders on one trading day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct DeliveryReconciliationRow {
    /// The store's trading day, `YYYY-MM-DD`.
    pub(crate) business_date: String,
    /// The vendor: the delivery connection's id.
    pub(crate) vendor_id: String,
    /// What the store calls it, or empty for a vendor the store no longer lists.
    pub(crate) vendor_name: String,
    /// ISO 4217 code of the amounts.
    pub(crate) currency_code: String,
    /// Settled orders.
    pub(crate) orders: u64,
    /// What they took, tax included, in minor units: the bills, gross.
    pub(crate) gross: i64,
    /// The vendor's commission rate the store's configuration gives, in basis points.
    pub(crate) commission_bps: u32,
    /// The commission at that rate, rounded half up to the minor unit.
    pub(crate) commission: i64,
    /// What the store expects the vendor to pay out: `gross - commission`.
    pub(crate) net: i64,
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/delivery/reconciliation",
    extensions(("x-permission" = json!("console.reports.revenue"))),
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant that store belongs to (a 26-character ULID)"),
        ("from" = Option<String>, Query, description = "The first trading day, `YYYY-MM-DD`, inclusive"),
        ("to" = Option<String>, Query, description = "The last trading day, `YYYY-MM-DD`, inclusive"),
        ("limit" = Option<usize>, Query, description = "The most trading days to cover, the newest in \
                                                       range: 90 when absent, and never more than 366"),
    ),
    responses(
        (status = 200, body = [DeliveryReconciliationRow], description = "Each trading day's delivery vendors, oldest day \
                                      first: the orders settled, what they took gross, the commission \
                                      at the vendor's rate and the net. Amounts are integers in the \
                                      currency's minor units. No customer or employee identifier"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID, or the window \
                                      is malformed", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.reports.revenue", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The rollup store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reports",
)]
/// `GET /admin/stores/{store_id}/delivery/reconciliation` (ADR-0248 decision 3).
pub(in crate::http) async fn admin_delivery_reconciliation<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ReadRevenue,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
    };
    let days = match vendor_days(&app.rollups, tenant_id, store_id, &window).await {
        Ok(days) => days,
        Err(error) => return rollup_error_response(&error),
    };
    let vendors = published_vendors(&app.config_trees, tenant_id, store_id).await;
    let rows: Vec<DeliveryReconciliationRow> = days
        .into_iter()
        .flat_map(|(business_date, vendors_of_day)| {
            let vendors = &vendors;
            vendors_of_day.into_iter().map(move |(vendor_id, day)| {
                let entry = vendor_id.parse().ok().and_then(|id| vendors.get(id));
                let commission_bps = entry.map_or(0, |vendor| vendor.commission_bps);
                let commission = commission_of(day.gross, &day.currency_code, commission_bps);
                DeliveryReconciliationRow {
                    business_date: business_date.clone(),
                    vendor_name: entry
                        .map(|vendor| vendor.display_name.as_str().to_owned())
                        .unwrap_or_default(),
                    vendor_id,
                    currency_code: day.currency_code,
                    orders: day.orders,
                    gross: day.gross,
                    commission_bps,
                    commission,
                    net: day.gross.saturating_sub(commission),
                }
            })
        })
        .collect();
    (StatusCode::OK, Json(rows)).into_response()
}

/// The commission on `gross` at `bps` basis points, rounded half up to the minor unit, with the
/// money primitives every amount here uses. `0` for a currency or a rate that cannot be read.
fn commission_of(gross: i64, currency: &str, bps: u32) -> i64 {
    let Ok(currency) = CurrencyCode::parse(currency) else {
        return 0;
    };
    Ratio::basis_points(i64::from(bps))
        .and_then(|rate| Money::new(currency, gross).mul_ratio(rate, Rounding::HalfUp))
        .map_or(0, |commission| commission.amount_minor)
}

/// The `vendors` node the store runs now, or none.
pub(crate) async fn published_vendors<T: ConfigTreeStore>(
    config_trees: &T,
    tenant_id: TenantId,
    store_id: StoreId,
) -> PublishedVendors {
    let Ok(Some(state)) = config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    else {
        return PublishedVendors::default();
    };
    ConfigTree::from_state(store_id, CapabilityValidator, state)
        .current_effective()
        .and_then(|effective| effective.get("vendors").cloned())
        .and_then(|node| serde_json::from_value::<PublishedVendors>(node).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::commission_of;

    /// ADR-0248 decision 3: 25% of 242,000 is 60,500; half a đồng rounds up.
    #[test]
    fn the_commission_is_the_rate_of_the_gross_rounded_half_up() {
        assert_eq!(commission_of(242_000, "VND", 2_500), 60_500);
        assert_eq!(commission_of(1, "VND", 5_000), 1);
        assert_eq!(commission_of(242_000, "VND", 0), 0);
        assert_eq!(commission_of(242_000, "", 2_500), 0, "no currency yet");
    }
}
