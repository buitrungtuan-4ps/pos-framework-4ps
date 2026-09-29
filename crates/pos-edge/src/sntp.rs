// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! SNTP clock-drift monitoring (roadmap-v3 **PF4**).
//!
//! The edge's clock is the OS clock ([`crate::clock`]). An SNTP probe measures the **offset** between
//! it and a reference time server; a large offset means the store's clock has drifted, which is not
//! cosmetic — the business date is derived from the store's local time
//! ([ADR-0014](../../../docs/adr/0014-datetime-library.md)), so a clock two minutes fast can file a
//! sale under the wrong trading day.
//!
//! # Measured, not corrected
//!
//! The probe reports; it does not steer [`SystemClock`]. Correcting the clock is the operating
//! system's job — `systemd-timesyncd`, the Windows Time service — and a second opinion applied inside
//! this process would make every event's `event_time` disagree with the machine's own log, its TLS
//! checks and the installer's clock-skew line (`deploy/edge/README.md`). What the edge adds is the
//! part the OS does not do: telling somebody when its correction is not happening.
//!
//! # Where a drifted clock is raised
//!
//! Through the same path a deep outbox is ([`crate::sync_status`]): a crossing logs once, in the
//! direction it went, and the reading rides `GET /api/sync`, which the till's Devices screen draws.
//! It does not reach the cloud yet: ADR-0073 defers a `ClockDrift` alert until a heartbeat field
//! carries the offset, and that is a cloud change as much as an edge one.
//!
//! # The shape of this module
//!
//! The **decisions** — what an offset is, whether a reply is believable, whether the offset is an
//! alarm — are pure and tested without a network. The **exchange** is one UDP datagram each way
//! ([`probe`]), and [`spawn`] runs it on a fixed interval in its own task. The format is written here
//! rather than taken from a crate, as `docs/architecture.md` Appendix C asks of the small formats:
//! the whole of it is one 48-byte packet.

use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use pos_proto::ClockSource;
use pos_proto::time::Timestamp;
use tokio::net::UdpSocket;

use crate::app::Edge;
use crate::clock::SystemClock;

/// The offset past which the clock is considered adrift and an alarm is raised. Two seconds is well
/// inside the tolerance business-date derivation needs and well outside ordinary NTP jitter.
pub const DRIFT_ALARM: Duration = Duration::from_secs(2);

/// The time server a box measures against when `config.toml` names none.
///
/// Google's public service rather than the NTP Pool: it is anycast, so a store in Ho Chi Minh City
/// and one in Osaka each reach a nearby server under one name, and it invites use by anybody. The
/// Pool asks a product that ships a default to apply for a vendor zone of its own instead. Google
/// smears leap seconds, and the difference from an unsmeared clock peaks at half a second — a
/// quarter of [`DRIFT_ALARM`], so it cannot raise the alarm by itself.
pub const DEFAULT_SNTP_SERVER: &str = "time.google.com";

/// The SNTP port (RFC 4330 §4). Fixed rather than configurable: a time server on another port is
/// not a thing a store has.
pub const SNTP_PORT: u16 = 123;

/// How often the clock is measured.
///
/// A PC's clock left to itself drifts by a second or two a day, so for drift alone hourly would do.
/// A quarter of an hour is for the other failure — a clock stepped by hand, or a machine whose
/// battery died and booted in the past — which should not trade under a wrong date for an hour
/// before anyone is told. It is also a courtesy interval: 96 datagrams a day per store.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// How long one probe — the name lookup and the round trip together — may take before it is
/// abandoned until the next interval.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest round trip an offset is still believed at.
///
/// SNTP assumes the request and the reply took equally long; when they did not, the offset is wrong
/// by up to half the round trip. Refusing a sample slower than a second bounds that error at half a
/// second, so an alarm means the clock is off rather than that the store's uplink was busy.
pub const MAX_ROUND_TRIP: Duration = Duration::from_secs(1);

// The error an accepted sample can carry must be well under the alarm it feeds, or a slow link
// would read as a drifted clock. Asserted rather than remembered, because the two are tuned apart.
const _: () = assert!(
    MAX_ROUND_TRIP.as_millis() / 2 < DRIFT_ALARM.as_millis(),
    "half the longest believed round trip must stay under the drift alarm"
);

