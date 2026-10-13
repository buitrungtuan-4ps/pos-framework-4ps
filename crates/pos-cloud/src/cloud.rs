// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud application spine: idempotent ingest, and the rollup read model.
//!
//! [`Cloud`] is generic over the store `S`, so the same ingest and rollup code runs against
//! `pos-fakes` in a test and `store-postgres` in the cloud — static dispatch, no `dyn`
//! ([ADR-0013](../../../docs/adr/0013-async-strategy.md), [ADR-0026](../../../docs/adr/0026-port-shapes.md)).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use std::sync::Arc;

use pos_ports::dynamic::BoxFuture;
use pos_ports::event_store::EventStore;
use pos_ports::{PortError, TxContext};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{BrandId, StoreId, TenantId};

use pos_proto::events::{EventType, StoreChainAnchored};

use crate::anchor::{AnchorLedger, AnchorVerdict, ChainWindow, ConflictReport, StoreAnchor, judge};
// The report types moved to pos-cloud-reports (ADR-0170 decision 4); re-exported so
// `pos_cloud::cloud::DailyRevenue` and the rest keep their paths.
use crate::chain_audit::{ChainFinding, EnvelopeCheck, MAX_WINDOW, audit_window, check_envelope};
pub use pos_cloud_reports::types::{
    AdmittedDevice, CashTotals, DailyCash, DailyRevenue, DailyRollup, FeeTotal, ItemMix,
    STORE_DRAWER, TaxComponentTotal, XzReport,
};

/// Who owns a store, as the registry records it
/// ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreOwner {
    /// The tenant that owns the store.
    pub tenant_id: TenantId,
    /// The brand it trades under, or the nil id when it is under none — the registry's `brand_id` is
    /// nullable, and a nil id reads as "no brand" rather than as a brand that might exist.
    pub brand_id: BrandId,
}

/// Resolves a store to its owner, for the stamp
/// [`Cloud::ingest`] puts on every event ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
///
/// `dyn`-compatible on purpose — boxed futures rather than `impl Future` — so [`Cloud`] can hold one
/// without gaining a type parameter that every existing construction site would have to name. The
/// same trade `pos_ports::dynamic` makes, for the same reason.
pub trait StoreOwners: Send + Sync + core::fmt::Debug {
    /// The store's owner, or `None` if the registry has no row for it.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the registry itself cannot be read — distinct from an unknown store, which is
    /// `Ok(None)`.
    fn owner_of(&self, store_id: StoreId) -> BoxFuture<'_, Result<Option<StoreOwner>, PortError>>;
}

/// How many trading days before an anchor's own the chain window reaches back.
///
/// One, because `business_date` is the store's trading day with its own cut-off
/// ([ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)) — a shift sits inside one date
/// unless it crossed that cut-off, and then it sits inside two. Reaching further would read days
/// no shift could have touched.
const WINDOW_DAYS_BACK: u8 = 1;

/// What one ingest achieved.
///
/// Idempotency is the point: re-delivering a batch the cloud already has adds `duplicates`, not
/// `appended`, and grows the log by nothing. At-least-once delivery from the edge
/// ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §4) guarantees the re-delivery happens, so a
/// caller that treats a duplicate as an error turns a healthy retry into a stuck feed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestOutcome {
    /// Events newly durable in the cloud.
    pub appended: u32,
    /// Events the cloud already had, discarded.
    pub duplicates: u32,
}

/// The cloud's application layer over an [`EventStore`].
///
/// Cloneable and shareable — every clone talks to the same store.
#[derive(Debug, Clone)]
pub struct Cloud<S> {
    store: S,
    /// The registry [`Self::ingest`] stamps each event's owner from
    /// ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    ///
    /// `None` leaves each envelope's claimed tenant and brand alone, which is what a fakes-backed
    /// test and the on-fakes example want — they have no registry to resolve against. **The shipped
    /// binary always sets one**: without it the event log's tenant column is whatever the box asserted,
    /// which is the hole S2 names.
    owners: Option<Arc<dyn StoreOwners>>,
    /// The ledger [`Self::ingest`] offers each `store.chain.anchored` event to
    /// ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
    ///
    /// `None` keeps no anchors, which is what a test with no ledger and the on-fakes example want.
    /// The shipped binary sets one: a chain nobody anchors catches careless editing and not fraud,
    /// which [`crate::anchor`] states at length.
    anchors: Option<Arc<dyn AnchorLedger>>,
    /// The log reader the chain recomputation walks
    /// ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)).
    ///
    /// `None` leaves an accepted anchor unverified against the events behind it — the head is still
    /// kept and still refused when it forks, but nothing recomputes the chain that produced it.
    chain_window: Option<Arc<dyn ChainWindow>>,
}

