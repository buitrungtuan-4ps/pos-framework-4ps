// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the till's status bar says about the store's link to the cloud
//! ([ADR-0137](../../../docs/adr/0137-a-deep-outbox-warns-and-never-refuses.md)).
//!
//! `docs/ui-ux.md` §4 has always said what a store that lost the internet shows: *"Offline — selling
//! normally"*, a count of pending events, and nothing blocked. Until this module nothing on the edge
//! could say either half. The status bar reported the LAN link to this box and only that, so a store
//! whose outbox had been filling for three days looked exactly like one that had synced a second ago
//! — and the first anyone heard of it was the refusal ADR-0137 removes.
//!
//! # Two facts, measured by the two things that already know them
//!
//! - **The cloud link** is what the outbox drain ([`crate::event_publish`]) last saw: a pass that
//!   published or found nothing to publish means the link is up, a failed one means it is down. Not
//!   a probe of its own: the drain already talks to the cloud every few seconds, and a second opinion
//!   from a separate ping would be a way for the bar and the drain to disagree.
//! - **The outbox depth** is read from the store on demand and cached for [`DEPTH_TTL`], so ten
//!   tablets polling the bar cost one count between them. The heartbeat, which reads the same depth
//!   on its own interval, feeds the same cache.
//!
//! # A level, not a limit
//!
//! [`OutboxLevel`] grades the depth against [`OUTBOX_PLANNED_DEPTH`], and the grade is only ever a
//! warning. Crossing a threshold upward logs once — `WARN` for the first two, `ERROR` past the plan —
//! so a support engineer reading the box's log finds the day the store went quiet rather than a line
//! per sale.

use core::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use core::time::Duration;
use std::sync::{Mutex, PoisonError};

use tokio::time::Instant;

use pos_proto::time::Timestamp;

/// How many unsent events a store is planned to hold before its warning turns red and stays red.
///
/// Sized from the busiest store the capacity model describes: a bill is at least `2 × lines + 7`
/// events, so five hundred bills a day at eight lines each is about 11,500 events a day, and this is
/// a little under nine days of that. A quieter store gets weeks. It is **not a ceiling** — the store
/// keeps selling past it (ADR-0137) — only the point at which somebody should already be on the
/// phone.
pub const OUTBOX_PLANNED_DEPTH: u64 = 100_000;

/// How long a measured depth is reused before the store is asked again.
///
/// Short enough that the bar moves while somebody watches it, long enough that every device polling
/// at once costs one count. The count is an index scan on SQLite, cheap but not free, and it is
/// served by the same writer thread a sale is.
pub const DEPTH_TTL: Duration = Duration::from_secs(5);

/// How deep the outbox is, graded against [`OUTBOX_PLANNED_DEPTH`].
///
/// Ordered, so "rose" and "fell" are comparisons. The wire names follow the naming standard's enum
/// rule, including the zero value nothing emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutboxLevel {
    /// Under half the plan — including an empty outbox, the ordinary state of a connected store.
    Normal,
    /// Half the plan or more. The store has been offline for days, or its link is too slow.
    Elevated,
    /// Four fifths of the plan or more. Somebody should be fixing the link now.
    High,
    /// Past the plan. Still selling; the warning does not go away until the outbox drains.
    Beyond,
}

impl OutboxLevel {
    /// Grades `depth` against `planned`. A `planned` of zero grades everything but an empty outbox
    /// [`Beyond`](Self::Beyond), which is the only reading of "plan for nothing" that does not hide a
    /// backlog.
    #[must_use]
    pub const fn of(depth: u64, planned: u64) -> Self {
        // Integer arithmetic only, per the money rule's spirit: `depth * 10 >= planned * 8` rather
        // than a ratio. Saturating, so a depth near `u64::MAX` grades high instead of wrapping low.
        let scaled = depth.saturating_mul(10);
        if depth > 0 && scaled >= planned.saturating_mul(10) {
            Self::Beyond
        } else if depth > 0 && scaled >= planned.saturating_mul(8) {
            Self::High
        } else if depth > 0 && scaled >= planned.saturating_mul(5) {
            Self::Elevated
        } else {
            Self::Normal
        }
    }

