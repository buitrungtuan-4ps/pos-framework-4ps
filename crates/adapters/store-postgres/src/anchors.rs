// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The anchors a store publishes, and the contradictions among them
//! ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
//!
//! A store's hash chain is only as good as the record of it the store cannot reach. This adapter is
//! that record's storage: every chain head a store published, and every offered head the cloud
//! refused because it contradicted one already held. `pos-cloud` implements its `AnchorLedger` seam
//! over this type and owns the rule that decides between the two.
//!
//! # Where the refusal actually lives
//!
//! In the primary key. `chain_anchors` is keyed `(tenant_id, store_id, chain_seq)` and written with
//! `ON CONFLICT DO NOTHING`, so a store gets exactly one head per chain length and nothing — not
//! this adapter, not a caller above it — can replace it. The application layer decides what to
//! *report*; the schema decides what is *possible*, and the schema is the half that still holds
//! when the layer above has a bug.

use deadpool_postgres::Pool;

use pos_ports::PortError;

use crate::store::{pool_unavailable, unavailable};

/// The anchor ledger over a shared pool. Built by
/// [`PostgresStore::chain_anchors`](crate::PostgresStore::chain_anchors).
#[derive(Clone, Debug)]
pub struct PostgresAnchors {
    pool: Pool,
}

