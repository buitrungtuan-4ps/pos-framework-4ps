// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud recomputes a store's chain from the events it holds
//! ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)).
//!
//! # Why the anchor was not enough
//!
//! [`crate::anchor`] refuses a second, different head at a chain length the cloud already holds.
//! That closes **recomputation**. It does not close **truncation**: a store cut back to length 20
//! and then traded on publishes its next anchor *above* anything the cloud holds, and nothing
//! collides. From the anchors alone it is honest growth.
//!
//! The evidence was never in the anchors. It is in the **events** — the cloud holds the original
//! records at seqs 21..61 and then receives new, different ones claiming those same seqs. Two
//! events at one position in one store's chain is the fork, in the open, in data the cloud already
//! has and was not reading.
//!
//! # What this module is, and what it is not
//!
//! It is pure. It takes the events the cloud holds for a window and the anchor that closes it, and
//! says what it found. It does no I/O, holds no clock and reaches no database — the reading is
//! [`crate::anchor::ChainWindow`]'s and the recording is the ledger's, so the rule the whole
//! mechanism rests on can be read and tested on its own.
//!
//! # Two of the four findings are accusations, and two are not
//!
//! Getting that boundary wrong is the failure that matters here. [`ChainFinding::Incomplete`] is a
//! missing `seq`, and ingest is at-least-once: a broker gap is ordinary and
//! [ADR-0040](../../../docs/adr/0040-reconciliation.md) exists to fill exactly this. Reporting it
//! as tampering would be an accusation manufactured by the recovery mechanism working correctly.
//! [`ChainFinding::Unverified`] is an honest absence of an answer — never a pass.

use std::collections::BTreeMap;

use pos_proto::chain::{ChainHash, ChainLink};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::EventId;
use sha2::{Digest as _, Sha256};

/// How many records one audit will walk.
///
/// A shift is thousands of events, not millions. A window beyond this means something is wrong with
/// the claim, not with the cap ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)
/// decision 4), so it is reported as [`Unverified::WindowTooLarge`] rather than silently trimmed.
pub const MAX_WINDOW: u64 = 50_000;

/// How many findings one audit carries back.
///
/// A truncated-and-re-traded shift produces one finding per re-used position, which is a whole
/// shift's worth. The list is capped and the total is reported beside it, because a reader needs to
/// know there were nine hundred and not that there were thirty-two.
pub const MAX_FINDINGS: usize = 32;

/// Why an audit could not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unverified {
    /// The window between the last anchor and this one exceeds [`MAX_WINDOW`].
    WindowTooLarge,
    /// The cloud holds no chained events in the window — a store whose chain predates the migration,
    /// or one whose events have not arrived. Distinct from a clean walk over zero records.
    NothingChained,
}

/// What recomputing a store's chain turned up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainFinding {
    /// Two events claim one position in the chain. **This is the truncation case.**
    ///
    /// A store cannot have two different records at one length of one log. The pair is kept because
    /// the pair is the finding.
    Forked {
        /// The contested position.
        seq: u64,
        /// The event the cloud saw there first, in `event_id` order.
        first: EventId,
        /// The one that claims the same place.
        second: EventId,
        /// What the first record hashes to.
        ///
        /// Carried because it is what makes a fork *recordable*: two different records at position
        /// `seq` are two different answers to "what does this chain hash to at `seq`", which is
        /// exactly the pair of contradicting heads [`crate::anchor::ConflictReport`] holds. Without
        /// them the finding could only be written down as prose.
        first_hash: ChainHash,
        /// What the second record hashes to.
        second_hash: ChainHash,
    },
    /// A record's `prev_hash` is not the hash of the record before it.
    ///
    /// The cloud re-derives the chain from its own copies rather than trusting that the store did.
    BrokenLink {
        /// The record whose link does not hold.
        seq: u64,
        /// What that record says the one before it hashed to.
        declared: ChainHash,
        /// What the cloud computes from the record it holds there.
        computed: ChainHash,
    },
    /// A position in the window is missing. **Not a fault** — see the module note.
    Incomplete {
        /// The lowest missing position.
        from_seq: u64,
        /// How many positions in the window the cloud does not hold.
        missing: u64,
    },
    /// The audit could not answer.
    Unverified(Unverified),
}