/// The length of an SNTP packet without extensions (RFC 4330 §4).
const PACKET_LEN: usize = 48;

/// Leap indicator 0, version 4, mode 3 (client) — the first byte of every request.
const CLIENT_HEADER: u8 = 0b00_100_011;

/// The mode a unicast server answers in (RFC 4330 §4).
const MODE_SERVER: u8 = 4;

/// The leap indicator a server sets when its own clock is not synchronised.
const LEAP_UNSYNCHRONISED: u8 = 3;

/// The first stratum that means "unsynchronised" (RFC 5905 §7.3); stratum 0 is a kiss-o'-death.
const STRATUM_UNSYNCHRONISED: u8 = 16;

/// Seconds from the NTP epoch (1900-01-01) to the Unix epoch (1970-01-01).
const NTP_TO_UNIX_SECONDS: i64 = 2_208_988_800;

/// The assessment of one measured offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drift {
    /// Within tolerance; the offset is reported for the metrics sink.
    Ok {
        /// The measured offset (edge clock minus reference), in milliseconds.
        offset_ms: i64,
    },
    /// Beyond [`DRIFT_ALARM`]; the operator and the cloud should be alerted.
    Alarm {
        /// The measured offset (edge clock minus reference), in milliseconds.
        offset_ms: i64,
    },
}

impl Drift {
    /// The wire token (`CLOCK_DRIFT_*`). A clock nobody has measured is `CLOCK_DRIFT_UNSPECIFIED`,
    /// which is the absence of a `Drift` rather than a variant of one.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Ok { .. } => "CLOCK_DRIFT_OK",
            Self::Alarm { .. } => "CLOCK_DRIFT_ALARM",
        }
    }
}

/// [`DRIFT_ALARM`] in milliseconds, the unit every offset here is in.
#[must_use]
pub fn drift_alarm_ms() -> u64 {
    u64::try_from(DRIFT_ALARM.as_millis()).unwrap_or(u64::MAX)
}

/// Assesses a measured offset (edge clock minus reference), in milliseconds.
#[must_use]
pub fn assess(offset_ms: i64) -> Drift {
    if offset_ms.unsigned_abs() > drift_alarm_ms() {
        Drift::Alarm { offset_ms }
    } else {
        Drift::Ok { offset_ms }
    }
}

/// One measurement of this box's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    /// This box's clock minus the time server's, in milliseconds: positive is fast, negative slow.
    pub offset_ms: i64,
    /// When the request left, on this box's own clock — the clock every other time it reports is
    /// on, so a reader compares like with like.
    pub measure_time: Timestamp,
}

impl ClockReading {
    /// Whether this reading is an alarm.
    #[must_use]
    pub fn drift(&self) -> Drift {
        assess(self.offset_ms)
    }
}

/// One request and its reply, as the four instants RFC 4330 §5 calls T1–T4, in milliseconds since
/// the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "the unit suffix is the naming standard's rule, and all four are milliseconds"
)]
struct Exchange {
    /// T1: this box's clock when the request left.
    edge_sent_ms: i64,
    /// T2: the server's clock when the request arrived.
    server_received_ms: i64,
    /// T3: the server's clock when the reply left.
    server_sent_ms: i64,
    /// T4: this box's clock when the reply arrived.
    edge_received_ms: i64,
}

impl Exchange {
    /// This box's clock minus the server's: `((T1 − T2) + (T4 − T3)) / 2`.
    ///
    /// RFC 4330's θ with the sign turned round, because [`Drift`] has always been "edge minus
    /// reference" — the amount the box is fast by, which is what a person reading it asks.
    const fn offset_ms(&self) -> i64 {
        let outbound = self.edge_sent_ms.saturating_sub(self.server_received_ms);
        let inbound = self.edge_received_ms.saturating_sub(self.server_sent_ms);
        outbound.saturating_add(inbound) / 2
    }

    /// The time on the wire, without the server's own processing: `(T4 − T1) − (T3 − T2)`.
    const fn round_trip_ms(&self) -> i64 {
        let waited = self.edge_received_ms.saturating_sub(self.edge_sent_ms);
        let served = self.server_sent_ms.saturating_sub(self.server_received_ms);
        waited.saturating_sub(served)
    }

