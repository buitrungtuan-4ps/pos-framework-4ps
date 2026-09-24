// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the app knows about its edge, and which changes deserve a notification.
//!
//! Pure: the monitor feeds two consecutive [`Snapshot`]s to [`notices`] and shows what comes back.
//! Everything that decides *whether* to interrupt somebody lives here, where a test can drive it.
//!
//! # What is and is not known
//!
//! The edge answers three questions for the app. `/healthz` (no credential) says whether it is up and
//! which release it runs. `GET /api/pair/devices` (the paired-device gate only) says whether this
//! device is still paired, and asking it touches nobody's sign-in. `/api/sync` and `/api/printers`
//! sit behind the **signed-in** gate too, which has two consequences the monitor respects:
//!
//! - with nobody signed in on this device they answer `403`, so the reading is
//!   [`NotRead::SignInNeeded`] rather than a guess;
//! - with somebody signed in, every answer resets that person's idle window (ADR-0091's thirty
//!   minutes). The till's own status bar already asks every fifteen seconds while it is open, so the
//!   monitor asks only while a till window is open and adds nothing the till was not already doing;
//!   with the till closed the reading is [`NotRead::TillClosed`], and nobody is kept signed in by a
//!   tray icon.
//!
//! **Printers have no online state on the edge.** `/api/printers` lists what the store published —
//! an id, a name, a station — and nothing about reachability. So the app reports a printer
//! appearing in or leaving that list; it cannot report one going offline until the edge publishes
//! that (an additive `/api/*` route, which is edge work, not this app's).
//!
//! # When a change is news
//!
//! Only between two readings that were both actually taken. The first reading after start, or after
//! the till was closed for an hour, is a baseline and interrupts nobody; a notification that fires
//! on every restart is one people learn to ignore. The one exception is losing the pairing, which is
//! news whenever it is first seen, because it is the one change that stops the till working.

use serde::Serialize;

/// Whether the edge answers `/healthz`, and which release it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state")]
pub(crate) enum EdgeReading {
    /// Not asked yet.
    #[serde(rename = "EDGE_UNSPECIFIED")]
    Unknown,
    /// Answered, reporting its version.
    #[serde(rename = "EDGE_UP")]
    Up {
        /// The edge binary's version, from `/healthz`.
        version: String,
    },
    /// Did not answer.
    #[serde(rename = "EDGE_DOWN")]
    Down,
}

/// Whether the edge still recognises this device's token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum PairingReading {
    /// Not asked, or the edge could not be asked.
    #[serde(rename = "PAIRING_UNSPECIFIED")]
    Unknown,
    /// The token is accepted.
    #[serde(rename = "PAIRING_PAIRED")]
    Paired,
    /// The edge refused the token (`401`): revoked, or a registry that did not survive a restart.
    #[serde(rename = "PAIRING_LOST")]
    Lost,
}

/// The cloud link as `/api/sync` reports it, in the edge's own wire tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum CloudLink {
    /// The edge has not tried the cloud since it started.
    #[serde(rename = "CLOUD_LINK_UNSPECIFIED")]
    NotTried,
    /// The last drain reached the cloud.
    #[serde(rename = "CLOUD_LINK_ONLINE")]
    Online,
    /// The last drain did not.
    #[serde(rename = "CLOUD_LINK_OFFLINE")]
    Offline,
}

impl CloudLink {
    /// Reads the edge's token; anything unrecognised is treated as not tried.
    pub(crate) fn from_wire(token: &str) -> Self {
        match token {
            "CLOUD_LINK_ONLINE" => Self::Online,
            "CLOUD_LINK_OFFLINE" => Self::Offline,
            _ => Self::NotTried,
        }
    }
}