    /// The wire token (`OUTBOX_LEVEL_*`).
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Normal => "OUTBOX_LEVEL_NORMAL",
            Self::Elevated => "OUTBOX_LEVEL_ELEVATED",
            Self::High => "OUTBOX_LEVEL_HIGH",
            Self::Beyond => "OUTBOX_LEVEL_BEYOND",
        }
    }

    const fn to_bits(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Elevated => 1,
            Self::High => 2,
            Self::Beyond => 3,
        }
    }

    const fn from_bits(bits: u8) -> Self {
        match bits {
            1 => Self::Elevated,
            2 => Self::High,
            3 => Self::Beyond,
            _ => Self::Normal,
        }
    }
}

/// What the outbox drain last saw of the cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudLink {
    /// Nothing has tried yet — the drain has not run, or this store has no cloud to reach (a demo,
    /// or a box before activation). Not "offline": nobody measured it.
    Unknown,
    /// The last pass reached the cloud.
    Online,
    /// The last pass could not.
    Offline,
}

impl CloudLink {
    /// The wire token (`CLOUD_LINK_*`). [`Unknown`](Self::Unknown) is the `*_UNSPECIFIED` zero value.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Unknown => "CLOUD_LINK_UNSPECIFIED",
            Self::Online => "CLOUD_LINK_ONLINE",
            Self::Offline => "CLOUD_LINK_OFFLINE",
        }
    }

    const fn to_bits(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Online => 1,
            Self::Offline => 2,
        }
    }

    const fn from_bits(bits: u8) -> Self {
        match bits {
            1 => Self::Online,
            2 => Self::Offline,
            _ => Self::Unknown,
        }
    }
}

/// The two facts, as the status bar reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    /// Events committed and not yet acknowledged by the cloud.
    pub outbox_depth: u64,
    /// [`Self::outbox_depth`] graded against the plan.
    pub outbox_level: OutboxLevel,
    /// What the drain last saw of the cloud.
    pub cloud_link: CloudLink,
    /// When the drain last reached the cloud, or `None` if it never has since this process started.
    pub last_sync_time: Option<Timestamp>,
}

/// The shared cell the drain, the heartbeat and the status route read and write.
///
/// Lock-free where it is written on every pass (the link and the last-reached instant) and behind a
/// short mutex where it is not (the cached depth, which is written at most once per [`DEPTH_TTL`]).
#[derive(Debug)]
pub struct SyncStatus {
    link: AtomicU8,
    /// Milliseconds since the epoch, or [`i64::MIN`] for "never".
    last_sync_ms: AtomicI64,
    /// The level last logged, so a crossing logs once rather than on every read.
    level: AtomicU8,
    depth: Mutex<Option<(Instant, u64)>>,
}