    /// The reading this exchange supports, stamped `measure_time`.
    ///
    /// # Errors
    ///
    /// [`ProbeError::RoundTrip`] when the round trip is longer than [`MAX_ROUND_TRIP`] — or
    /// impossibly negative, a server claiming it spent longer on the request than the request took.
    /// A millisecond or two under zero is the rounding of four clocks read to the millisecond, and
    /// passes.
    fn reading(&self, measure_time: Timestamp) -> Result<ClockReading, ProbeError> {
        let round_trip = self.round_trip_ms();
        let limit = i64::try_from(MAX_ROUND_TRIP.as_millis()).unwrap_or(i64::MAX);
        if round_trip.unsigned_abs() > limit.unsigned_abs() {
            return Err(ProbeError::RoundTrip(round_trip));
        }
        Ok(ClockReading {
            offset_ms: self.offset_ms(),
            measure_time,
        })
    }
}

/// Why a probe produced no reading. Nothing here carries personal data; the server's name is
/// configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    /// The name did not resolve, or the datagram could not be sent or received.
    #[error("the time server could not be reached: {0}")]
    Unreachable(String),
    /// Nothing came back within [`PROBE_TIMEOUT`].
    #[error("the time server did not answer within {} seconds", PROBE_TIMEOUT.as_secs())]
    TimedOut,
    /// Something came back and it was not an answer to trust.
    #[error("the time server's reply was not usable: {0}")]
    Unusable(&'static str),
    /// The round trip was too long to believe the offset it gives.
    #[error("a round trip of {0} ms is too long to measure an offset against")]
    RoundTrip(i64),
    /// The OS entropy source, which the request's nonce is drawn from, is unavailable.
    #[error("the OS entropy source is unavailable")]
    Entropy,
}

/// The request: a client header and `nonce` where the transmit timestamp goes, everything else zero.
///
/// A random nonce rather than this box's clock: a server only copies the field back into its reply,
/// so it need not be a time at all. A nonce tells nobody on the path what time this box thinks it
/// is, and it cannot be guessed by someone forging a reply, which a clock reading to the millisecond
/// can.
fn request(nonce: [u8; 8]) -> [u8; PACKET_LEN] {
    let mut packet = [0_u8; PACKET_LEN];
    packet[0] = CLIENT_HEADER;
    packet[40..48].copy_from_slice(&nonce);
    packet
}

/// Reads a reply to the request carrying `nonce`: the server's receive and transmit instants, T2
/// and T3, in milliseconds since the Unix epoch.
///
/// # Errors
///
/// [`ProbeError::Unusable`] for a packet that is too short, is not a server's reply, comes from a
/// server whose own clock is unsynchronised (or that sent a kiss-o'-death), does not answer this
/// request, or carries no time.
fn read_reply(reply: &[u8], nonce: [u8; 8]) -> Result<(i64, i64), ProbeError> {
    let Some(packet) = reply
        .get(..PACKET_LEN)
        .and_then(|bytes| <&[u8; PACKET_LEN]>::try_from(bytes).ok())
    else {
        return Err(ProbeError::Unusable("shorter than an SNTP packet"));
    };
    let header = packet[0];
    let stratum = packet[1];
    if header & 0b111 != MODE_SERVER {
        return Err(ProbeError::Unusable("not a server's reply"));
    }
    if stratum == 0 {
        return Err(ProbeError::Unusable("the server declined to answer"));
    }
    if header >> 6 == LEAP_UNSYNCHRONISED || stratum >= STRATUM_UNSYNCHRONISED {
        return Err(ProbeError::Unusable(
            "the server's own clock is not synchronised",
        ));
    }
    // The originate field is the request's transmit field, copied back. Anything else is a reply to
    // somebody else's request, a stale one, or a forgery.
    if field(packet, 24) != nonce {
        return Err(ProbeError::Unusable("not an answer to this request"));
    }
    let transmitted = field(packet, 40);
    if transmitted == [0; 8] {
        return Err(ProbeError::Unusable("the reply carries no time"));
    }
    Ok((
        ntp_to_unix_ms(field(packet, 32)),
        ntp_to_unix_ms(transmitted),
    ))
}

/// The eight bytes of the timestamp at `at`.
fn field(packet: &[u8; PACKET_LEN], at: usize) -> [u8; 8] {
    packet
        .get(at..at + 8)
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .unwrap_or_default()
}