impl<S: EventStore> Cloud<S> {
    /// Builds the application over `store`, taking each event's tenant and brand as the envelope
    /// claims them.
    ///
    /// For a test and for the on-fakes example. The shipped cloud uses
    /// [`with_store_owners`](Self::with_store_owners) (ADR-0101).
    #[must_use]
    pub fn new(store: S) -> Self {
        Self {
            store,
            owners: None,
            anchors: None,
            chain_window: None,
        }
    }

    /// Builds the application over `store`, stamping each ingested event's tenant and brand from the
    /// store registry ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    ///
    /// This is the shipped composition. The tenant on an event is the column row-level isolation is
    /// defined on, and until this existed it was whatever the publishing box put there — one constant
    /// for the whole fleet, as it happened.
    #[must_use]
    pub fn with_store_owners(store: S, owners: Arc<dyn StoreOwners>) -> Self {
        Self {
            store,
            owners: Some(owners),
            anchors: None,
            chain_window: None,
        }
    }

    /// Adds the anchor ledger each ingested `store.chain.anchored` event is offered to
    /// ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
    ///
    /// A builder step rather than an argument to the two constructors above, because every caller
    /// that does not keep anchors — the fakes example, most tests — would otherwise have to name a
    /// `None` it has no opinion about.
    #[must_use]
    pub fn with_anchor_ledger(mut self, anchors: Arc<dyn AnchorLedger>) -> Self {
        self.anchors = Some(anchors);
        self
    }

    /// Adds the log reader the chain recomputation walks
    /// ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)).
    ///
    /// Separate from the ledger because they answer different questions and can be composed apart: a
    /// deployment that keeps anchors but cannot afford the window read still refuses a forked head.
    #[must_use]
    pub fn with_chain_window(mut self, window: Arc<dyn ChainWindow>) -> Self {
        self.chain_window = Some(window);
        self
    }

    /// The underlying store.
    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Ingests a batch, idempotently, in one transaction.
    ///
    /// The store's `append` is idempotent by `event_id` ([ADR-0026](../../../docs/adr/0026-port-shapes.md)),
    /// so a replay stores nothing and is reported as `duplicates`. The whole batch commits or none
    /// of it does.
    ///
    /// # The tenant is the registry's, not the envelope's
    ///
    /// Every event is stamped with the tenant and brand the store registry records for its `store_id`,
    /// overwriting what the envelope claimed ([ADR-0101](../../../docs/adr/0101-the-cloud-stamps-the-tenant.md)).
    /// This is the single funnel both ingest paths pass through — the NATS cursor and
    /// `POST /internal/ingest` — so it is the one place the stamp has to be. Idempotency is unaffected:
    /// the log's key is `(business_date, event_id)` and carries no tenant.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached or the transaction fails; nothing is committed.
    /// A registry that cannot be read is **not** an error — the batch is stored with the tenants it
    /// claimed and the failure is logged, because refusing would stall the fleet's ingest behind a
    /// lookup, and dropping would lose a real store's trading history.
    pub async fn ingest(
        &self,
        events: &[EventEnvelope<RawPayload>],
    ) -> Result<IngestOutcome, PortError> {
        if events.is_empty() {
            return Ok(IngestOutcome::default());
        }
        let stamped = self.stamp_owners(events).await;
        let mut tx = self.store.begin().await?;
        let outcome = self.store.append(&mut tx, &stamped).await?;
        tx.commit().await?;
        self.observe_anchors(&stamped).await;
        Ok(IngestOutcome {
            appended: outcome.appended,
            duplicates: outcome.duplicates,
        })
    }