impl ChainFinding {
    /// Whether this finding says a store's log disagrees with itself.
    ///
    /// The two that are not accusations — a gap the cloud can re-push for, and an answer it could
    /// not reach — are exactly as important to surface and must never be filed as tampering.
    #[must_use]
    pub fn is_tampering(&self) -> bool {
        matches!(self, Self::Forked { .. } | Self::BrokenLink { .. })
    }
}

/// What an anchor's own envelope settles about the payload it carries — which is less than it
/// first appears ([ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)
/// decision 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeCheck {
    /// Consistent, as far as one record can settle it.
    Consistent,
    /// The payload claims a chain at least as long as the record carrying it. Impossible: the
    /// anchor is written *into* the chain it describes, so it always sits above it.
    ImpossibleLength {
        /// Where the anchor record itself sits.
        envelope_seq: u64,
        /// The length it claims to describe.
        claimed_seq: u64,
    },
    /// The anchor sits immediately after the length it names, so its `prev_hash` is the head it
    /// reports — and is not.
    HeadMismatch {
        /// The head the envelope links to.
        linked: ChainHash,
        /// The head the payload reports.
        claimed: ChainHash,
    },
    /// The anchor carries no chain link, so its envelope settles nothing. Not a fault: a store
    /// whose events predate the chain publishes unchained anchors.
    Unchained,
}

impl EnvelopeCheck {
    /// Whether this anchor may be trusted enough to become a store's head.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        matches!(self, Self::Consistent | Self::Unchained)
    }
}

/// Checks an anchor against its own envelope, before anything is read.
///
/// # Why this is an inequality and not an equality
///
/// The tempting check is `prev_hash == chain_head`: the anchor links to the head it reports. It is
/// **false under concurrency**. The edge reads its head and *then* commits the anchor — a record
/// cannot contain its own hash — so a sale landing between the two leaves the anchor several
/// records above the length it names, linking to a record the payload never mentions.
///
/// What holds always is the inequality: an anchor claiming a chain at least as long as the record
/// carrying it is impossible. The hash comparison is made only when the two are adjacent, where it
/// is exact. When they are not, the head is checked against the window instead — where it is
/// checked anyway.
#[must_use]
pub fn check_envelope(
    link: Option<&ChainLink>,
    claimed_seq: u64,
    claimed_head: &ChainHash,
) -> EnvelopeCheck {
    let Some(link) = link else {
        return EnvelopeCheck::Unchained;
    };
    if link.seq <= claimed_seq {
        return EnvelopeCheck::ImpossibleLength {
            envelope_seq: link.seq,
            claimed_seq,
        };
    }
    if link.seq == claimed_seq.saturating_add(1) && link.prev_hash != *claimed_head {
        return EnvelopeCheck::HeadMismatch {
            linked: link.prev_hash.clone(),
            claimed: claimed_head.clone(),
        };
    }
    EnvelopeCheck::Consistent
}

/// One recomputation of a store's chain over the window an anchor closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainAudit {
    /// The first position the window covers.
    pub from_seq: u64,
    /// The last, which is the length the anchor claims.
    pub to_seq: u64,
    /// How many positions the cloud actually held and walked.
    pub checked: u64,
    /// What it found, capped at [`MAX_FINDINGS`].
    pub findings: Vec<ChainFinding>,
    /// How many findings there were in total, which may exceed the list above.
    pub found: u64,
}

impl ChainAudit {
    /// Whether anything found says the store's log disagrees with itself.
    #[must_use]
    pub fn found_tampering(&self) -> bool {
        self.findings.iter().any(ChainFinding::is_tampering)
    }