/// An NTP timestamp — 32 bits of seconds since 1900 and 32 of fraction — as milliseconds since the
/// Unix epoch. Integer arithmetic only.
///
/// The seconds wrap on 2036-02-07. RFC 4330 §3 resolves the era by the top bit: set is 1968–2036,
/// clear is 2036–2104. A box still running after that date reads the right century.
fn ntp_to_unix_ms(timestamp: [u8; 8]) -> i64 {
    let seconds = u32::from_be_bytes([timestamp[0], timestamp[1], timestamp[2], timestamp[3]]);
    let fraction = u32::from_be_bytes([timestamp[4], timestamp[5], timestamp[6], timestamp[7]]);
    let seconds = if seconds & 0x8000_0000 == 0 {
        i64::from(seconds) + (1_i64 << 32)
    } else {
        i64::from(seconds)
    };
    // A fraction of 2³² is one second; this is its whole milliseconds, 0–999.
    let millis = (i64::from(fraction) * 1_000) >> 32;
    (seconds - NTP_TO_UNIX_SECONDS) * 1_000 + millis
}

/// Measures `clock` against the SNTP server at `host`:`port`, once.
///
/// T4 is T1 plus the time the exchange took on the monotonic clock, rather than a second read of
/// `clock`. A clock stepped mid-exchange then measures as the clock it was when the request left,
/// rather than as half the step in the offset and all of it in the round trip.
///
/// # Errors
///
/// [`ProbeError`] when no reading was taken, for the reason it names.
pub async fn probe<C: ClockSource>(
    clock: &C,
    host: &str,
    port: u16,
) -> Result<ClockReading, ProbeError> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_error| ProbeError::Entropy)?;
    let (sent, exchange) = tokio::time::timeout(PROBE_TIMEOUT, exchange(clock, host, port, nonce))
        .await
        .map_err(|_elapsed| ProbeError::TimedOut)??;
    exchange.reading(sent)
}

/// The datagram out and the datagram back.
async fn exchange<C: ClockSource>(
    clock: &C,
    host: &str,
    port: u16,
    nonce: [u8; 8],
) -> Result<(Timestamp, Exchange), ProbeError> {
    let unreachable = |error: std::io::Error| ProbeError::Unreachable(error.to_string());
    // A name lookup is a blocking call underneath; tokio runs it on its blocking pool.
    let server = tokio::net::lookup_host((host, port))
        .await
        .map_err(unreachable)?
        .next()
        .ok_or_else(|| ProbeError::Unreachable(format!("{host} did not resolve")))?;
    let local: SocketAddr = if server.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(local).await.map_err(unreachable)?;
    // Connected, so the kernel drops a datagram from any other address before it reaches here.
    socket.connect(server).await.map_err(unreachable)?;

    let sent = clock.now();
    let started = tokio::time::Instant::now();
    socket.send(&request(nonce)).await.map_err(unreachable)?;
    // Room past 48 bytes for extension fields a server may append; they are not read.
    let mut reply = [0_u8; 2 * PACKET_LEN];
    let length = socket.recv(&mut reply).await.map_err(unreachable)?;
    let elapsed = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);

    let (server_received_ms, server_sent_ms) =
        read_reply(reply.get(..length).unwrap_or_default(), nonce)?;
    let edge_sent_ms = sent.as_milliseconds_since_epoch();
    Ok((
        sent,
        Exchange {
            edge_sent_ms,
            server_received_ms,
            server_sent_ms,
            edge_received_ms: edge_sent_ms.saturating_add(elapsed),
        },
    ))
}

/// What the probe last learned about this box's clock, shared by the probe loop and
/// `GET /api/sync`.
///
/// The last good reading is kept through failed probes: its `measure_time` says how old it is,
/// which is more use to a reader than a blank.
#[derive(Debug, Default)]
pub struct ClockStatus {
    reading: Mutex<Option<ClockReading>>,
    /// Whether the last reading alarmed, so a crossing logs once rather than every probe.
    alarmed: AtomicBool,
    /// Whether the last probe failed, so an unreachable server logs once rather than every interval.
    failing: AtomicBool,
}