/// How deep the outbox is against the edge's planned depth (ADR-0137): `ELEVATED` from half of it,
/// `HIGH` from four fifths, `BEYOND` past it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum OutboxLevel {
    /// A token this build does not know; never compared.
    #[serde(rename = "OUTBOX_LEVEL_UNSPECIFIED")]
    Unspecified,
    /// Under half the planned depth.
    #[serde(rename = "OUTBOX_LEVEL_NORMAL")]
    Normal,
    /// Half the planned depth or more.
    #[serde(rename = "OUTBOX_LEVEL_ELEVATED")]
    Elevated,
    /// Four fifths of it or more.
    #[serde(rename = "OUTBOX_LEVEL_HIGH")]
    High,
    /// Past it. The store still sells; the cloud is days behind.
    #[serde(rename = "OUTBOX_LEVEL_BEYOND")]
    Beyond,
}

impl OutboxLevel {
    /// Reads the edge's token.
    pub(crate) fn from_wire(token: &str) -> Self {
        match token {
            "OUTBOX_LEVEL_NORMAL" => Self::Normal,
            "OUTBOX_LEVEL_ELEVATED" => Self::Elevated,
            "OUTBOX_LEVEL_HIGH" => Self::High,
            "OUTBOX_LEVEL_BEYOND" => Self::Beyond,
            _ => Self::Unspecified,
        }
    }

    /// The order the levels rise in, or `None` for a level that cannot be compared.
    fn rank(self) -> Option<u8> {
        match self {
            Self::Unspecified => None,
            Self::Normal => Some(0),
            Self::Elevated => Some(1),
            Self::High => Some(2),
            Self::Beyond => Some(3),
        }
    }
}

/// One published printer, as `/api/printers` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Printer {
    /// The printer's device id in the published `devices` node.
    pub(crate) device_id: String,
    /// Its name, as the console published it.
    pub(crate) name: String,
}

/// What the signed-in reads said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StoreFacts {
    /// The cloud link.
    pub(crate) cloud_link: CloudLink,
    /// Events committed and not yet acknowledged by the cloud.
    pub(crate) outbox_depth: u64,
    /// The depth the levels are graded against. Not a limit.
    pub(crate) outbox_planned_depth: u64,
    /// The grade.
    pub(crate) outbox_level: OutboxLevel,
    /// The published printers, in publication order.
    pub(crate) printers: Vec<Printer>,
}

/// Why the signed-in reads were not taken, or did not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum NotRead {
    /// No till window is open, so asking would keep a signed-in person signed in.
    #[serde(rename = "NOT_READ_TILL_CLOSED")]
    TillClosed,
    /// Nobody is signed in on this device (`403`).
    #[serde(rename = "NOT_READ_SIGN_IN_NEEDED")]
    SignInNeeded,
    /// The edge is not answering.
    #[serde(rename = "NOT_READ_EDGE_DOWN")]
    EdgeDown,
    /// This device holds no accepted token.
    #[serde(rename = "NOT_READ_NOT_PAIRED")]
    NotPaired,
    /// The edge answered something this build could not read.
    #[serde(rename = "NOT_READ_FAILED")]
    Failed,
}

/// The cloud link, outbox and printers, or why they are not known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state")]
pub(crate) enum StoreReading {
    /// Not known this round.
    #[serde(rename = "STORE_NOT_READ")]
    NotRead {
        /// Why.
        reason: NotRead,
    },
    /// Read this round.
    #[serde(rename = "STORE_READ")]
    Read(StoreFacts),
}

/// One round of the monitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Snapshot {
    /// The edge itself.
    pub(crate) edge: EdgeReading,
    /// This device's pairing.
    pub(crate) pairing: PairingReading,
    /// The store's cloud link, outbox and printers.
    pub(crate) store: StoreReading,
}

impl Snapshot {
    /// Before the first round.
    pub(crate) fn unknown() -> Self {
        Self {
            edge: EdgeReading::Unknown,
            pairing: PairingReading::Unknown,
            store: StoreReading::NotRead {
                reason: NotRead::NotPaired,
            },
        }
    }
}