    /// An audit that reached no answer, with the reason.
    fn unverified(from_seq: u64, to_seq: u64, why: Unverified) -> Self {
        Self {
            from_seq,
            to_seq,
            checked: 0,
            findings: vec![ChainFinding::Unverified(why)],
            found: 1,
        }
    }
}

/// The hash of one record as its own chain uses it.
///
/// The same three lines `store-sqlite`'s writer runs, and deliberately a second copy rather than a
/// shared helper: `pos-proto` owns the preimage but cannot own the digest (it is a backbone crate
/// and `sha2` is not allow-listed there, see [`pos_proto::chain`]), and two occurrences is one short
/// of the third that `docs/design-principles.md` says to extract on. What must not drift is the
/// *preimage*, and that has one definition.
fn record_hash(event: &EventEnvelope<RawPayload>, link: &ChainLink) -> Option<ChainHash> {
    event
        .chain_hash(link, |bytes| Sha256::digest(bytes).into())
        .ok()
}

/// The findings an audit accumulates, capped without losing the count.
///
/// A struct rather than a closure over two locals because the walk hands it between helpers, and a
/// cap that silently became the total is exactly the lie this type exists to prevent.
#[derive(Debug, Default)]
struct Findings {
    kept: Vec<ChainFinding>,
    total: u64,
}

impl Findings {
    fn push(&mut self, finding: ChainFinding) {
        self.total = self.total.saturating_add(1);
        if self.kept.len() < MAX_FINDINGS {
            self.kept.push(finding);
        }
    }
}

/// Indexes the window by chain position, reporting every position two different events claim.
///
/// The second claimant is the fork — the whole reason this module exists. The pair is kept in
/// `event_id` order so one fork reads the same way whatever order ingest delivered it in, and the
/// lower id wins the slot so the link walk below is deterministic rather than delivery-ordered.
fn index_by_position<'a>(
    window: &'a [EventEnvelope<RawPayload>],
    from_seq: u64,
    to_seq: u64,
    findings: &mut Findings,
) -> BTreeMap<u64, &'a EventEnvelope<RawPayload>> {
    use std::collections::btree_map::Entry;

    let mut at: BTreeMap<u64, &EventEnvelope<RawPayload>> = BTreeMap::new();
    for event in window {
        let Some(link) = event.chain.as_ref() else {
            continue;
        };
        if link.seq < from_seq || link.seq > to_seq {
            continue;
        }
        match at.entry(link.seq) {
            Entry::Vacant(slot) => {
                slot.insert(event);
            }
            Entry::Occupied(mut slot) => {
                let held: &EventEnvelope<RawPayload> = slot.get();
                if held.event_id == event.event_id {
                    // The same event twice: overlapping pages from the caller, or an at-least-once
                    // redelivery. Not a fork.
                    continue;
                }
                let (first, second) = if held.event_id < event.event_id {
                    (held, event)
                } else {
                    (event, held)
                };
                // A record that will not hash is reported with a genesis placeholder rather than
                // dropped: the fork is the finding, and losing it to a serialisation failure would
                // hand a store a way to hide one.
                let hash_of = |record: &EventEnvelope<RawPayload>| {
                    record
                        .chain
                        .as_ref()
                        .and_then(|its_link| record_hash(record, its_link))
                        .unwrap_or_else(ChainHash::genesis)
                };
                findings.push(ChainFinding::Forked {
                    seq: link.seq,
                    first: first.event_id,
                    second: second.event_id,
                    first_hash: hash_of(first),
                    second_hash: hash_of(second),
                });
                if event.event_id < held.event_id {
                    slot.insert(event);
                }
            }
        }
    }
    at
}

/// Reports the positions the cloud does not hold. **Not a fault** — see the module note.
fn note_gaps(
    at: &BTreeMap<u64, &EventEnvelope<RawPayload>>,
    from_seq: u64,
    to_seq: u64,
    findings: &mut Findings,
) {
    let expected = to_seq.saturating_sub(from_seq).saturating_add(1);
    let held = at.len() as u64;
    if held >= expected {
        return;
    }
    let lowest_missing = (from_seq..=to_seq)
        .find(|seq| !at.contains_key(seq))
        .unwrap_or(from_seq);
    findings.push(ChainFinding::Incomplete {
        from_seq: lowest_missing,
        missing: expected.saturating_sub(held),
    });
}