    /// Offers every `store.chain.anchored` event in a committed batch to the anchor ledger
    /// ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
    ///
    /// # After the commit, and never able to fail it
    ///
    /// Two deliberate choices, and they are the same choice twice.
    ///
    /// **After**, because the event log is the source of truth and an anchor is an event like any
    /// other: a store that publishes a contradicting anchor has still published it, and the record
    /// of what it said is the evidence. Refusing the batch would delete the finding.
    ///
    /// **Never able to fail it**, because a ledger that cannot be written must not stop a fleet's
    /// trading history reaching the cloud — the same reasoning
    /// [`stamp_owners`](Self::stamp_owners) already applies to the registry, and the stronger case
    /// here: an outage that halted ingest is exactly the cover somebody tampering would want. A
    /// ledger failure is logged at `error` and the batch stands.
    ///
    /// What is refused is only the offered anchor's promotion to the store's head.
    async fn observe_anchors(&self, events: &[EventEnvelope<RawPayload>]) {
        let Some(ledger) = self.anchors.as_ref() else {
            return;
        };
        for event in events {
            // The declared token rather than a literal: a rename would move both together, and a
            // literal here would go on matching nothing in silence.
            if event.event_type.as_str() != EventType::StoreChainAnchored.as_str() {
                continue;
            }
            let Ok(published) = event.data.decode::<StoreChainAnchored>() else {
                // An event this system wrote that will not decode is a corrupt row, not a path.
                tracing::warn!(
                    store = %event.store_id,
                    event_id = %event.event_id,
                    "an anchor event did not decode; the chain head it carried was not recorded"
                );
                continue;
            };
            let offered = StoreAnchor {
                store: event.store_id,
                chain_seq: published.chain_seq,
                head: published.chain_head,
                unchained: published.unchained,
                observed_at: event.event_time,
            };
            self.weigh_anchor(ledger.as_ref(), event, &offered).await;
        }
    }

    /// Judges one offered anchor against what the ledger holds, and acts on the verdict.
    ///
    /// Split out so [`observe_anchors`](Self::observe_anchors) stays a loop over a batch and this
    /// stays the decision for one anchor.
    async fn weigh_anchor(
        &self,
        ledger: &dyn AnchorLedger,
        event: &EventEnvelope<RawPayload>,
        offered: &StoreAnchor,
    ) {
        // The anchor's own envelope first, because it costs nothing and settles the impossible
        // cases on one record (ADR-0132 decision 1). An anchor that fails this is not trusted enough
        // to become anybody's head.
        if !self.envelope_holds(ledger, event, offered).await {
            return;
        }
        let held = match ledger
            .held_for(event.tenant_id, offered.store, offered.chain_seq)
            .await
        {
            Ok(held) => held,
            Err(error) => {
                tracing::error!(
                    store = %offered.store,
                    %error,
                    "the anchor ledger could not be read; this store's chain head was not checked"
                );
                return;
            }
        };
        match judge(&held, offered) {
            AnchorVerdict::Forked { held: held_head } => {
                // Two irreconcilable claims about one log. The held record is what makes that
                // visible, so it is not overwritten; the offered one is recorded as a conflict.
                tracing::error!(
                    store = %offered.store,
                    chain_seq = offered.chain_seq,
                    held_head = %held_head,
                    offered_head = %offered.head,
                    event_id = %event.event_id,
                    "a store published a chain head that contradicts the one the cloud holds at                      that length; the anchor was refused and the contradiction recorded"
                );
                let report = ConflictReport {
                    store: offered.store,
                    chain_seq: offered.chain_seq,
                    held_head,
                    offered_head: offered.head.clone(),
                    offered_event_id: event.event_id,
                };
                if let Err(error) = ledger.record_conflict(event.tenant_id, &report).await {
                    tracing::error!(
                        store = %offered.store,
                        %error,
                        "recording a refused anchor failed; the refusal is in this log only"
                    );
                }
            }
            AnchorVerdict::Repeat => {}
            verdict @ (AnchorVerdict::Extends | AnchorVerdict::Backfills) => {
                if let Err(error) = ledger.accept(event.tenant_id, offered).await {
                    tracing::error!(
                        store = %offered.store,
                        chain_seq = offered.chain_seq,
                        %error,
                        "the anchor ledger could not be written; this chain head was not kept"
                    );
                    return;
                }
                tracing::info!(
                    store = %offered.store,
                    chain_seq = offered.chain_seq,
                    unchained = offered.unchained,
                    new_head = verdict.advances_the_head(),
                    "recorded a store's chain head"
                );
                // Only an anchor that moved the head is worth a window: a backfill describes
                // history already covered by the anchor above it, and re-walking it would spend a
                // read to re-derive a finding somebody has already been shown.
                if verdict.advances_the_head() {
                    self.recompute_chain(ledger, event, offered, held.head.as_ref())
                        .await;
                }
            }
        }
    }

