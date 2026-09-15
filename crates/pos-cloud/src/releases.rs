// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A release is one decision and many writes ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)).
//!
//! A Tết menu is not one node. It is a menu, the tax rates it prices against, the campaigns that
//! discount it, the reason codes the staff void it with, and the layout that shows it — five to nine
//! nodes, to forty stores, all switching on together. Before this module that was eighty separate
//! publishes with no name above them, no report across them, and no way to cancel the set (findings
//! **F8**, **F9**, **F10**).
//!
//! # What a release is, and what it deliberately is not
//!
//! A release is **bookkeeping over writes that already had a home**. Creating one expands to one
//! [`crate::scheduling`] row per `(node, store)` pair, each carrying the release's id, and ADR-0077's
//! existing activator applies them. There is no second activator, and no second way for a config
//! version to be born — the alternative was rejected in ADR-0125 §1 because two code paths that write
//! config versions become two sets of prerequisite checks, two audit shapes, and two ways to be wrong
//! about what a store is running.
//!
//! So what lives here is the part a row cannot hold on its own: an identity, a state, and **the
//! conversion**.
//!
//! # Why the conversion is the interesting half
//!
//! "Monday 04:00 at each store" is not a time. It is one instant per timezone, and a fleet spanning Ho
//! Chi Minh City and Tokyo has a two-hour spread that nobody has had to write down before — so today an
//! operator picks one UTC instant and half the fleet switches over during service (finding **F17**).
//!
//! [`resolve_moment`] is the answer, and it runs **on the cloud at schedule time** (decision **D8**,
//! option O2). Not at fire time, because an operator who cannot be *shown* the forty instants they are
//! committing to cannot review them; not at the edge, because that is a second scheduler at the one
//! place in the system with no operator (option O3, deferred to the roadmap's B·W7 line).
//!
//! A store whose `locale` node has never been published is **refused by name**, not defaulted to UTC:
//! "04:00 at this store" has no meaning at a store whose timezone nobody recorded, and quietly assuming
//! UTC would put a Vietnamese publish in the middle of dinner service. A release given a plain
//! [`ReleaseMoment::Instant`] needs no locale at all, which is the escape hatch for a store being set
//! up.

use core::future::Future;

use pos_core::business_date::{StoreTimeZone, resolve_local_time};

use crate::config_tree::prerequisites_for;
use pos_proto::ids::{StoreId, TenantId};

/// Where a release stands. Derived from its pairs, stored so a list read need not aggregate.
///
/// `Partial` is the state the whole record exists for: N × M writes fail individually, and at forty
/// stores an operator needs the two shops that did not take it, not a red cross over the fleet. There
/// is no `RolledBack` — an applied write is a config version, and the way back is
/// [ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)'s version history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStatus {
    /// Being assembled. Nothing is scheduled; nothing will fire.
    Draft,
    /// Every pair is a pending scheduled publish waiting for its instant.
    Scheduled,
    /// The activator has started on it and not finished. 360 writes are not instantaneous, and a
    /// console showing `Scheduled` halfway through is lying to somebody watching a switchover.
    Applying,
    /// Every pair published.
    Applied,
    /// Terminal, with a per-pair report: some pairs published and some did not.
    Partial,
}

impl ReleaseStatus {
    /// The stored token.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Draft => "DRAFT",
            Self::Scheduled => "SCHEDULED",
            Self::Applying => "APPLYING",
            Self::Applied => "APPLIED",
            Self::Partial => "PARTIAL",
        }
    }

    /// Parses a stored token, defaulting an unknown one to [`Draft`](ReleaseStatus::Draft).
    ///
    /// Defaulting to `Draft` rather than to a terminal state is deliberate: an unreadable status must
    /// not make a release look finished, and a `Draft` fires nothing.
    #[must_use]
    pub fn from_wire(token: &str) -> Self {
        match token {
            "SCHEDULED" => Self::Scheduled,
            "APPLYING" => Self::Applying,
            "APPLIED" => Self::Applied,
            "PARTIAL" => Self::Partial,
            _ => Self::Draft,
        }
    }
}