impl Default for SyncStatus {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncStatus {
    /// A store nobody has measured yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            link: AtomicU8::new(0),
            last_sync_ms: AtomicI64::new(i64::MIN),
            level: AtomicU8::new(0),
            depth: Mutex::new(None),
        }
    }

    /// The drain reached the cloud at `now`.
    pub fn mark_online(&self, now: Timestamp) {
        let before = CloudLink::from_bits(
            self.link
                .swap(CloudLink::Online.to_bits(), Ordering::Relaxed),
        );
        self.last_sync_ms
            .store(now.as_milliseconds_since_epoch(), Ordering::Relaxed);
        if before == CloudLink::Offline {
            tracing::info!("sync: the cloud is reachable again; the outbox is draining");
        }
    }

    /// The drain could not reach the cloud. Logged on the transition only: the drain retries every
    /// few seconds and says why itself.
    pub fn mark_offline(&self) {
        let before = CloudLink::from_bits(
            self.link
                .swap(CloudLink::Offline.to_bits(), Ordering::Relaxed),
        );
        if before != CloudLink::Offline {
            tracing::warn!(
                "sync: the cloud is unreachable; the store keeps selling and events wait in the outbox"
            );
        }
    }

    /// What the drain last saw.
    #[must_use]
    pub fn cloud_link(&self) -> CloudLink {
        CloudLink::from_bits(self.link.load(Ordering::Relaxed))
    }

    /// When the drain last reached the cloud.
    #[must_use]
    pub fn last_sync_time(&self) -> Option<Timestamp> {
        let ms = self.last_sync_ms.load(Ordering::Relaxed);
        if ms == i64::MIN {
            return None;
        }
        Timestamp::from_milliseconds_since_epoch(ms).ok()
    }

    /// A depth measured within [`DEPTH_TTL`] of `now`, if there is one.
    #[must_use]
    pub fn cached_depth(&self, now: Instant) -> Option<u64> {
        let cached = *self.depth.lock().unwrap_or_else(PoisonError::into_inner);
        cached.and_then(|(at, depth)| {
            (now.saturating_duration_since(at) < DEPTH_TTL).then_some(depth)
        })
    }

    /// Records a measured depth and logs a threshold crossing, once, in the direction it went.
    ///
    /// Returns the level the depth grades to.
    pub fn record_depth(&self, depth: u64, now: Instant) -> OutboxLevel {
        *self.depth.lock().unwrap_or_else(PoisonError::into_inner) = Some((now, depth));
        let level = OutboxLevel::of(depth, OUTBOX_PLANNED_DEPTH);
        let before = OutboxLevel::from_bits(self.level.swap(level.to_bits(), Ordering::Relaxed));
        if level > before {
            match level {
                OutboxLevel::Beyond => tracing::error!(
                    depth,
                    planned = OUTBOX_PLANNED_DEPTH,
                    "sync: the outbox is past the depth this store was planned for. The store is still \
                     selling and nothing is lost — every event is in this box's own database — but \
                     the cloud has not heard from it for days: fix the link"
                ),
                OutboxLevel::High | OutboxLevel::Elevated => tracing::warn!(
                    depth,
                    planned = OUTBOX_PLANNED_DEPTH,
                    level = level.as_wire(),
                    "sync: the outbox is filling; the store is still selling, and the cloud is not \
                     receiving its events"
                ),
                OutboxLevel::Normal => {}
            }
        } else if level < before && level == OutboxLevel::Normal {
            tracing::info!(
                depth,
                "sync: the outbox is back under half its planned depth"
            );
        }
        level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_outbox_is_normal_even_against_a_plan_of_nothing() {
        assert_eq!(
            OutboxLevel::of(0, OUTBOX_PLANNED_DEPTH),
            OutboxLevel::Normal
        );
        assert_eq!(OutboxLevel::of(0, 0), OutboxLevel::Normal);
        assert_eq!(OutboxLevel::of(1, 0), OutboxLevel::Beyond);
    }

    #[test]
    fn the_thresholds_sit_at_half_four_fifths_and_the_plan() {
        let plan = 1_000;
        assert_eq!(OutboxLevel::of(499, plan), OutboxLevel::Normal);
        assert_eq!(OutboxLevel::of(500, plan), OutboxLevel::Elevated);
        assert_eq!(OutboxLevel::of(799, plan), OutboxLevel::Elevated);
        assert_eq!(OutboxLevel::of(800, plan), OutboxLevel::High);
        assert_eq!(OutboxLevel::of(999, plan), OutboxLevel::High);
        assert_eq!(OutboxLevel::of(1_000, plan), OutboxLevel::Beyond);
        assert_eq!(OutboxLevel::of(u64::MAX, plan), OutboxLevel::Beyond);
    }

    #[test]
    fn a_depth_is_reused_inside_its_ttl_and_not_after() {
        let status = SyncStatus::new();
        let at = Instant::now();
        assert_eq!(status.cached_depth(at), None);
        status.record_depth(42, at);
        assert_eq!(status.cached_depth(at), Some(42));
        assert_eq!(status.cached_depth(at + DEPTH_TTL), None);
    }

    #[test]
    fn the_link_is_unknown_until_the_drain_says_otherwise() {
        let status = SyncStatus::new();
        assert_eq!(status.cloud_link(), CloudLink::Unknown);
        assert_eq!(status.last_sync_time(), None);

        status.mark_offline();
        assert_eq!(status.cloud_link(), CloudLink::Offline);
        assert_eq!(status.last_sync_time(), None);

        let now =
            Timestamp::from_milliseconds_since_epoch(1_750_000_000_000).expect("a valid instant");
        status.mark_online(now);
        assert_eq!(status.cloud_link(), CloudLink::Online);
        assert_eq!(status.last_sync_time(), Some(now));
    }

    #[test]
    fn a_crossing_is_remembered_so_it_is_not_logged_twice() {
        let status = SyncStatus::new();
        let at = Instant::now();
        assert_eq!(
            status.record_depth(OUTBOX_PLANNED_DEPTH / 2, at),
            OutboxLevel::Elevated
        );
        assert_eq!(
            OutboxLevel::from_bits(status.level.load(Ordering::Relaxed)),
            OutboxLevel::Elevated
        );
        assert_eq!(status.record_depth(0, at), OutboxLevel::Normal);
        assert_eq!(
            OutboxLevel::from_bits(status.level.load(Ordering::Relaxed)),
            OutboxLevel::Normal
        );
    }
}