    /// Checks an anchor against its own envelope, recording the one failure that has a durable shape.
    ///
    /// Returns whether the anchor may go on to be judged against the ledger.
    ///
    /// A `HeadMismatch` is two heads claiming to describe one chain length, which is exactly what a
    /// [`ConflictReport`] holds, so it is recorded as one. An `ImpossibleLength` has no second head
    /// to pair — it is a single malformed record — so it is refused and logged rather than filed
    /// under a shape it does not fit. Inventing a head to make the row look tidy would be worse
    /// than the gap; the durable record for that case arrives with the findings table.
    async fn envelope_holds(
        &self,
        ledger: &dyn AnchorLedger,
        event: &EventEnvelope<RawPayload>,
        offered: &StoreAnchor,
    ) -> bool {
        match check_envelope(event.chain.as_ref(), offered.chain_seq, &offered.head) {
            EnvelopeCheck::Consistent | EnvelopeCheck::Unchained => true,
            EnvelopeCheck::ImpossibleLength {
                envelope_seq,
                claimed_seq,
            } => {
                tracing::error!(
                    store = %offered.store,
                    event_id = %event.event_id,
                    envelope_seq,
                    claimed_seq,
                    "a store published an anchor claiming a chain at least as long as the record \
                     carrying it, which cannot happen; the anchor was refused"
                );
                false
            }
            EnvelopeCheck::HeadMismatch { linked, claimed } => {
                tracing::error!(
                    store = %offered.store,
                    event_id = %event.event_id,
                    chain_seq = offered.chain_seq,
                    linked_head = %linked,
                    claimed_head = %claimed,
                    "a store published an anchor that links to one head and reports another; the \
                     anchor was refused and the contradiction recorded"
                );
                let report = ConflictReport {
                    store: offered.store,
                    chain_seq: offered.chain_seq,
                    held_head: linked,
                    offered_head: claimed,
                    offered_event_id: event.event_id,
                };
                if let Err(error) = ledger.record_conflict(event.tenant_id, &report).await {
                    tracing::error!(
                        store = %offered.store,
                        %error,
                        "recording a self-contradicting anchor failed; the refusal is in this log only"
                    );
                }
                false
            }
        }
    }

    /// Recomputes the chain the cloud's own events make over the window this anchor closes
    /// ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)).
    ///
    /// The window runs from just past the previous anchor to the length this one claims. Its events
    /// are read by trading day — the index the log already carries — plus one day back, because a
    /// shift that crosses the store's own cut-off puts part of its window on the previous date.
    ///
    /// Like every other step here, a failure is reported and never fails the ingest.
    async fn recompute_chain(
        &self,
        ledger: &dyn AnchorLedger,
        event: &EventEnvelope<RawPayload>,
        offered: &StoreAnchor,
        previous: Option<&StoreAnchor>,
    ) {
        let Some(window) = self.chain_window.as_ref() else {
            return;
        };
        let from_seq = previous
            .map_or(1, |anchor| anchor.chain_seq.saturating_add(1))
            .min(offered.chain_seq);
        let limit = u32::try_from(MAX_WINDOW).unwrap_or(u32::MAX);
        let events = match window
            .events_in_window(
                event.tenant_id,
                offered.store,
                &event.business_date,
                WINDOW_DAYS_BACK,
                limit,
            )
            .await
        {
            Ok(events) => events,
            Err(error) => {
                tracing::error!(
                    store = %offered.store,
                    %error,
                    "the chain window could not be read; this anchor's records were not recomputed"
                );
                return;
            }
        };
        let audit = audit_window(&events, from_seq, offered.chain_seq, &offered.head);
        if audit.found == 0 {
            tracing::info!(
                store = %offered.store,
                from_seq = audit.from_seq,
                to_seq = audit.to_seq,
                checked = audit.checked,
                "recomputed a store's chain and it holds"
            );
            return;
        }
        for finding in &audit.findings {
            // Two of the four are accusations and two are not, and the level says which: a gap the
            // cloud can re-push for must not read like a store rewriting its books.
            if finding.is_tampering() {
                tracing::error!(
                    store = %offered.store,
                    anchor_seq = offered.chain_seq,
                    ?finding,
                    "recomputing a store's chain found its log disagreeing with itself"
                );
                self.record_finding(ledger, event, offered, finding).await;
            } else {
                tracing::warn!(
                    store = %offered.store,
                    anchor_seq = offered.chain_seq,
                    ?finding,
                    "recomputing a store's chain could not complete the walk"
                );
            }
        }
        if audit.found > audit.findings.len() as u64 {
            tracing::error!(
                store = %offered.store,
                found = audit.found,
                reported = audit.findings.len(),
                "more chain findings than this audit carries back; the log above is a sample"
            );
        }
    }