/// A change worth a desktop notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Notice {
    /// The edge stopped answering.
    EdgeDown,
    /// The edge answers again.
    EdgeBack,
    /// The edge no longer accepts this device's token.
    PairingLost,
    /// The cloud link went from online to offline.
    CloudLost {
        /// Events waiting to sync when it was noticed.
        waiting: u64,
    },
    /// The cloud link came back.
    CloudRestored,
    /// The outbox rose past a grade.
    OutboxRose {
        /// The grade it reached.
        level: OutboxLevel,
        /// Events waiting.
        depth: u64,
        /// The planned depth.
        planned: u64,
    },
    /// The outbox fell back under half the plan.
    OutboxNormal,
    /// A printer appeared in the published list.
    PrinterAdded {
        /// Its published name.
        name: String,
    },
    /// A printer left the published list.
    PrinterRemoved {
        /// Its published name.
        name: String,
    },
}

/// The changes between two consecutive rounds that deserve a notification, in a stable order.
pub(crate) fn notices(before: &Snapshot, after: &Snapshot) -> Vec<Notice> {
    let mut out = Vec::new();
    match (&before.edge, &after.edge) {
        (EdgeReading::Up { .. }, EdgeReading::Down) => out.push(Notice::EdgeDown),
        (EdgeReading::Down, EdgeReading::Up { .. }) => out.push(Notice::EdgeBack),
        _ => {}
    }
    if after.pairing == PairingReading::Lost && before.pairing != PairingReading::Lost {
        out.push(Notice::PairingLost);
    }
    if let (StoreReading::Read(was), StoreReading::Read(now)) = (&before.store, &after.store) {
        store_notices(was, now, &mut out);
    }
    out
}

