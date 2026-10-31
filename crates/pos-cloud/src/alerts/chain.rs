// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The chain-break alert ([ADR-0073](../../../../docs/adr/0073-alerting.md) amendment 2026-10-31).
//!
//! A store's chain finding — its log disagreeing with itself — was logged and listed on the
//! console's Chain integrity screen, and never reached the Alerts screen. This turns the persisted
//! findings into one [`AlertKind::ChainBroken`] alert per store.
//!
//! # Which findings
//!
//! Exactly the ones the anchor ledger keeps, and it keeps only accusations: the two of
//! [ADR-0132](../../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md) decision 5's
//! four that say a store's log disagrees with itself — a **fork** (two records at one position) and
//! a **broken link** — plus the refused second head of
//! [ADR-0131](../../../../docs/adr/0131-a-chained-event-log.md) decision 4, which is the same
//! question answered twice. An *incomplete* window (a broker gap) and an *unverified* one are never
//! filed, so they can never raise this alert.
//!
//! # Why it does not clear
//!
//! A broken chain does not heal, so the condition never stops "firing" in any useful sense. The
//! alert stays open until a person acknowledges it ([`AlertKind::clears_itself`]), and after that
//! only a finding the cloud noticed *later* than the ones that person saw opens a new one. What they
//! saw is written into the alert's detail as [`WATERMARK`], and read back from the closed alert.

use std::collections::BTreeMap;

use pos_proto::ids::TenantId;
use pos_proto::time::Timestamp;
use serde_json::{Value, json};

use super::model::{AlertKind, FiringAlert};
use super::store::{AlertRecord, AlertStore, AlertStoreError};
use crate::anchor::{AnchorConflict, AnchorLedger};

/// How many of a tenant's newest findings one pass reads. The ledger lists newest first, so a cap
/// can only ever understate a range that is already alerting, never hide a store.
pub const FINDINGS_PER_TENANT: u32 = 500;

/// The detail field holding the newest finding the alert covers (Unix ms, the cloud's clock).
pub const WATERMARK: &str = "latest_noticed_at_ms";

/// The chain-break alerts firing for `tenants`, read from `ledger`.
///
/// A ledger that cannot be read for a tenant is logged and skipped rather than failing the pass, as
/// an unreadable stream probe is: the store-offline and webhook conditions do not need it. Skipping
/// is also safe for this kind in particular, because it never resolves on silence — an open
/// chain-break alert survives a pass that could not see its finding.
///
/// # Errors
///
/// [`AlertStoreError`] if the closed alerts could not be read.
pub async fn chain_alerts<S: AlertStore + Sync>(
    ledger: Option<&dyn AnchorLedger>,
    alerts: &S,
    tenants: &[TenantId],
) -> Result<Vec<FiringAlert>, AlertStoreError> {
    let Some(ledger) = ledger else {
        return Ok(Vec::new());
    };
    let closed = alerts.list_last_resolved(AlertKind::ChainBroken).await?;
    let mut firing = Vec::new();
    for &tenant in tenants {
        match ledger
            .list_conflicts(tenant, None, FINDINGS_PER_TENANT)
            .await
        {
            Ok(conflicts) => firing.extend(chain_broken(tenant, &conflicts, &closed)),
            Err(error) => tracing::warn!(
                %tenant,
                %error,
                "could not read the chain findings; their alert is not evaluated this pass"
            ),
        }
    }
    Ok(firing)
}

/// One store's findings past its watermark, folded into a seq range.
struct Span {
    from_seq: u64,
    to_seq: u64,
    findings: u64,
    latest_ms: i64,
}