/// When a release goes out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseMoment {
    /// A local date and time, resolved separately for each store's own clock. The operator's
    /// "Monday 04:00, local".
    WallClock {
        /// The local date, `YYYY-MM-DD`.
        date: String,
        /// The local time of day, `HH:MM`.
        time: String,
    },
    /// One instant for the whole fleet, in Unix milliseconds. Needs no store locale.
    Instant(i64),
}

/// Why one store could not be given an instant.
///
/// Each names the store and what it is waiting for, because the console's job here is to send the
/// operator somewhere they can fix it — and "this release could not be scheduled" is not that.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveRefusal {
    /// The store has published no `locale` node, so it has no timezone to resolve against.
    #[error(
        "store {store} has published no `locale` node, so `{date} {time}` local has no meaning there"
    )]
    NoPublishedLocale {
        /// The store that cannot be scheduled.
        store: String,
        /// The local date that was asked for.
        date: String,
        /// The local time that was asked for.
        time: String,
    },
    /// The store published a timezone the bundled tzdb does not know.
    #[error("store {store} publishes timezone `{timezone}`, which is not a known IANA zone")]
    UnknownTimeZone {
        /// The store that cannot be scheduled.
        store: String,
        /// The unusable name, as published.
        timezone: String,
    },
    /// The date or time is not a real civil moment.
    #[error("`{date} {time}` is not a date and time this calendar has")]
    NotACivilMoment {
        /// The date as given.
        date: String,
        /// The time as given.
        time: String,
    },
}

/// A store a release is aimed at, and the timezone it publishes.
///
/// `timezone` is `None` for a store with no published `locale` node — the caller reads it from the
/// effective config, and the distinction between "no locale" and "a locale with no timezone" is not one
/// this module needs, because both mean the same thing to an operator: nobody has said where this shop
/// is.
#[derive(Debug, Clone)]
pub struct TargetStore {
    /// The store.
    pub store_id: StoreId,
    /// Its published IANA timezone name, if it has published one.
    pub timezone: Option<String>,
}

/// A local date and time, split into the parts [`resolve_local_time`] takes.
///
/// Strict on purpose: `YYYY-MM-DD` and `HH:MM`, nothing else. These values come from an HTML `date`
/// and `time` input, which produce exactly this, and a lenient parser here would turn a console bug
/// into a publish at the wrong hour.
fn civil_parts(date: &str, time: &str) -> Option<(i16, u8, u8, u8, u8)> {
    let mut ymd = date.split('-');
    let year: i16 = ymd.next()?.parse().ok()?;
    let month: u8 = ymd.next()?.parse().ok()?;
    let day: u8 = ymd.next()?.parse().ok()?;
    if ymd.next().is_some() {
        return None;
    }
    let mut hm = time.split(':');
    let hour: u8 = hm.next()?.parse().ok()?;
    let minute: u8 = hm.next()?.parse().ok()?;
    // An `HH:MM:SS` from a browser that offers seconds is accepted and its seconds ignored; a release
    // is not scheduled to the second, and refusing would be a refusal the operator cannot act on.
    if hm.next().is_some() && hm.next().is_some() {
        return None;
    }
    Some((year, month, day, hour, minute))
}

/// The instant this release fires at one store, in Unix milliseconds.
///
/// An [`Instant`](ReleaseMoment::Instant) is the same number everywhere and never refuses. A
/// [`WallClock`](ReleaseMoment::WallClock) is resolved against the store's own published timezone,
/// applying the one disambiguation policy
/// [ADR-0014](../../../docs/adr/0014-datetime-library.md) fixes — a skipped time resolves forward, a
/// doubled one to the earlier instant — so a release scheduled across a DST boundary lands once rather
/// than twice or never.
///
/// # Errors
///
/// A [`ResolveRefusal`] naming the store and what it is waiting for.
pub fn resolve_moment(moment: &ReleaseMoment, store: &TargetStore) -> Result<i64, ResolveRefusal> {
    let (date, time) = match moment {
        ReleaseMoment::Instant(at_ms) => return Ok(*at_ms),
        ReleaseMoment::WallClock { date, time } => (date, time),
    };
    let Some(name) = store.timezone.as_deref().filter(|name| !name.is_empty()) else {
        return Err(ResolveRefusal::NoPublishedLocale {
            store: store.store_id.to_string(),
            date: date.clone(),
            time: time.clone(),
        });
    };
    let zone =
        StoreTimeZone::from_iana_name(name).map_err(|_| ResolveRefusal::UnknownTimeZone {
            store: store.store_id.to_string(),
            timezone: name.to_owned(),
        })?;
    let not_civil = || ResolveRefusal::NotACivilMoment {
        date: date.clone(),
        time: time.clone(),
    };
    let (year, month, day, hour, minute) = civil_parts(date, time).ok_or_else(not_civil)?;
    let resolved =
        resolve_local_time(year, month, day, hour, minute, &zone).map_err(|_| not_civil())?;
    Ok(resolved.as_milliseconds_since_epoch())
}