impl ClockStatus {
    /// A clock nobody has measured yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            reading: Mutex::new(None),
            alarmed: AtomicBool::new(false),
            failing: AtomicBool::new(false),
        }
    }

    /// The last reading, or `None` if no probe has succeeded since the edge started.
    #[must_use]
    pub fn reading(&self) -> Option<ClockReading> {
        *self.reading.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records one probe's outcome and logs a change, once, in the direction it went.
    pub fn record(&self, outcome: Result<ClockReading, ProbeError>) {
        match outcome {
            Ok(reading) => {
                *self.reading.lock().unwrap_or_else(PoisonError::into_inner) = Some(reading);
                if self.failing.swap(false, Ordering::Relaxed) {
                    tracing::info!("clock: the time server answers again");
                }
                let alarm = matches!(reading.drift(), Drift::Alarm { .. });
                let before = self.alarmed.swap(alarm, Ordering::Relaxed);
                if alarm && !before {
                    tracing::warn!(
                        offset_ms = reading.offset_ms,
                        alarm_ms = drift_alarm_ms(),
                        "clock: this box's clock is off by more than the drift alarm. The trading \
                         day is worked out from it, so a sale near the day's cutoff can be filed \
                         under the wrong day: turn on the operating system's automatic time sync"
                    );
                } else if before && !alarm {
                    tracing::info!(
                        offset_ms = reading.offset_ms,
                        "clock: this box's clock is back within tolerance"
                    );
                } else {
                    tracing::debug!(offset_ms = reading.offset_ms, "clock: measured");
                }
            }
            Err(error) => {
                if self.failing.swap(true, Ordering::Relaxed) {
                    tracing::debug!(%error, "clock: still cannot measure");
                } else {
                    tracing::warn!(
                        %error,
                        "clock: cannot measure this box's clock against the time server, so a \
                         drift goes unreported until it can; the store trades either way"
                    );
                }
            }
        }
    }
}

