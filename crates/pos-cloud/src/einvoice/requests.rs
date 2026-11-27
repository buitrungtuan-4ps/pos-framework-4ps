// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The queue of correction requests a store manager sent from the till
//! ([ADR-0250](../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)
//! decision 3). The e-invoice feed records each `fiscal.correction.requested` once, as `PENDING`;
//! HQ or the accountant approves or declines it in the console. People are named by id only.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pos_proto::einvoice::CorrectionRequest;
use pos_proto::envelope::EventEnvelope;
use pos_proto::ids::{StoreId, TenantId};
use store_postgres::CorrectionRequestRow;

use super::feed::{FeedError, FeedFuture, kind_token};

/// Waiting for HQ.
pub const STATUS_PENDING: &str = "PENDING";
/// HQ approved: the store may void the bill once.
pub const STATUS_APPROVED: &str = "APPROVED";
/// HQ declined: the void stays refused.
pub const STATUS_DECLINED: &str = "DECLINED";
/// Requests one list read returns.
pub const LIST_LIMIT: i64 = 1_000;
/// The most requests [`InMemoryCorrectionRequests`] holds.
pub const IN_MEMORY_LIMIT: usize = 10_000;

/// The queue: a table in `store-postgres`, or [`InMemoryCorrectionRequests`] in tests.
pub trait CorrectionRequests: Send + Sync {
    /// Records a request unless it is recorded; `true` when it was new.
    fn record(&self, row: CorrectionRequestRow) -> FeedFuture<'_, bool>;
    /// The tenant's requests, oldest first, optionally of one status.
    fn list(
        &self,
        tenant: TenantId,
        status: Option<&'static str>,
    ) -> FeedFuture<'_, Vec<CorrectionRequestRow>>;
    /// The newest approved request for `bill_id` at `store`, whose kind HQ approved (ADR-0256).
    fn approved_for_bill(
        &self,
        tenant: TenantId,
        store: StoreId,
        bill_id: String,
    ) -> FeedFuture<'_, Option<CorrectionRequestRow>>;
    /// One request.
    fn get(
        &self,
        tenant: TenantId,
        request_id: String,
    ) -> FeedFuture<'_, Option<CorrectionRequestRow>>;
    /// Decides a `PENDING` request; `false` when it was already decided or is not there.
    fn decide(
        &self,
        tenant: TenantId,
        request_id: String,
        status: &'static str,
        decided_by_user_id: String,
        decided_time_ms: i64,
    ) -> FeedFuture<'_, bool>;
}

/// The row a `fiscal.correction.requested` in `tenant`'s log records: `PENDING`, the requester
/// from the envelope.
#[must_use]
pub fn requested_row<P>(
    tenant: TenantId,
    envelope: &EventEnvelope<P>,
    request: &CorrectionRequest,
) -> CorrectionRequestRow {
    CorrectionRequestRow {
        tenant_id: tenant.to_string(),
        request_id: envelope.event_id.to_string(),
        store_id: envelope.store_id.to_string(),
        bill_id: request.bill_id.to_string(),
        amount_minor: request.amount.amount_minor,
        currency_code: request.amount.currency_code.as_str().to_owned(),
        kind: kind_token(request.kind.known()).to_owned(),
        reason_code_id: request.reason_code_id.map(|id| id.to_string()),
        requested_by: envelope.employee_id.map(|id| id.to_string()),
        requested_time_ms: envelope.event_time.as_milliseconds_since_epoch(),
        status: STATUS_PENDING.to_owned(),
        decided_by_user_id: None,
        decided_time_ms: None,
    }
}

fn text<E: core::fmt::Display>(error: E) -> FeedError {
    error.to_string()
}

impl CorrectionRequests for store_postgres::PostgresCorrectionRequests {
    fn record(&self, row: CorrectionRequestRow) -> FeedFuture<'_, bool> {
        Box::pin(async move { Self::record(self, &row).await.map_err(text) })
    }

    fn list(
        &self,
        tenant: TenantId,
        status: Option<&'static str>,
    ) -> FeedFuture<'_, Vec<CorrectionRequestRow>> {
        Box::pin(async move {
            Self::list(self, &tenant.to_string(), status, LIST_LIMIT)
                .await
                .map_err(text)
        })
    }

    fn approved_for_bill(
        &self,
        tenant: TenantId,
        store: StoreId,
        bill_id: String,
    ) -> FeedFuture<'_, Option<CorrectionRequestRow>> {
        Box::pin(async move {
            Self::approved_for_bill(self, &tenant.to_string(), &store.to_string(), &bill_id)
                .await
                .map_err(text)
        })
    }

    fn get(
        &self,
        tenant: TenantId,
        request_id: String,
    ) -> FeedFuture<'_, Option<CorrectionRequestRow>> {
        Box::pin(async move {
            Self::get(self, &tenant.to_string(), &request_id)
                .await
                .map_err(text)
        })
    }

    fn decide(
        &self,
        tenant: TenantId,
        request_id: String,
        status: &'static str,
        decided_by_user_id: String,
        decided_time_ms: i64,
    ) -> FeedFuture<'_, bool> {
        Box::pin(async move {
            Self::decide(
                self,
                &tenant.to_string(),
                &request_id,
                status,
                &decided_by_user_id,
                decided_time_ms,
            )
            .await
            .map_err(text)
        })
    }
}