/// One store's place in a release: the instant it fires at, or the refusal that stops it.
#[derive(Debug, Clone)]
pub struct ResolvedStore {
    /// The store.
    pub store_id: StoreId,
    /// Its instant, or why it has none.
    pub outcome: Result<i64, ResolveRefusal>,
}

/// Resolves a moment for every target store, keeping the refusals rather than stopping at the first.
///
/// Every store is resolved because the operator's next question after "this cannot be scheduled" is
/// "which ones?", and answering it one store per attempt is how a forty-store release takes forty
/// round trips to become schedulable. This is the same reasoning
/// [ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) applied to the batch
/// publish, and a release is a batch publish with a clock.
#[must_use]
pub fn resolve_for_stores(moment: &ReleaseMoment, stores: &[TargetStore]) -> Vec<ResolvedStore> {
    stores
        .iter()
        .map(|store| ResolvedStore {
            store_id: store.store_id,
            outcome: resolve_moment(moment, store),
        })
        .collect()
}

/// A release to create. Its pairs are written separately, as scheduled publishes carrying its id.
#[derive(Debug, Clone)]
pub struct NewRelease {
    /// The row's id (a ULID string), server-minted.
    pub id: String,
    /// The tenant.
    pub tenant_id: TenantId,
    /// What the operator called it — "Tết 2027", not a ULID.
    pub name: String,
    /// The cohort it was aimed at, or `None` for an explicit store list.
    ///
    /// Recorded for the report only. The concrete stores live on the pairs, because a group is
    /// expanded at schedule time (ADR-0125 §3): a store added to the cohort on Friday must not
    /// silently receive a Monday menu nobody checked it against.
    pub target_group_id: Option<String>,
    /// When it goes out, or `None` for a draft nobody has timed yet.
    pub moment: Option<ReleaseMoment>,
    /// The admin who created it, for the audit trail.
    pub created_by: String,
}

/// A stored release.
#[derive(Debug, Clone)]
pub struct Release {
    /// The id.
    pub id: String,
    /// The tenant.
    pub tenant_id: TenantId,
    /// The operator's name for it.
    pub name: String,
    /// Where it stands — the roll-up of its pairs, stored so a list read need not aggregate.
    pub status: ReleaseStatus,
    /// The cohort it was aimed at, if any.
    pub target_group_id: Option<String>,
    /// When it goes out, or `None` for a draft with no time yet.
    pub moment: Option<ReleaseMoment>,
    /// The admin who created it.
    pub created_by: String,
    /// When it was created, Unix milliseconds.
    pub created_at_ms: i64,
    /// When its row last changed, Unix milliseconds.
    pub updated_at_ms: i64,
}

/// Persists and reads releases.
///
/// Only the identity and the roll-up: the pairs are [`crate::scheduling::ScheduledPublish`] rows
/// carrying a `release_id`, read through that seam's `list_for_release`. Splitting them is what keeps
/// a release from becoming a second publish path (ADR-0125 §1) — this store cannot write a config
/// version and has no way to try.
pub trait ReleaseStore {
    /// Creates a release. Its pairs are scheduled separately.
    fn create(
        &self,
        release: &NewRelease,
    ) -> impl Future<Output = Result<(), ReleaseStoreError>> + Send;