/// Starts measuring this box's clock against `server` every [`PROBE_INTERVAL`], the first time at
/// once, for as long as the process lives. Each outcome lands on [`Edge::clock_status`].
///
/// Its own task, and nothing in it blocks: the exchange is async UDP, and the name lookup runs on
/// tokio's blocking pool. A failed probe is logged and never fatal — the store trades either way.
///
/// An empty `server` measures nothing, which is how `sntp_server = ""` turns it off. That is
/// announced as a warning rather than accepted quietly, as a store with archiving off is.
pub fn spawn<S>(edge: Arc<Edge<S>>, server: &str) -> Option<tokio::task::JoinHandle<()>>
where
    S: Send + Sync + 'static,
{
    let server = server.trim().to_owned();
    if server.is_empty() {
        tracing::warn!(
            "sntp_server is empty: this box's clock is not measured, so a drifting clock goes \
             unreported"
        );
        return None;
    }
    Some(tokio::spawn(async move {
        let mut ticks = tokio::time::interval(PROBE_INTERVAL);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            edge.clock_status()
                .record(probe(&SystemClock, &server, SNTP_PORT).await);
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use pos_fakes::FakeClock;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("a valid instant")
    }

    /// Unix milliseconds as an NTP timestamp, the inverse of `ntp_to_unix_ms` for this era.
    fn ntp(unix_ms: i64) -> [u8; 8] {
        let seconds = unix_ms.div_euclid(1_000) + NTP_TO_UNIX_SECONDS;
        // Rounded up, so reading it back truncates to the same millisecond rather than the one before.
        let fraction = ((unix_ms.rem_euclid(1_000) << 32) + 999) / 1_000;
        let mut bytes = [0_u8; 8];
        bytes[..4].copy_from_slice(&u32::try_from(seconds).expect("era 0").to_be_bytes());
        bytes[4..].copy_from_slice(&u32::try_from(fraction).expect("a fraction").to_be_bytes());
        bytes
    }

    /// A server's reply: `header`, `stratum`, and the three timestamps a client reads.
    fn reply(header: u8, stratum: u8, originate: [u8; 8], received: i64, sent: i64) -> Vec<u8> {
        let mut packet = vec![0_u8; PACKET_LEN];
        packet[0] = header;
        packet[1] = stratum;
        packet[24..32].copy_from_slice(&originate);
        packet[32..40].copy_from_slice(&ntp(received));
        packet[40..48].copy_from_slice(&ntp(sent));
        packet
    }

    /// Leap indicator 0, version 4, mode 4.
    const SERVER_HEADER: u8 = 0b00_100_100;
    const NONCE: [u8; 8] = [7, 1, 8, 2, 8, 1, 8, 2];
    /// 2026-09-29T00:00:00Z.
    const NOW_MS: i64 = 1_790_640_000_000;

    #[test]
    fn a_small_offset_either_way_is_within_tolerance() {
        assert_eq!(assess(0), Drift::Ok { offset_ms: 0 });
        assert_eq!(assess(1_999), Drift::Ok { offset_ms: 1_999 });
        assert_eq!(assess(-1_999), Drift::Ok { offset_ms: -1_999 });
    }

    #[test]
    fn an_offset_past_two_seconds_either_way_alarms() {
        assert_eq!(assess(2_001), Drift::Alarm { offset_ms: 2_001 });
        assert_eq!(assess(-60_000), Drift::Alarm { offset_ms: -60_000 });
    }

    #[test]
    fn exactly_the_threshold_is_not_yet_an_alarm() {
        assert_eq!(assess(2_000), Drift::Ok { offset_ms: 2_000 });
    }

    #[test]
    fn the_offset_is_how_far_ahead_this_box_is() {
        // The box is three seconds fast; 50 ms each way on the wire, 1 ms inside the server.
        let fast = Exchange {
            edge_sent_ms: 13_000,
            server_received_ms: 10_050,
            server_sent_ms: 10_051,
            edge_received_ms: 13_101,
        };
        assert_eq!(fast.offset_ms(), 3_000);
        assert_eq!(fast.round_trip_ms(), 100);

        // And two and a half seconds slow, the same path.
        let slow = Exchange {
            edge_sent_ms: 7_500,
            server_received_ms: 10_050,
            server_sent_ms: 10_051,
            edge_received_ms: 7_601,
        };
        assert_eq!(slow.offset_ms(), -2_500);
        assert_eq!(slow.round_trip_ms(), 100);
    }

    #[test]
    fn an_uneven_path_errs_by_at_most_half_the_round_trip() {
        // A right clock, an instant request and a reply 400 ms late: SNTP cannot tell that from a
        // clock 200 ms fast, and says so by the bound, not by the answer.
        let lopsided = Exchange {
            edge_sent_ms: 0,
            server_received_ms: 0,
            server_sent_ms: 0,
            edge_received_ms: 400,
        };
        assert_eq!(lopsided.offset_ms(), 200);
        assert_eq!(lopsided.offset_ms(), lopsided.round_trip_ms() / 2);
    }

    #[test]
    fn a_slow_round_trip_is_refused_rather_than_read_as_drift() {
        let measured = at(NOW_MS);
        let within = Exchange {
            edge_sent_ms: 0,
            server_received_ms: 0,
            server_sent_ms: 0,
            edge_received_ms: 1_000,
        };
        assert_eq!(
            within.reading(measured),
            Ok(ClockReading {
                offset_ms: 500,
                measure_time: measured
            })
        );
        let too_slow = Exchange {
            edge_received_ms: 1_001,
            ..within
        };
        assert_eq!(
            too_slow.reading(measured),
            Err(ProbeError::RoundTrip(1_001))
        );
        // A server that claims it held the request longer than the request took is not a clock.
        let impossible = Exchange {
            server_sent_ms: 5_000,
            ..within
        };
        assert_eq!(
            impossible.reading(measured),
            Err(ProbeError::RoundTrip(-4_000))
        );
    }

    #[test]
    fn a_request_is_a_client_header_and_the_nonce() {
        let packet = request(NONCE);
        assert_eq!(packet[0], 0x23, "leap 0, version 4, mode 3");
        assert_eq!(&packet[40..48], &NONCE);
        assert!(
            packet[1..40].iter().all(|byte| *byte == 0),
            "nothing else is sent"
        );
    }

    #[test]
    fn a_reply_to_this_request_yields_the_server_s_two_instants() {
        let packet = reply(SERVER_HEADER, 2, NONCE, NOW_MS, NOW_MS + 3);
        assert_eq!(read_reply(&packet, NONCE), Ok((NOW_MS, NOW_MS + 3)));
    }

    #[test]
    fn a_reply_that_is_not_an_answer_to_trust_is_refused() {
        let good = reply(SERVER_HEADER, 2, NONCE, NOW_MS, NOW_MS);
        let refused = |packet: &[u8]| read_reply(packet, NONCE).is_err();

        assert!(refused(&good[..47]), "short");
        assert!(
            refused(&reply(0b00_100_011, 2, NONCE, NOW_MS, NOW_MS)),
            "a client's packet"
        );
        assert!(
            refused(&reply(0b11_100_100, 2, NONCE, NOW_MS, NOW_MS)),
            "leap: unsynchronised"
        );
        assert!(
            refused(&reply(SERVER_HEADER, 0, NONCE, NOW_MS, NOW_MS)),
            "kiss-o'-death"
        );
        assert!(
            refused(&reply(SERVER_HEADER, 16, NONCE, NOW_MS, NOW_MS)),
            "stratum 16"
        );
        assert!(
            refused(&reply(SERVER_HEADER, 2, [0; 8], NOW_MS, NOW_MS)),
            "someone else's"
        );
        let mut no_time = good.clone();
        no_time[40..48].fill(0);
        assert!(refused(&no_time), "no transmit time");
        // And a longer packet — extension fields — is still read.
        let mut extended = good;
        extended.extend_from_slice(&[0; 20]);
        assert!(read_reply(&extended, NONCE).is_ok());
    }

    #[test]
    fn an_ntp_timestamp_reads_to_the_millisecond_and_across_the_2036_wrap() {
        let unix_epoch = NTP_TO_UNIX_SECONDS.to_be_bytes();
        let mut timestamp = [0_u8; 8];
        timestamp[..4].copy_from_slice(&unix_epoch[4..]);
        assert_eq!(ntp_to_unix_ms(timestamp), 0);
        timestamp[4] = 0x80;
        assert_eq!(ntp_to_unix_ms(timestamp), 500, "half a second");
        assert_eq!(ntp_to_unix_ms(ntp(NOW_MS + 999)), NOW_MS + 999);
        // Seconds zero with the top bit clear is 2036-02-07T06:28:16Z, not 1900.
        assert_eq!(ntp_to_unix_ms([0; 8]), 2_085_978_496_000);
    }

    #[test]
    fn a_crossing_is_recorded_and_a_failure_keeps_the_last_reading() {
        let status = ClockStatus::new();
        assert_eq!(status.reading(), None);

        let fast = ClockReading {
            offset_ms: 2_500,
            measure_time: at(NOW_MS),
        };
        status.record(Ok(fast));
        assert_eq!(status.reading(), Some(fast));
        assert!(status.alarmed.load(Ordering::Relaxed));

        // An unreachable server says nothing new about the clock, so the last word stands.
        status.record(Err(ProbeError::TimedOut));
        assert_eq!(status.reading(), Some(fast));
        assert!(status.failing.load(Ordering::Relaxed));

        let fixed = ClockReading {
            offset_ms: -40,
            measure_time: at(NOW_MS + 900_000),
        };
        status.record(Ok(fixed));
        assert_eq!(status.reading(), Some(fixed));
        assert!(!status.alarmed.load(Ordering::Relaxed));
        assert!(!status.failing.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn a_probe_measures_a_clock_against_a_server_on_this_machine() {
        // A time server on loopback that is five seconds behind the box's (fake) clock, so the whole
        // exchange — request, nonce, decode, offset — is exercised with nothing leaving the machine.
        let server = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        let port = server.local_addr().expect("an address").port();
        let reference_ms = NOW_MS - 5_000;
        tokio::spawn(async move {
            let mut packet = [0_u8; PACKET_LEN];
            let (_, from) = server.recv_from(&mut packet).await.expect("a request");
            let mut originate = [0_u8; 8];
            originate.copy_from_slice(&packet[40..48]);
            let answer = reply(SERVER_HEADER, 1, originate, reference_ms, reference_ms);
            server.send_to(&answer, from).await.expect("a reply");
        });

        let clock = FakeClock::new(at(NOW_MS));
        let reading = probe(&clock, "127.0.0.1", port).await.expect("a reading");
        assert_eq!(reading.measure_time, at(NOW_MS));
        // Five seconds, plus at most half of however long loopback took.
        assert!(
            (5_000..=5_010).contains(&reading.offset_ms),
            "got {}",
            reading.offset_ms
        );
        assert_eq!(reading.drift().as_wire(), "CLOCK_DRIFT_ALARM");
    }
}