    /// Files one tampering finding as a conflict, so it outlives the process that noticed it.
    ///
    /// # Why a chain finding fits the anchor's conflict shape
    ///
    /// Both findings this records are the *same kind of thing* a forked anchor is: two
    /// irreconcilable answers to "what does this store's chain hash to at position N". A
    /// [`ChainFinding::BrokenLink`] is that pair directly. A [`ChainFinding::Forked`] is two
    /// records at one position, and two records hash to two values — which is that pair too. So
    /// this is one table because it is one question, not because a second one was inconvenient.
    ///
    /// The two findings that are **not** accusations are deliberately not filed. A gap the cloud
    /// can re-push for, and a window it could not read, are not contradictions and do not belong in
    /// a record of them.
    async fn record_finding(
        &self,
        ledger: &dyn AnchorLedger,
        event: &EventEnvelope<RawPayload>,
        offered: &StoreAnchor,
        finding: &ChainFinding,
    ) {
        let report = match finding {
            ChainFinding::Forked {
                seq,
                second,
                first_hash,
                second_hash,
                ..
            } => ConflictReport {
                store: offered.store,
                chain_seq: *seq,
                held_head: first_hash.clone(),
                offered_head: second_hash.clone(),
                // The later of the two records, which is the one a reader goes looking for first.
                offered_event_id: *second,
            },
            ChainFinding::BrokenLink {
                seq,
                declared,
                computed,
            } => ConflictReport {
                store: offered.store,
                chain_seq: *seq,
                held_head: computed.clone(),
                offered_head: declared.clone(),
                offered_event_id: event.event_id,
            },
            ChainFinding::Incomplete { .. } | ChainFinding::Unverified(_) => return,
        };
        if let Err(error) = ledger.record_conflict(event.tenant_id, &report).await {
            tracing::error!(
                store = %offered.store,
                %error,
                "recording a chain finding failed; the finding is in this log only"
            );
        }
    }

    /// Rewrites each event's tenant and brand to the registry's answer for its store (ADR-0101).
    ///
    /// One lookup per distinct store in the batch, not per event: a batch is one store's window far
    /// more often than not, and a fleet-wide batch is still bounded by the number of stores in it.
    ///
    /// Three things are deliberately not errors, and each keeps its own claim:
    /// - **No registry composed** — a test or the on-fakes example.
    /// - **The registry cannot be read** — refusing would stall every store's ingest behind one
    ///   unavailable lookup, and the log is the thing that must not stop.
    /// - **The store is unknown** — a provisioned store has a registry row by construction, so this
    ///   is a diagnostic rather than a path; losing a real store's history to a missing row would be
    ///   a provisioning bug turned into data loss.
    async fn stamp_owners(
        &self,
        events: &[EventEnvelope<RawPayload>],
    ) -> Vec<EventEnvelope<RawPayload>> {
        let mut stamped = events.to_vec();
        let Some(owners) = self.owners.as_ref() else {
            return stamped;
        };
        let mut resolved: BTreeMap<StoreId, Option<StoreOwner>> = BTreeMap::new();
        for event in &mut stamped {
            let owner = if let Some(owner) = resolved.get(&event.store_id) {
                *owner
            } else {
                let owner = match owners.owner_of(event.store_id).await {
                    Ok(owner) => owner,
                    Err(error) => {
                        tracing::warn!(
                            store = %event.store_id,
                            %error,
                            "could not resolve a store's owner; ingesting with the tenant the event claimed"
                        );
                        None
                    }
                };
                if owner.is_none() {
                    tracing::warn!(
                        store = %event.store_id,
                        "no registry row for the store that published this event; ingesting with the tenant it claimed"
                    );
                }
                resolved.insert(event.store_id, owner);
                owner
            };
            if let Some(owner) = owner {
                event.tenant_id = owner.tenant_id;
                event.brand_id = owner.brand_id;
            }
        }
        stamped
    }

    /// The per-day activity rollup for one store, oldest day first, computed from the log every call:
    /// [`daily_rollups_from_log`](crate::dashboard::from_log::daily_rollups_from_log) over this cloud's
    /// event store.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached.
    pub async fn daily_rollups(&self, store_id: StoreId) -> Result<Vec<DailyRollup>, PortError> {
        crate::dashboard::from_log::daily_rollups_from_log(&self.store, store_id).await
    }
}