/// One alert per store with a finding newer than what a person last closed for it. Pure.
#[must_use]
pub fn chain_broken(
    tenant: TenantId,
    conflicts: &[AnchorConflict],
    closed: &[AlertRecord],
) -> Vec<FiringAlert> {
    let mut stores: BTreeMap<String, Span> = BTreeMap::new();
    for conflict in conflicts {
        let store = conflict.report.store.to_string();
        let noticed_ms = conflict.noticed_at.as_milliseconds_since_epoch();
        if noticed_ms <= watermark(tenant, &store, closed) {
            continue;
        }
        let seq = conflict.report.chain_seq;
        let span = stores.entry(store).or_insert(Span {
            from_seq: seq,
            to_seq: seq,
            findings: 0,
            latest_ms: noticed_ms,
        });
        span.from_seq = span.from_seq.min(seq);
        span.to_seq = span.to_seq.max(seq);
        span.findings += 1;
        span.latest_ms = span.latest_ms.max(noticed_ms);
    }
    stores
        .into_iter()
        .map(|(store, span)| {
            FiringAlert::new(
                AlertKind::ChainBroken,
                Some(tenant),
                store.clone(),
                format!(
                    "Store {store}'s chained log disagrees with itself at positions {}\u{2013}{}",
                    span.from_seq, span.to_seq
                ),
                // Positions, a count and an instant: no event contents and no identifier of a
                // person, the same as the finding it summarises.
                json!({
                    "store_id": store,
                    "from_seq": span.from_seq,
                    "to_seq": span.to_seq,
                    "findings": span.findings,
                    WATERMARK: span.latest_ms,
                }),
            )
        })
        .collect()
}