    /// A tenant's releases, newest first, for the console's list.
    fn list_for_tenant(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Release>, ReleaseStoreError>> + Send;

    /// One release, or `None` — an absence, not a failure: an operator following a stale link is an
    /// ordinary thing to do.
    fn fetch_one(
        &self,
        tenant_id: TenantId,
        id: &str,
    ) -> impl Future<Output = Result<Option<Release>, ReleaseStoreError>> + Send;

    /// Moves a release to `status`. Returns whether a release with that id existed in the tenant.
    fn set_status(
        &self,
        tenant_id: TenantId,
        id: &str,
        status: ReleaseStatus,
    ) -> impl Future<Output = Result<bool, ReleaseStoreError>> + Send;

    /// Every release still scheduled or applying, across all tenants.
    ///
    /// What the activator re-tallies after a pass. Fleet-wide and read as the trusted pool owner, the
    /// same posture as the due read it follows — a per-tenant loop would make the roll-up cost scale
    /// with the tenant count rather than with the work actually in flight.
    fn in_flight(&self) -> impl Future<Output = Result<Vec<Release>, ReleaseStoreError>> + Send;
}

/// A failure of the release store itself.
#[derive(Debug, thiserror::Error)]
#[error("the release store failed: {0}")]
pub struct ReleaseStoreError(String);

impl ReleaseStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The order a release's nodes must be applied in, so the set satisfies its own prerequisites.
///
/// This is the half of [ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §7
/// that only a release needs. The batch and single publishes read the prerequisite table to *refuse*
/// a `menu` at a store with no `tax`; a release carrying both is not refusable — it brings its own
/// prerequisite — but it is only true that it does if the activator writes `tax` first. Applied in
/// the operator's typing order, a Tết release would publish a menu against last year's tax table for
/// however long the next write took.
///
/// A stable topological sort over `NODE_PREREQUISITES`, which is a handful of rules over a handful
/// of nodes: a node goes after every prerequisite that is *also in this release* (one that is not is
/// either already published, and the refusal check has passed it, or missing, and the refusal check
/// has stopped it). Input order is otherwise preserved, so two unrelated nodes stay where the
/// operator put them and the report reads as the release was written.
///
/// Termination does not depend on the table being acyclic: each pass must place at least one node or
/// the remainder is appended as given. A cycle in the prerequisites would be a bug in the table
/// rather than in a release, and a release that refuses to apply is worse than one that applies a
/// cyclic pair in input order.
#[must_use]
pub fn order_for_release(nodes: &[String]) -> Vec<String> {
    let mut remaining: Vec<&String> = nodes.iter().collect();
    let mut ordered: Vec<String> = Vec::with_capacity(nodes.len());
    while !remaining.is_empty() {
        let placed_before = ordered.len();
        let mut next: Vec<&String> = Vec::with_capacity(remaining.len());
        for node in remaining {
            let waiting = prerequisites_for(node).iter().any(|needed| {
                !ordered.iter().any(|done| done == needed) && nodes.iter().any(|row| row == needed)
            });
            if waiting {
                next.push(node);
            } else {
                ordered.push(node.clone());
            }
        }
        if ordered.len() == placed_before {
            // Nothing could be placed: the remainder depends on itself. Append it as given rather
            // than loop, and let the table's own tests be where a cycle is caught.
            ordered.extend(next.into_iter().cloned());
            break;
        }
        remaining = next;
    }
    ordered
}

/// The roll-up a release's state is derived from: how many of its pairs have settled, and how.
///
/// Stored on the release so a list read need not aggregate, but computed here so the stored value and
/// the pairs cannot disagree about a release nobody has looked at in a week.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairTally {
    /// Pairs still waiting for their instant.
    pub pending: usize,
    /// Pairs that published.
    pub applied: usize,
    /// Pairs that failed, or were cancelled after the release started applying.
    pub failed: usize,
}