/// Re-derives each record's link from the record the cloud holds before it.
///
/// A position whose predecessor is absent is skipped rather than reported: the gap is already a
/// finding, and calling it a broken link as well would turn one broker hiccup into two alarms. The
/// window's own first position is skipped for the same reason — what it links to sits below the
/// window, so there is nothing here to compare it with.
fn walk_links(at: &BTreeMap<u64, &EventEnvelope<RawPayload>>, findings: &mut Findings) {
    for (seq, event) in at {
        let Some(link) = event.chain.as_ref() else {
            continue;
        };
        let Some(previous) = seq.checked_sub(1).and_then(|before| at.get(&before)) else {
            continue;
        };
        let Some(previous_link) = previous.chain.as_ref() else {
            continue;
        };
        let Some(computed) = record_hash(previous, previous_link) else {
            continue;
        };
        if computed != link.prev_hash {
            findings.push(ChainFinding::BrokenLink {
                seq: *seq,
                declared: link.prev_hash.clone(),
                computed,
            });
        }
    }
}

/// Checks the head the anchor claims against the record the cloud holds at that position.
fn check_head(
    at: &BTreeMap<u64, &EventEnvelope<RawPayload>>,
    to_seq: u64,
    claimed_head: &ChainHash,
    findings: &mut Findings,
) {
    let Some(head_event) = at.get(&to_seq) else {
        return;
    };
    let Some(link) = head_event.chain.as_ref() else {
        return;
    };
    let Some(computed) = record_hash(head_event, link) else {
        return;
    };
    if computed != *claimed_head {
        findings.push(ChainFinding::BrokenLink {
            seq: to_seq,
            declared: claimed_head.clone(),
            computed,
        });
    }
}