/// The store-level changes between two readings that were both taken.
fn store_notices(was: &StoreFacts, now: &StoreFacts, out: &mut Vec<Notice>) {
    match (was.cloud_link, now.cloud_link) {
        (CloudLink::Online, CloudLink::Offline) => out.push(Notice::CloudLost {
            waiting: now.outbox_depth,
        }),
        (CloudLink::Offline, CloudLink::Online) => out.push(Notice::CloudRestored),
        _ => {}
    }
    if let (Some(from), Some(to)) = (was.outbox_level.rank(), now.outbox_level.rank()) {
        if to > from {
            out.push(Notice::OutboxRose {
                level: now.outbox_level,
                depth: now.outbox_depth,
                planned: now.outbox_planned_depth,
            });
        } else if to == 0 && from > 0 {
            out.push(Notice::OutboxNormal);
        }
    }
    for printer in &now.printers {
        if !was
            .printers
            .iter()
            .any(|p| p.device_id == printer.device_id)
        {
            out.push(Notice::PrinterAdded {
                name: printer.name.clone(),
            });
        }
    }
    for printer in &was.printers {
        if !now
            .printers
            .iter()
            .any(|p| p.device_id == printer.device_id)
        {
            out.push(Notice::PrinterRemoved {
                name: printer.name.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CloudLink, EdgeReading, NotRead, Notice, OutboxLevel, PairingReading, Printer, Snapshot,
        StoreFacts, StoreReading, notices,
    };

    fn facts(
        cloud_link: CloudLink,
        depth: u64,
        level: OutboxLevel,
        printers: &[(&str, &str)],
    ) -> StoreFacts {
        StoreFacts {
            cloud_link,
            outbox_depth: depth,
            outbox_planned_depth: 100_000,
            outbox_level: level,
            printers: printers
                .iter()
                .map(|(id, name)| Printer {
                    device_id: (*id).to_owned(),
                    name: (*name).to_owned(),
                })
                .collect(),
        }
    }

    fn up(store: StoreReading) -> Snapshot {
        Snapshot {
            edge: EdgeReading::Up {
                version: "0.1.0".to_owned(),
            },
            pairing: PairingReading::Paired,
            store,
        }
    }

    fn read(facts: StoreFacts) -> Snapshot {
        up(StoreReading::Read(facts))
    }

    fn not_read(reason: NotRead) -> Snapshot {
        up(StoreReading::NotRead { reason })
    }

    fn down() -> Snapshot {
        Snapshot {
            edge: EdgeReading::Down,
            pairing: PairingReading::Unknown,
            store: StoreReading::NotRead {
                reason: NotRead::EdgeDown,
            },
        }
    }

    fn normal() -> StoreFacts {
        facts(
            CloudLink::Online,
            0,
            OutboxLevel::Normal,
            &[("p1", "Kitchen")],
        )
    }

    #[test]
    fn the_first_reading_is_a_baseline() {
        assert_eq!(notices(&Snapshot::unknown(), &read(normal())), vec![]);
        assert_eq!(notices(&Snapshot::unknown(), &down()), vec![]);
    }

    #[test]
    fn the_edge_going_away_and_coming_back_are_each_one_notice() {
        assert_eq!(notices(&read(normal()), &down()), vec![Notice::EdgeDown]);
        assert_eq!(notices(&down(), &down()), vec![]);
        assert_eq!(
            notices(&down(), &not_read(NotRead::TillClosed)),
            vec![Notice::EdgeBack]
        );
    }

    #[test]
    fn losing_the_pairing_is_news_whenever_it_is_first_seen() {
        let mut lost = not_read(NotRead::NotPaired);
        lost.pairing = PairingReading::Lost;
        assert_eq!(notices(&read(normal()), &lost), vec![Notice::PairingLost]);
        assert_eq!(
            notices(&Snapshot::unknown(), &lost),
            vec![Notice::PairingLost]
        );
        assert_eq!(notices(&lost, &lost), vec![]);
    }

    #[test]
    fn the_cloud_link_is_lost_only_from_online_and_restored_only_from_offline() {
        let offline = facts(
            CloudLink::Offline,
            12,
            OutboxLevel::Normal,
            &[("p1", "Kitchen")],
        );
        let not_tried = facts(
            CloudLink::NotTried,
            0,
            OutboxLevel::Normal,
            &[("p1", "Kitchen")],
        );
        assert_eq!(
            notices(&read(normal()), &read(offline.clone())),
            vec![Notice::CloudLost { waiting: 12 }]
        );
        assert_eq!(
            notices(&read(offline.clone()), &read(normal())),
            vec![Notice::CloudRestored]
        );
        assert_eq!(notices(&read(not_tried.clone()), &read(offline)), vec![]);
        assert_eq!(notices(&read(not_tried), &read(normal())), vec![]);
    }

    #[test]
    fn the_outbox_notifies_on_each_grade_it_rises_past_and_once_on_its_way_back() {
        let at = |level| facts(CloudLink::Offline, 60_000, level, &[("p1", "Kitchen")]);
        let rose = |level| {
            vec![Notice::OutboxRose {
                level,
                depth: 60_000,
                planned: 100_000,
            }]
        };
        assert_eq!(
            notices(
                &read(at(OutboxLevel::Normal)),
                &read(at(OutboxLevel::Elevated))
            ),
            rose(OutboxLevel::Elevated)
        );
        assert_eq!(
            notices(
                &read(at(OutboxLevel::Elevated)),
                &read(at(OutboxLevel::High))
            ),
            rose(OutboxLevel::High)
        );
        // A jump over a grade is one notice, for the grade reached.
        assert_eq!(
            notices(
                &read(at(OutboxLevel::Normal)),
                &read(at(OutboxLevel::Beyond))
            ),
            rose(OutboxLevel::Beyond)
        );
        // Falling from high to elevated is not news; falling back to normal is.
        assert_eq!(
            notices(
                &read(at(OutboxLevel::High)),
                &read(at(OutboxLevel::Elevated))
            ),
            vec![]
        );
        assert_eq!(
            notices(&read(at(OutboxLevel::High)), &read(at(OutboxLevel::Normal))),
            vec![Notice::OutboxNormal]
        );
        // An unknown grade is never compared.
        assert_eq!(
            notices(
                &read(at(OutboxLevel::Unspecified)),
                &read(at(OutboxLevel::High))
            ),
            vec![]
        );
    }

    #[test]
    fn printers_are_news_when_they_join_or_leave_the_published_list() {
        let before = facts(
            CloudLink::Online,
            0,
            OutboxLevel::Normal,
            &[("p1", "Kitchen"), ("p2", "Bar")],
        );
        let after = facts(
            CloudLink::Online,
            0,
            OutboxLevel::Normal,
            &[("p1", "Kitchen (hot)"), ("p3", "Counter")],
        );
        assert_eq!(
            notices(&read(before), &read(after)),
            vec![
                Notice::PrinterAdded {
                    name: "Counter".to_owned()
                },
                Notice::PrinterRemoved {
                    name: "Bar".to_owned()
                },
            ]
        );
    }

    #[test]
    fn nothing_is_compared_across_a_round_that_was_not_read() {
        let offline = facts(CloudLink::Offline, 5, OutboxLevel::High, &[]);
        assert_eq!(
            notices(&read(normal()), &not_read(NotRead::TillClosed)),
            vec![]
        );
        assert_eq!(
            notices(&not_read(NotRead::TillClosed), &read(offline.clone())),
            vec![]
        );
        assert_eq!(
            notices(&not_read(NotRead::SignInNeeded), &read(offline)),
            vec![]
        );
    }

    #[test]
    fn the_edge_tokens_are_read_and_written_as_the_edge_spells_them() {
        assert_eq!(CloudLink::from_wire("CLOUD_LINK_ONLINE"), CloudLink::Online);
        assert_eq!(
            CloudLink::from_wire("CLOUD_LINK_OFFLINE"),
            CloudLink::Offline
        );
        assert_eq!(
            CloudLink::from_wire("CLOUD_LINK_UNSPECIFIED"),
            CloudLink::NotTried
        );
        assert_eq!(CloudLink::from_wire("SOMETHING_NEW"), CloudLink::NotTried);
        for (token, level) in [
            ("OUTBOX_LEVEL_NORMAL", OutboxLevel::Normal),
            ("OUTBOX_LEVEL_ELEVATED", OutboxLevel::Elevated),
            ("OUTBOX_LEVEL_HIGH", OutboxLevel::High),
            ("OUTBOX_LEVEL_BEYOND", OutboxLevel::Beyond),
            ("OUTBOX_LEVEL_SOMETHING_NEW", OutboxLevel::Unspecified),
        ] {
            assert_eq!(OutboxLevel::from_wire(token), level);
        }
        let json = serde_json::to_value(read(normal())).unwrap();
        let at = |pointer: &str| json.pointer(pointer).and_then(serde_json::Value::as_str);
        assert_eq!(at("/edge/state"), Some("EDGE_UP"));
        assert_eq!(at("/pairing"), Some("PAIRING_PAIRED"));
        assert_eq!(at("/store/state"), Some("STORE_READ"));
        assert_eq!(at("/store/cloud_link"), Some("CLOUD_LINK_ONLINE"));
        assert_eq!(at("/store/outbox_level"), Some("OUTBOX_LEVEL_NORMAL"));
        assert_eq!(at("/store/printers/0/name"), Some("Kitchen"));
        let json = serde_json::to_value(not_read(NotRead::TillClosed)).unwrap();
        let at = |pointer: &str| json.pointer(pointer).and_then(serde_json::Value::as_str);
        assert_eq!(at("/store/state"), Some("STORE_NOT_READ"));
        assert_eq!(at("/store/reason"), Some("NOT_READ_TILL_CLOSED"));
    }
}