/// The state a release with this tally is in.
///
/// A release with no pairs at all is [`Draft`](ReleaseStatus::Draft): an empty set has not been
/// scheduled, whatever its row says, and calling it `Applied` would report a Tết that never shipped as
/// a success.
#[must_use]
pub fn status_from_tally(tally: PairTally) -> ReleaseStatus {
    if tally.pending + tally.applied + tally.failed == 0 {
        return ReleaseStatus::Draft;
    }
    if tally.pending > 0 {
        // Something has already landed, so this is no longer merely waiting.
        return if tally.applied + tally.failed > 0 {
            ReleaseStatus::Applying
        } else {
            ReleaseStatus::Scheduled
        };
    }
    if tally.failed > 0 {
        ReleaseStatus::Partial
    } else {
        ReleaseStatus::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PairTally, ReleaseMoment, ReleaseStatus, ResolveRefusal, TargetStore, order_for_release,
        resolve_for_stores, resolve_moment, status_from_tally,
    };
    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    /// A store id from a readable seed, so a failure message names something a reader can follow.
    fn store(seed: u128) -> StoreId {
        StoreId::new(Ulid::from_parts(1_700_000_000_000, seed))
    }

    fn at(timezone: Option<&str>, seed: u128) -> TargetStore {
        TargetStore {
            store_id: store(seed),
            timezone: timezone.map(str::to_owned),
        }
    }

    fn monday_0400() -> ReleaseMoment {
        ReleaseMoment::WallClock {
            date: "2027-02-08".to_owned(),
            time: "04:00".to_owned(),
        }
    }

    #[test]
    fn one_wall_clock_time_is_two_instants_for_two_timezones() {
        // The whole point of D8. Ho Chi Minh City is UTC+7 and Tokyo is UTC+9 year round, so "04:00
        // local" at the two shops is two hours apart — and a single UTC instant would switch one of
        // them over during service.
        let saigon = resolve_moment(&monday_0400(), &at(Some("Asia/Ho_Chi_Minh"), 1))
            .expect("a real zone resolves");
        let tokyo = resolve_moment(&monday_0400(), &at(Some("Asia/Tokyo"), 2))
            .expect("a real zone resolves");
        assert_ne!(saigon, tokyo, "two timezones cannot share one instant");
        assert_eq!(
            saigon - tokyo,
            2 * 60 * 60 * 1000,
            "Tokyo's 04:00 comes two hours before Ho Chi Minh City's"
        );
    }

    #[test]
    fn a_store_with_no_published_locale_is_refused_by_name() {
        let refusal = resolve_moment(&monday_0400(), &at(None, 3))
            .expect_err("a store with no timezone cannot be scheduled in local time");
        let ResolveRefusal::NoPublishedLocale { store: named, .. } = &refusal else {
            panic!("expected the no-locale refusal, got {refusal:?}");
        };
        assert_eq!(*named, store(3).to_string(), "the refusal names the store");
        // The message has to send the operator somewhere, so it says what is missing and what was
        // asked for — not merely that something failed.
        let said = refusal.to_string();
        assert!(said.contains("locale"), "{said}");
        assert!(said.contains("04:00"), "{said}");
    }

    #[test]
    fn an_instant_release_needs_no_locale_at_all() {
        // The escape hatch for a store being set up: a UTC moment is the same number everywhere, so a
        // store with no published locale is schedulable into it.
        let resolved = resolve_moment(&ReleaseMoment::Instant(1_800_000_000_000), &at(None, 4))
            .expect("an instant needs no timezone");
        assert_eq!(resolved, 1_800_000_000_000);
    }

    #[test]
    fn an_unknown_timezone_is_its_own_refusal() {
        // Distinct from "no locale" because the fix is different: this store *has* been set up, and
        // somebody published a name the tzdb does not carry.
        let refusal = resolve_moment(&monday_0400(), &at(Some("Mars/Olympus_Mons"), 5))
            .expect_err("an unknown zone cannot be resolved");
        assert!(
            matches!(refusal, ResolveRefusal::UnknownTimeZone { .. }),
            "{refusal:?}"
        );
    }

    #[test]
    fn a_date_this_calendar_does_not_have_is_refused() {
        let moment = ReleaseMoment::WallClock {
            date: "2027-02-30".to_owned(),
            time: "04:00".to_owned(),
        };
        let refusal = resolve_moment(&moment, &at(Some("Asia/Ho_Chi_Minh"), 6))
            .expect_err("February has no thirtieth");
        assert!(
            matches!(refusal, ResolveRefusal::NotACivilMoment { .. }),
            "{refusal:?}"
        );
    }

    #[test]
    fn every_store_is_resolved_so_the_operator_sees_every_gap_at_once() {
        // A forty-store release that reports one refusal per attempt takes forty round trips to become
        // schedulable. The batch publish learned this (ADR-0122); a release is a batch with a clock.
        let stores = [
            at(Some("Asia/Ho_Chi_Minh"), 7),
            at(None, 8),
            at(Some("Asia/Tokyo"), 9),
            at(None, 10),
        ];
        let resolved = resolve_for_stores(&monday_0400(), &stores);
        assert_eq!(resolved.len(), 4, "every store gets an answer");
        assert_eq!(
            resolved.iter().filter(|row| row.outcome.is_err()).count(),
            2,
            "both gaps are reported, not just the first"
        );
    }

    #[test]
    fn a_release_applies_its_own_prerequisites_first() {
        // The failure this closes: a Tết release carrying both, applied in typing order, publishes
        // the menu against last year's tax table until the next write lands.
        let nodes = vec![
            "menu".to_owned(),
            "campaigns".to_owned(),
            "tax".to_owned(),
            "locale".to_owned(),
        ];
        let ordered = order_for_release(&nodes);
        let at = |name: &str| {
            ordered
                .iter()
                .position(|node| node == name)
                .unwrap_or_else(|| panic!("{name} is missing from {ordered:?}"))
        };
        assert!(at("tax") < at("menu"), "{ordered:?}");
        assert!(at("locale") < at("menu"), "{ordered:?}");
        assert_eq!(ordered.len(), nodes.len(), "no node is dropped");
    }

    #[test]
    fn a_prerequisite_the_release_does_not_carry_does_not_hold_it_back() {
        // `tax` is not in this release, so it is either already published — the refusal check let
        // this through — or missing, and the refusal check stopped it. Either way the ordering has
        // nothing to wait for, and a `menu` alone must not be held behind a node nobody is sending.
        let nodes = vec!["menu".to_owned()];
        assert_eq!(order_for_release(&nodes), vec!["menu".to_owned()]);
    }

    #[test]
    fn unrelated_nodes_keep_the_order_they_were_written_in() {
        // The report reads as the release was written, so an operator can check it against what
        // they typed.
        let nodes = vec![
            "reason_codes".to_owned(),
            "stations".to_owned(),
            "floor".to_owned(),
        ];
        assert_eq!(order_for_release(&nodes), nodes);
    }

    #[test]
    fn an_empty_release_orders_to_nothing() {
        assert!(order_for_release(&[]).is_empty());
    }

    #[test]
    fn a_release_with_no_pairs_is_a_draft_not_a_success() {
        // The trap this closes: `failed == 0 && pending == 0` reads as "everything worked" and would
        // report a Tết that never shipped as applied.
        assert_eq!(
            status_from_tally(PairTally {
                pending: 0,
                applied: 0,
                failed: 0,
            }),
            ReleaseStatus::Draft
        );
    }

    #[test]
    fn the_tally_names_the_four_live_states() {
        let cases = [
            (3, 0, 0, ReleaseStatus::Scheduled),
            (2, 1, 0, ReleaseStatus::Applying),
            (2, 0, 1, ReleaseStatus::Applying),
            (0, 3, 0, ReleaseStatus::Applied),
            (0, 2, 1, ReleaseStatus::Partial),
            (0, 0, 3, ReleaseStatus::Partial),
        ];
        for (pending, applied, failed, expected) in cases {
            assert_eq!(
                status_from_tally(PairTally {
                    pending,
                    applied,
                    failed,
                }),
                expected,
                "pending {pending}, applied {applied}, failed {failed}"
            );
        }
    }

    #[test]
    fn an_unreadable_status_reads_as_draft_so_nothing_fires() {
        assert_eq!(ReleaseStatus::from_wire("WAT"), ReleaseStatus::Draft);
        for status in [
            ReleaseStatus::Draft,
            ReleaseStatus::Scheduled,
            ReleaseStatus::Applying,
            ReleaseStatus::Applied,
            ReleaseStatus::Partial,
        ] {
            assert_eq!(
                ReleaseStatus::from_wire(status.as_wire()),
                status,
                "{status:?} survives a round trip through its stored token"
            );
        }
    }
}