impl PostgresAnchors {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The store's highest-`chain_seq` anchor, and the one held at exactly `chain_seq`, in that
    /// order. Either may be absent.
    ///
    /// Both in one round trip, against one snapshot: two separate queries could straddle a
    /// concurrent insert and produce a head and a same-length row that never coexisted, which is how
    /// a store gets falsely accused.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn held_for(
        &self,
        tenant_id: &str,
        store_id: &str,
        chain_seq: i64,
    ) -> Result<(Option<AnchorRow>, Option<AnchorRow>), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT chain_seq, chain_head, unchained, observed_at, \
                        chain_seq = $3 AS at_offered_length \
                 FROM chain_anchors \
                 WHERE tenant_id = $1 AND store_id = $2 \
                   AND (chain_seq = $3 \
                        OR chain_seq = (SELECT max(chain_seq) FROM chain_anchors \
                                        WHERE tenant_id = $1 AND store_id = $2))",
                &[&tenant_id, &store_id, &chain_seq],
            )
            .await
            .map_err(unavailable)?;
        let mut head = None;
        let mut at_offered_length = None;
        for row in &rows {
            let anchor = AnchorRow {
                chain_seq: row.get(0),
                chain_head: row.get(1),
                unchained: row.get(2),
                observed_at: row.get(3),
            };
            // One row can be both — the head *is* the offered length when a store re-sends its
            // latest anchor — so this is two independent assignments, not a match.
            if row.get::<_, bool>(4) {
                at_offered_length = Some(anchor.clone());
            }
            if head
                .as_ref()
                .is_none_or(|held: &AnchorRow| held.chain_seq < anchor.chain_seq)
            {
                head = Some(anchor);
            }
        }
        Ok((head, at_offered_length))
    }

    /// Records one anchor. Idempotent by `(tenant, store, chain_seq)`.
    ///
    /// `ON CONFLICT DO NOTHING` rather than an upsert, and that is the load-bearing word in this
    /// file: at-least-once ingest makes a re-delivery routine, and an upsert would turn the one
    /// write that must never happen — a second, different head at a length already recorded — into
    /// a silent success.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_anchor(
        &self,
        tenant_id: &str,
        store_id: &str,
        anchor: &AnchorRow,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO chain_anchors \
                     (tenant_id, store_id, chain_seq, chain_head, unchained, observed_at) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (tenant_id, store_id, chain_seq) DO NOTHING",
                &[
                    &tenant_id,
                    &store_id,
                    &anchor.chain_seq,
                    &anchor.chain_head,
                    &anchor.unchained,
                    &anchor.observed_at,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Records one refused anchor. `noticed_at` is the server's clock, by the column default.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn record_conflict(
        &self,
        conflict_id: &str,
        tenant_id: &str,
        store_id: &str,
        conflict: &ConflictRow,
    ) -> Result<(), PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        connection
            .execute(
                "INSERT INTO chain_anchor_conflicts \
                     (conflict_id, tenant_id, store_id, chain_seq, held_head, offered_head, \
                      offered_event_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (conflict_id) DO NOTHING",
                &[
                    &conflict_id,
                    &tenant_id,
                    &store_id,
                    &conflict.chain_seq,
                    &conflict.held_head,
                    &conflict.offered_head,
                    &conflict.offered_event_id,
                ],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    /// Every event the cloud holds for one store over `days_back + 1` trading days ending at
    /// `business_date`, in `event_id` order, capped at `limit` — the window a chain recomputation
    /// walks ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)).
    ///
    /// The day is the filter, against `events_tenant_store_date`
    /// ([ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)), and it is the partition
    /// key too, so this is a partition slice rather than a scan. Chain positions are read out of the
    /// envelopes by the caller: nothing here touches a JSON operator, and no column was promoted
    /// onto the most-written table in the system to make this query tidier.
    ///
    /// `days_back` exists because a shift that crosses the store's own day cut-off puts part of the
    /// window on the previous trading date. The arithmetic is PostgreSQL's — `pos-proto`'s
    /// `BusinessDate` deliberately offers none, and inventing calendar arithmetic in Rust to avoid
    /// one `interval` would be the worse trade.
    ///
    /// Returns the raw envelope text. Deserialising is `pos-cloud`'s, which is where the wire types
    /// live; this adapter's job is the rows.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn events_in_window(
        &self,
        tenant_id: &str,
        store_id: &str,
        business_date: &str,
        days_back: i32,
        limit: i64,
    ) -> Result<Vec<String>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let rows = connection
            .query(
                "SELECT envelope::text FROM events \
                 WHERE tenant_id = $1 AND store_id = $2 \
                   AND business_date <= to_date($3, 'YYYY-MM-DD') \
                   AND business_date >= to_date($3, 'YYYY-MM-DD') - make_interval(days => $4) \
                 ORDER BY event_id LIMIT $5",
                &[&tenant_id, &store_id, &business_date, &days_back, &limit],
            )
            .await
            .map_err(unavailable)?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// Lists a tenant's most recent refused anchors, newest first, capped at `limit`. An optional
    /// `store_id` narrows to one store.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_conflicts(
        &self,
        tenant_id: &str,
        store_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ListedConflict>, PortError> {
        let connection = self.pool.get().await.map_err(pool_unavailable)?;
        let columns = "conflict_id, store_id, chain_seq, held_head, offered_head, \
                       offered_event_id, (extract(epoch FROM noticed_at) * 1000)::bigint";
        let rows = match store_id {
            Some(store) => {
                connection
                    .query(
                        &format!(
                            "SELECT {columns} FROM chain_anchor_conflicts \
                             WHERE tenant_id = $1 AND store_id = $2 \
                             ORDER BY noticed_at DESC, conflict_id DESC LIMIT $3"
                        ),
                        &[&tenant_id, &store, &limit],
                    )
                    .await
            }
            None => {
                connection
                    .query(
                        &format!(
                            "SELECT {columns} FROM chain_anchor_conflicts \
                             WHERE tenant_id = $1 \
                             ORDER BY noticed_at DESC, conflict_id DESC LIMIT $2"
                        ),
                        &[&tenant_id, &limit],
                    )
                    .await
            }
        }
        .map_err(unavailable)?;
        Ok(rows
            .iter()
            .map(|row| ListedConflict {
                conflict_id: row.get(0),
                store_id: row.get(1),
                noticed_at: row.get(6),
                conflict: ConflictRow {
                    chain_seq: row.get(2),
                    held_head: row.get(3),
                    offered_head: row.get(4),
                    offered_event_id: row.get(5),
                },
            })
            .collect())
    }
}

/// One anchor, as this adapter writes and reads it. `pos-cloud` maps it onto its own `StoreAnchor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorRow {
    /// How many records the store's chain held.
    pub chain_seq: i64,
    /// The hash of the last of them.
    pub chain_head: String,
    /// How many records predate the chain and carry none.
    pub unchained: i64,
    /// The store's own timestamp on the anchor event, Unix ms. Its claim, not a finding.
    pub observed_at: i64,
}

/// One refused anchor: the pair of heads that cannot both be true, and the event that offered one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictRow {
    /// The chain length both heads claim to describe.
    pub chain_seq: i64,
    /// What the cloud held there.
    pub held_head: String,
    /// What it was offered instead.
    pub offered_head: String,
    /// The anchor event that carried the offered head (a ULID string).
    pub offered_event_id: String,
}

/// A refused anchor read back, with the cloud's own timestamp on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedConflict {
    /// The conflict's ULID string.
    pub conflict_id: String,
    /// The store that offered the contradicting anchor (a ULID string).
    pub store_id: String,
    /// Unix ms when the **cloud** saw the contradiction, from the server's clock.
    pub noticed_at: i64,
    /// The contradiction itself.
    pub conflict: ConflictRow,
}