/// Requests in memory, bounded at [`IN_MEMORY_LIMIT`], for tests and a cloud run without a database.
#[derive(Debug, Clone, Default)]
pub struct InMemoryCorrectionRequests {
    rows: Arc<Mutex<BTreeMap<(String, String), CorrectionRequestRow>>>,
}

impl InMemoryCorrectionRequests {
    fn with<T>(
        &self,
        apply: impl FnOnce(
            &mut BTreeMap<(String, String), CorrectionRequestRow>,
        ) -> Result<T, FeedError>,
    ) -> Result<T, FeedError> {
        let mut rows = self.rows.lock().map_err(|_| "poisoned".to_owned())?;
        apply(&mut rows)
    }
}

impl CorrectionRequests for InMemoryCorrectionRequests {
    fn record(&self, row: CorrectionRequestRow) -> FeedFuture<'_, bool> {
        let done = self.with(|rows| {
            let key = (row.tenant_id.clone(), row.request_id.clone());
            if rows.contains_key(&key) {
                return Ok(false);
            }
            if rows.len() >= IN_MEMORY_LIMIT {
                return Err("full".to_owned());
            }
            rows.insert(key, row);
            Ok(true)
        });
        Box::pin(async move { done })
    }

    fn list(
        &self,
        tenant: TenantId,
        status: Option<&'static str>,
    ) -> FeedFuture<'_, Vec<CorrectionRequestRow>> {
        let tenant = tenant.to_string();
        let found = self.with(|rows| {
            let mut found: Vec<CorrectionRequestRow> = rows
                .values()
                .filter(|row| row.tenant_id == tenant && status.is_none_or(|s| row.status == s))
                .cloned()
                .collect();
            found.sort_by(|a, b| {
                (a.requested_time_ms, &a.request_id).cmp(&(b.requested_time_ms, &b.request_id))
            });
            found.truncate(usize::try_from(LIST_LIMIT).unwrap_or(usize::MAX));
            Ok(found)
        });
        Box::pin(async move { found })
    }

    fn approved_for_bill(
        &self,
        tenant: TenantId,
        store: StoreId,
        bill_id: String,
    ) -> FeedFuture<'_, Option<CorrectionRequestRow>> {
        let (tenant, store) = (tenant.to_string(), store.to_string());
        let found = self.with(|rows| {
            Ok(rows
                .values()
                .filter(|row| {
                    row.tenant_id == tenant
                        && row.store_id == store
                        && row.bill_id == bill_id
                        && row.status == "APPROVED"
                })
                .max_by(|a, b| {
                    (a.decided_time_ms, &a.request_id).cmp(&(b.decided_time_ms, &b.request_id))
                })
                .cloned())
        });
        Box::pin(async move { found })
    }

    fn get(
        &self,
        tenant: TenantId,
        request_id: String,
    ) -> FeedFuture<'_, Option<CorrectionRequestRow>> {
        let found = self.with(|rows| Ok(rows.get(&(tenant.to_string(), request_id)).cloned()));
        Box::pin(async move { found })
    }

    fn decide(
        &self,
        tenant: TenantId,
        request_id: String,
        status: &'static str,
        decided_by_user_id: String,
        decided_time_ms: i64,
    ) -> FeedFuture<'_, bool> {
        let done = self.with(|rows| {
            let Some(row) = rows.get_mut(&(tenant.to_string(), request_id)) else {
                return Ok(false);
            };
            if row.status != STATUS_PENDING {
                return Ok(false);
            }
            status.clone_into(&mut row.status);
            row.decided_by_user_id = Some(decided_by_user_id);
            row.decided_time_ms = Some(decided_time_ms);
            Ok(true)
        });
        Box::pin(async move { done })
    }
}