/// The newest finding a closed alert for `store` covered, or `i64::MIN` when none was closed. A
/// closed alert whose detail lacks the field falls back to when it was closed.
fn watermark(tenant: TenantId, store: &str, closed: &[AlertRecord]) -> i64 {
    closed
        .iter()
        .filter(|alert| {
            alert.kind == AlertKind::ChainBroken
                && alert.tenant_id == Some(tenant)
                && alert.dedup_key == store
        })
        .filter_map(|alert| {
            alert
                .detail
                .get(WATERMARK)
                .and_then(Value::as_i64)
                .or_else(|| {
                    alert
                        .resolved_at
                        .map(Timestamp::as_milliseconds_since_epoch)
                })
        })
        .max()
        .unwrap_or(i64::MIN)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::chain::ChainHash;
    use pos_proto::ids::{EventId, StoreId, TenantId};
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use super::{WATERMARK, chain_broken};
    use crate::alerts::evaluator::reconcile;
    use crate::alerts::model::{AlertKind, AlertSeverity, FiringAlert};
    use crate::alerts::store::{AlertRecord, AlertStore, AlertStoreError, last_resolved};
    use crate::anchor::{AnchorConflict, ConflictReport};

    #[derive(Default)]
    struct FakeAlerts {
        rows: Mutex<Vec<AlertRecord>>,
    }

    impl AlertStore for FakeAlerts {
        async fn upsert(&self, record: &AlertRecord) -> Result<(), AlertStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            if let Some(open) = rows
                .iter_mut()
                .find(|r| r.resolved_at.is_none() && r.key() == record.key())
            {
                open.summary.clone_from(&record.summary);
                open.detail = record.detail.clone();
                open.last_seen_at = record.last_seen_at;
            } else {
                rows.push(record.clone());
            }
            Ok(())
        }

        async fn resolve(&self, id: &str, at: Timestamp) -> Result<(), AlertStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            if let Some(row) = rows
                .iter_mut()
                .find(|r| r.id == id && r.resolved_at.is_none())
            {
                row.resolved_at = Some(at);
            }
            Ok(())
        }

        async fn acknowledge(&self, id: &str, at: Timestamp) -> Result<(), AlertStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
                row.acknowledged_at = Some(at);
            }
            Ok(())
        }

        async fn list_active(&self) -> Result<Vec<AlertRecord>, AlertStoreError> {
            let rows = self.rows.lock().expect("lock");
            Ok(rows
                .iter()
                .filter(|r| r.resolved_at.is_none())
                .cloned()
                .collect())
        }

        async fn list_recent(&self, _limit: u32) -> Result<Vec<AlertRecord>, AlertStoreError> {
            Ok(self.rows.lock().expect("lock").clone())
        }

        async fn list_last_resolved(
            &self,
            kind: AlertKind,
        ) -> Result<Vec<AlertRecord>, AlertStoreError> {
            Ok(last_resolved(&self.rows.lock().expect("lock"), kind))
        }
    }

    fn ts(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid timestamp")
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn store(n: u128) -> StoreId {
        StoreId::new(Ulid::from_u128(0x5700 + n))
    }

    fn finding(store_n: u128, seq: u64, noticed_ms: i64) -> AnchorConflict {
        AnchorConflict {
            conflict_id: format!("c-{store_n}-{seq}"),
            noticed_at: ts(noticed_ms),
            report: ConflictReport {
                store: store(store_n),
                chain_seq: seq,
                held_head: ChainHash::from_hex(&"a".repeat(64)),
                offered_head: ChainHash::from_hex(&"b".repeat(64)),
                offered_event_id: EventId::new(Ulid::from_u128(u128::from(seq))),
            },
        }
    }

    /// One evaluator pass for the chain kind alone: what the ledger holds, against what the store
    /// has closed, reconciled.
    async fn pass(store: &FakeAlerts, now: i64, ledger: &[AnchorConflict]) -> Vec<FiringAlert> {
        let closed = store
            .list_last_resolved(AlertKind::ChainBroken)
            .await
            .expect("closed");
        let firing = chain_broken(tenant(), ledger, &closed);
        let mut n = 0_u32;
        let (opened, _) = reconcile(store, ts(now), &firing, || {
            n += 1;
            format!("alert-{now}-{n}")
        })
        .await
        .expect("reconcile");
        opened
    }

    #[test]
    fn findings_fold_into_one_critical_alert_per_store_with_the_seq_range() {
        let firing = chain_broken(
            tenant(),
            &[
                finding(1, 9, 1_000),
                finding(1, 4, 2_000),
                finding(2, 7, 1_500),
            ],
            &[],
        );
        assert_eq!(firing.len(), 2, "two stores, two alerts: {firing:?}");
        let first = &firing[0];
        assert_eq!(first.kind, AlertKind::ChainBroken);
        assert_eq!(first.severity, AlertSeverity::Critical);
        assert_eq!(first.tenant_id, Some(tenant()));
        assert_eq!(
            first.dedup_key,
            store(1).to_string(),
            "deduplicated by store"
        );
        assert_eq!(first.detail["from_seq"], 4);
        assert_eq!(first.detail["to_seq"], 9);
        assert_eq!(first.detail["findings"], 2);
        assert_eq!(first.detail[WATERMARK], 2_000);
        assert!(first.summary.contains("4\u{2013}9"), "{}", first.summary);
    }

    #[test]
    fn no_findings_raise_nothing() {
        // The ledger files only accusations; a gap or an unverified window never reaches it, so a
        // store with nothing filed has nothing to alert on (the whole path is covered in
        // `tests/cloud/chain.rs`).
        assert!(chain_broken(tenant(), &[], &[]).is_empty());
    }

    #[tokio::test]
    async fn a_chain_break_stays_open_until_a_person_closes_it_and_only_new_findings_reopen() {
        let alerts = FakeAlerts::default();
        let ledger = vec![finding(1, 4, 1_000)];

        let opened = pass(&alerts, 10_000, &ledger).await;
        assert_eq!(opened.len(), 1, "the finding opens one alert");

        // Re-evaluated with no new finding — and with the finding even gone from what the pass
        // read, as when the ledger could not be reached — it stays open: a chain does not heal.
        assert!(pass(&alerts, 20_000, &ledger).await.is_empty());
        assert!(pass(&alerts, 30_000, &[]).await.is_empty());
        let active = alerts.list_active().await.expect("active");
        assert_eq!(active.len(), 1, "still open: {active:?}");
        assert_eq!(active[0].first_seen_at, ts(10_000));

        // A person closes it (the console's acknowledge resolves this kind).
        alerts
            .resolve(&active[0].id, ts(35_000))
            .await
            .expect("resolve");

        // The same findings do not reopen it.
        assert!(pass(&alerts, 40_000, &ledger).await.is_empty());
        assert!(alerts.list_active().await.expect("active").is_empty());

        // A finding the cloud noticed after the one that person saw opens a new alert.
        let later = vec![finding(1, 12, 36_000), finding(1, 4, 1_000)];
        let opened = pass(&alerts, 50_000, &later).await;
        assert_eq!(opened.len(), 1, "a new finding is a new alert");
        assert_eq!(
            opened[0].detail["from_seq"], 12,
            "covering only what is new"
        );
        assert_eq!(alerts.rows.lock().expect("lock").len(), 2, "history kept");
    }
}