/// Recomputes the chain the events in `window` make, and compares it to what the store claims.
///
/// `window` is every event the cloud holds that might fall in `[from_seq, to_seq]`; events outside
/// it and events carrying no chain link are ignored, so a caller may hand over a whole trading day.
///
/// # What it checks, in order
///
/// 1. **One event per position.** Two is the fork, and the reason this exists.
/// 2. **No gaps.** Reported, never accused — see the module note.
/// 3. **Each link.** A record's `prev_hash` against the hash the cloud computes for the record
///    before it.
/// 4. **The head.** The record at `to_seq` must hash to what the anchor claims.
#[must_use]
pub fn audit_window(
    window: &[EventEnvelope<RawPayload>],
    from_seq: u64,
    to_seq: u64,
    claimed_head: &ChainHash,
) -> ChainAudit {
    if to_seq < from_seq {
        return ChainAudit::unverified(from_seq, to_seq, Unverified::NothingChained);
    }
    if to_seq.saturating_sub(from_seq).saturating_add(1) > MAX_WINDOW {
        return ChainAudit::unverified(from_seq, to_seq, Unverified::WindowTooLarge);
    }

    let mut findings = Findings::default();
    let at = index_by_position(window, from_seq, to_seq, &mut findings);
    if at.is_empty() {
        return ChainAudit::unverified(from_seq, to_seq, Unverified::NothingChained);
    }
    note_gaps(&at, from_seq, to_seq, &mut findings);
    walk_links(&at, &mut findings);
    check_head(&at, to_seq, claimed_head, &mut findings);

    ChainAudit {
        from_seq,
        to_seq,
        checked: at.len() as u64,
        findings: findings.kept,
        found: findings.total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_proto::Ulid;
    use pos_proto::events::{DeviceActivationCompleted, EventType};
    use pos_proto::ids::{BrandId, DeviceId, StoreId, TenantId};
    use pos_proto::{BusinessDate, Timestamp};

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(7))
    }

    /// An event with `event_id(seed)`, carrying no chain link yet.
    fn unchained(seed: u128) -> EventEnvelope<RawPayload> {
        EventEnvelope {
            event_id: EventId::new(Ulid::from_u128(seed)),
            event_type: EventType::DeviceActivationCompleted.into(),
            event_time: Timestamp::from_milliseconds_since_epoch(1_700_000_000_000)
                .expect("a fixed instant inside the representable range"),
            business_date: BusinessDate::from_ymd(2026, 1, 1).expect("2026-01-01 is a valid date"),
            schema_version: 1,
            tenant_id: TenantId::new(Ulid::from_u128(1)),
            brand_id: BrandId::new(Ulid::from_u128(1)),
            store_id: store(),
            device_id: DeviceId::new(Ulid::from_u128(1)),
            employee_id: None,
            shift_id: None,
            chain: None,
            data: RawPayload::encode(&DeviceActivationCompleted {
                activated_device_id: DeviceId::new(Ulid::from_u128(seed)),
            })
            .expect("a fixture payload serialises"),
        }
    }

    /// A correctly chained run of `count` records starting at `seq` 1, exactly as an edge writes it.
    ///
    /// Built by actually hashing each record rather than by stamping made-up values: a fixture whose
    /// links do not really hold would make every "this is sound" assertion below vacuous.
    fn honest_chain(count: u64) -> Vec<EventEnvelope<RawPayload>> {
        let mut chain = Vec::new();
        let mut link = ChainLink::first();
        for seed in 1..=count {
            let mut event = unchained(u128::from(seed));
            let hash = record_hash(&event, &link).expect("a fixture record hashes");
            event.chain = Some(link.clone());
            link = ChainLink::following(link.seq, hash);
            chain.push(event);
        }
        chain
    }

    /// The head of a correctly chained run — the hash of its last record.
    fn head_of(chain: &[EventEnvelope<RawPayload>]) -> ChainHash {
        let last = chain.last().expect("a non-empty chain");
        let link = last.chain.as_ref().expect("a chained record");
        record_hash(last, link).expect("the head hashes")
    }

    // --- the envelope check ----------------------------------------------------------------------

    #[test]
    fn an_anchor_one_above_the_length_it_names_is_consistent() {
        let chain = honest_chain(3);
        let head = head_of(&chain);
        // The anchor is the next record: seq 4, linking to the hash of record 3, reporting length 3.
        let link = ChainLink::following(3, head.clone());
        assert_eq!(
            check_envelope(Some(&link), 3, &head),
            EnvelopeCheck::Consistent
        );
    }

    #[test]
    fn a_sale_landing_between_the_read_and_the_commit_is_not_a_fault() {
        // The edge reads its head and *then* commits the anchor, so a busy till leaves the anchor
        // several records above the length it names. The tempting `prev_hash == chain_head` check
        // would call this tampering; it is an ordinary Friday night.
        let chain = honest_chain(5);
        let head_at_three = {
            let third = &chain[2];
            record_hash(third, third.chain.as_ref().expect("chained")).expect("hashes")
        };
        let link = ChainLink::following(5, head_of(&chain));
        assert_eq!(
            check_envelope(Some(&link), 3, &head_at_three),
            EnvelopeCheck::Consistent,
            "an anchor reporting an older length is normal, not a contradiction"
        );
    }

    #[test]
    fn an_anchor_claiming_a_chain_as_long_as_its_own_record_is_impossible() {
        // The anchor is written *into* the chain it describes, so it always sits above it.
        let head = ChainHash::from_hex("ab");
        let link = ChainLink::following(41, head.clone());
        assert_eq!(
            check_envelope(Some(&link), 42, &head),
            EnvelopeCheck::ImpossibleLength {
                envelope_seq: 42,
                claimed_seq: 42,
            }
        );
        assert!(!check_envelope(Some(&link), 42, &head).is_consistent());
    }

    #[test]
    fn an_adjacent_anchor_that_links_elsewhere_is_caught_on_one_record() {
        let link = ChainLink::following(41, ChainHash::from_hex("aa"));
        let verdict = check_envelope(Some(&link), 41, &ChainHash::from_hex("ff"));
        assert_eq!(
            verdict,
            EnvelopeCheck::HeadMismatch {
                linked: ChainHash::from_hex("aa"),
                claimed: ChainHash::from_hex("ff"),
            }
        );
        assert!(!verdict.is_consistent());
    }

    #[test]
    fn an_unchained_anchor_settles_nothing_and_is_not_a_fault() {
        let verdict = check_envelope(None, 42, &ChainHash::from_hex("ab"));
        assert_eq!(verdict, EnvelopeCheck::Unchained);
        assert!(
            verdict.is_consistent(),
            "a store whose events predate the chain is not accused of anything"
        );
    }

    // --- the window ------------------------------------------------------------------------------

    #[test]
    fn an_honest_chain_walks_clean() {
        let chain = honest_chain(6);
        let audit = audit_window(&chain, 1, 6, &head_of(&chain));
        assert_eq!(audit.findings, Vec::new(), "nothing is wrong with this log");
        assert_eq!(audit.checked, 6);
        assert_eq!(audit.found, 0);
        assert!(!audit.found_tampering());
    }

    #[test]
    fn two_events_at_one_position_is_a_fork() {
        // The truncation case: the store cut its log back and traded on, so the cloud now holds the
        // original record at seq 4 and a different one claiming the same place.
        let mut chain = honest_chain(6);
        let mut impostor = unchained(0xF00D);
        impostor.chain = chain[3].chain.clone();
        chain.push(impostor);

        let audit = audit_window(&chain, 1, 6, &head_of(&chain[..6]));
        let forks: Vec<&ChainFinding> = audit
            .findings
            .iter()
            .filter(|finding| matches!(finding, ChainFinding::Forked { .. }))
            .collect();
        assert_eq!(forks.len(), 1, "one contested position, one finding");
        let ChainFinding::Forked {
            seq,
            first,
            second,
            first_hash,
            second_hash,
        } = forks[0]
        else {
            unreachable!("filtered to forks above")
        };
        assert_eq!(*seq, 4);
        assert_eq!(*first, EventId::new(Ulid::from_u128(4)));
        assert_eq!(*second, EventId::new(Ulid::from_u128(0xF00D)));
        assert_ne!(
            first_hash, second_hash,
            "two records at one position hash to two different answers — that pair is the finding"
        );
        assert!(audit.found_tampering());
    }

    #[test]
    fn the_same_event_delivered_twice_is_not_a_fork() {
        // Overlapping pages from the caller, or an at-least-once redelivery. Accusing here would
        // turn the delivery guarantee into an alarm.
        let chain = honest_chain(4);
        let mut doubled = chain.clone();
        doubled.push(chain[2].clone());
        let audit = audit_window(&doubled, 1, 4, &head_of(&chain));
        assert_eq!(audit.findings, Vec::new());
    }

    #[test]
    fn an_edited_record_breaks_the_link_that_follows_it() {
        // The cloud re-derives the chain from its own copies rather than trusting that the store
        // did. Edit record 3 and record 4's `prev_hash` no longer matches what 3 now hashes to.
        let mut chain = honest_chain(5);
        chain[2].data = RawPayload::encode(&DeviceActivationCompleted {
            activated_device_id: DeviceId::new(Ulid::from_u128(0xDEAD)),
        })
        .expect("a fixture payload serialises");

        let audit = audit_window(&chain, 1, 5, &head_of(&chain));
        let broken: Vec<u64> = audit
            .findings
            .iter()
            .filter_map(|finding| match finding {
                ChainFinding::BrokenLink { seq, .. } => Some(*seq),
                _ => None,
            })
            .collect();
        assert!(
            broken.contains(&4),
            "record 4 links to what record 3 used to be; found {broken:?}"
        );
        assert!(audit.found_tampering());
    }

    #[test]
    fn a_head_the_records_do_not_produce_is_caught() {
        let chain = honest_chain(4);
        let audit = audit_window(&chain, 1, 4, &ChainHash::from_hex("ff"));
        assert!(
            audit
                .findings
                .iter()
                .any(|finding| matches!(finding, ChainFinding::BrokenLink { seq: 4, .. })),
            "the anchor claims a head the records it describes do not produce"
        );
    }

    #[test]
    fn a_missing_record_is_reported_and_never_accused() {
        // A broker gap. ADR-0040 exists to fill exactly this, and calling it tampering would be an
        // accusation manufactured by the recovery mechanism working correctly.
        let chain = honest_chain(6);
        let mut with_a_gap = chain.clone();
        with_a_gap.remove(2);

        let audit = audit_window(&with_a_gap, 1, 6, &head_of(&chain));
        let gaps: Vec<&ChainFinding> = audit
            .findings
            .iter()
            .filter(|finding| matches!(finding, ChainFinding::Incomplete { .. }))
            .collect();
        assert_eq!(
            gaps,
            vec![&ChainFinding::Incomplete {
                from_seq: 3,
                missing: 1
            }]
        );
        assert!(
            !audit.found_tampering(),
            "a gap is a prompt to reconcile, never an accusation"
        );
    }

    #[test]
    fn a_gap_does_not_also_report_the_link_across_it_as_broken() {
        // One broker hiccup must be one finding. Record 4's predecessor is absent, so its link has
        // nothing here to be compared with and is skipped rather than blamed.
        let chain = honest_chain(6);
        let mut with_a_gap = chain.clone();
        with_a_gap.remove(2);
        let audit = audit_window(&with_a_gap, 1, 6, &head_of(&chain));
        assert_eq!(
            audit.findings.len(),
            1,
            "one gap, one finding: {:?}",
            audit.findings
        );
    }

    #[test]
    fn a_window_beyond_the_cap_is_unverified_rather_than_trimmed() {
        let audit = audit_window(&[], 1, MAX_WINDOW + 1, &ChainHash::genesis());
        assert_eq!(
            audit.findings,
            vec![ChainFinding::Unverified(Unverified::WindowTooLarge)]
        );
        assert!(
            !audit.found_tampering(),
            "an unread window is an absent answer, not a pass and not an accusation"
        );
    }

    #[test]
    fn a_window_the_cloud_holds_nothing_chained_for_is_unverified() {
        let audit = audit_window(&[unchained(1)], 1, 3, &ChainHash::genesis());
        assert_eq!(
            audit.findings,
            vec![ChainFinding::Unverified(Unverified::NothingChained)]
        );
        assert_eq!(audit.checked, 0, "nothing was walked, and it says so");
    }

    #[test]
    fn events_outside_the_window_are_ignored_so_a_caller_may_hand_over_a_whole_day() {
        let chain = honest_chain(8);
        // Ask only about 3..=6; the records either side are in the slice and must not be counted,
        // nor produce a gap finding.
        let head_at_six = {
            let sixth = &chain[5];
            record_hash(sixth, sixth.chain.as_ref().expect("chained")).expect("hashes")
        };
        let audit = audit_window(&chain, 3, 6, &head_at_six);
        assert_eq!(audit.checked, 4);
        assert_eq!(audit.findings, Vec::new());
    }

    #[test]
    fn the_findings_list_is_capped_and_the_total_is_not() {
        // A truncated-and-re-traded shift produces one finding per re-used position. A reader needs
        // to know there were many, not that there were exactly as many as the cap.
        let chain = honest_chain(MAX_FINDINGS as u64 + 10);
        let mut forked = chain.clone();
        for (index, event) in chain.iter().enumerate() {
            let mut impostor = unchained(0x1000 + index as u128);
            impostor.chain = event.chain.clone();
            forked.push(impostor);
        }
        let audit = audit_window(&forked, 1, MAX_FINDINGS as u64 + 10, &head_of(&chain));
        assert_eq!(audit.findings.len(), MAX_FINDINGS);
        assert!(
            audit.found > MAX_FINDINGS as u64,
            "the total is reported beside the capped list, not replaced by it"
        );
    }
}
