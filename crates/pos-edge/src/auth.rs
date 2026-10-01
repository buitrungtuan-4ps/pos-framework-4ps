// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Offline user authentication: a PIN, verified locally, with a lockout (ADR-0030).
//!
//! The cloud syncs each employee's **Argon2id** PIN hash to the edge as configuration
//! ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md)); the edge verifies a PIN against
//! that hash with no network at all ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)).
//! Because a PIN is short, the Argon2id cost **and a local lockout** — by default, five consecutive
//! failures lock an employee out for five minutes — are the brute-force defence, not the PIN's
//! entropy. A store may set its own numbers in its `session` node
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)),
//! within bounds that never switch the lockout off ([`LockoutPolicy`]).
//!
//! # No secrets in logs
//!
//! A PIN and its hash are secrets: they never enter a log, a span, an event, or the fan-out. Only the
//! employee id (an identifier, not PII) and the outcome are observable. See
//! [`crate::telemetry`].
//!
//! # Why the lockout is separable from the hashing
//!
//! [`Lockout::record`] is a pure state machine over `(employee, verified, now, policy)` — no Argon2,
//! no clock of its own, and the store's numbers passed in rather than held — so the window is
//! unit-tested in microseconds with a fixed [`Timestamp`]. [`verify_pin`] is the thin Argon2
//! wrapper. [`Lockout::authenticate`] combines them.

use core::fmt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use argon2::Argon2;
use argon2::password_hash::PasswordVerifier;
use pos_ports::device_registry::DeviceSession;
use pos_ports::error::PortError;
use pos_proto::ids::{DeviceId, EmployeeId};
use pos_proto::session::PublishedSession;

use crate::app::Edge;
use crate::durable_auth::DurableAuth;
use pos_proto::time::Timestamp;

/// The `tracing` target every event in this module carries.
///
/// Named rather than spelled out at the reader's end, so that
/// [`crate::telemetry::EXCLUDED_TARGETS`] — which keeps this module's per-employee sign-in,
/// wrong-PIN and lockout stream out of the durable log file (ADR-0117 decision 6) — cannot go stale
/// if the module moves.
pub(crate) const LOG_TARGET: &str = module_path!();

/// How many consecutive wrong PINs lock a person out, and for how long: the store's own numbers from
/// its `session` node ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)),
/// read afresh from the live session at every attempt.
///
/// The node's accessors hold each number to bounds that keep the lockout on — at least three
/// attempts and at least a minute — so no publish, and no value an edge cannot read, switches it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockoutPolicy {
    /// Consecutive wrong PINs that trigger a lockout. Never zero.
    pub max_failures: u32,
    /// How long a locked-out employee must wait. Never zero.
    pub lockout: Duration,
}

impl LockoutPolicy {
    /// The numbers a store's `session` node sets, and the defaults for any it does not.
    #[must_use]
    pub fn of(settings: &PublishedSession) -> Self {
        Self {
            max_failures: settings.lockout_attempts(),
            lockout: Duration::from_secs(u64::from(settings.lockout_minutes()).saturating_mul(60)),
        }
    }
}

impl Default for LockoutPolicy {
    /// Five wrong PINs, five minutes: what every store ran before the `session` node existed.
    fn default() -> Self {
        Self::of(&PublishedSession::default())
    }
}

/// The outcome of a sign-in attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    /// The PIN matched; the failure count is now reset.
    Ok,
    /// Wrong PIN, with how many attempts remain before a lockout.
    Wrong {
        /// Attempts left before the next failure locks the employee out.
        remaining: u32,
    },
    /// The employee is locked out until this instant (milliseconds since the Unix epoch). The UI
    /// shows a countdown rather than rejecting silently (ADR-0030).
    LockedOut {
        /// When the lockout ends, in milliseconds since the Unix epoch.
        until_ms: i64,
    },
}

/// Verifies a PIN against a stored Argon2id PHC hash.
///
/// A malformed stored hash verifies nothing — it returns `false` rather than erroring, because a
/// corrupt synced hash must not become a way in.
#[must_use]
pub fn verify_pin(phc_hash: &str, pin: &str) -> bool {
    // `PasswordVerifier<str>` parses the PHC string itself, so a malformed one is an `Err` here
    // and becomes `false` — a corrupt synced hash must not become a way in.
    Argon2::default()
        .verify_password(pin.as_bytes(), phc_hash)
        .is_ok()
}

/// Hashes a PIN into an Argon2id PHC string, for a fixture that needs a roster it can sign into.
///
/// Behind the `demo-fixtures` feature, and enabled by no shipped binary: a real store's PIN hashes
/// are computed in the cloud and synced down as configuration (ADR-0004, ADR-0030), so the edge has
/// only ever needed [`verify_pin`]. `examples/minimal-edge` enables the feature because a demo store
/// has no cloud to publish a roster from, and without one no sign-in can succeed.
///
/// `None` if the OS entropy source needed for the salt is unavailable — a salt is never faked, for
/// the same reason a pairing code is not ([`crate::pairing::Pairing::mint`]).
#[cfg(feature = "demo-fixtures")]
#[must_use]
pub fn hash_pin(pin: &str) -> Option<String> {
    use argon2::password_hash::PasswordHasher as _;
    use argon2::password_hash::phc::PasswordHash;

    // No salt argument: `hash_password` draws a fresh one from the OS per call. A fixed salt here
    // would make every employee's PIN hash to the same string for the same PIN, which is the whole
    // point of a salt — so this call deliberately does not take one.
    let hash: PasswordHash = Argon2::default().hash_password(pin.as_bytes()).ok()?;
    Some(hash.to_string())
}

/// Per-employee failure tracking for the offline PIN lockout.
///
/// One instance lives in the edge's application state. It holds counts only — never a PIN or a hash.
#[derive(Debug, Default)]
pub struct Lockout {
    state: Mutex<HashMap<EmployeeId, Record>>,
}

/// One employee's failure state.
#[derive(Debug, Default, Clone, Copy)]
struct Record {
    /// Consecutive failures since the last success or served lockout.
    failures: u32,
    /// When the current lockout ends, if any (ms since epoch).
    locked_until_ms: Option<i64>,
}

impl Lockout {
    /// A fresh tracker with no failures recorded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the outcome of a verification and returns what the caller may do.
    ///
    /// Pure but for the internal map: given whether the PIN `verified`, the current instant `now`
    /// and the store's `policy`, it advances the lockout state machine. A correct PIN while locked
    /// out is still refused — serving the lockout is the point.
    ///
    /// The policy is the one in force at this attempt, so a store that changes its numbers changes
    /// them from the next attempt: the failures already counted are weighed against the new number,
    /// and a lockout already running keeps the end it was given.
    pub fn record(
        &self,
        employee: EmployeeId,
        verified: bool,
        now: Timestamp,
        policy: LockoutPolicy,
    ) -> SignIn {
        let now_ms = now.as_milliseconds_since_epoch();
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let record = guard.entry(employee).or_default();

        // A lockout that has elapsed is cleared before this attempt is judged.
        if let Some(until) = record.locked_until_ms
            && now_ms >= until
        {
            record.locked_until_ms = None;
            record.failures = 0;
        }

        if let Some(until) = record.locked_until_ms {
            return SignIn::LockedOut { until_ms: until };
        }

        if verified {
            *record = Record::default();
            return SignIn::Ok;
        }

        record.failures = record.failures.saturating_add(1);
        if record.failures >= policy.max_failures {
            let lockout_ms = i64::try_from(policy.lockout.as_millis()).unwrap_or(i64::MAX);
            let until = now_ms.saturating_add(lockout_ms);
            record.locked_until_ms = Some(until);
            SignIn::LockedOut { until_ms: until }
        } else {
            SignIn::Wrong {
                remaining: policy.max_failures.saturating_sub(record.failures),
            }
        }
    }

    /// Verifies `pin` against `phc_hash` and records the outcome under the store's lockout `policy`.
    pub fn authenticate(
        &self,
        employee: EmployeeId,
        phc_hash: &str,
        pin: &str,
        now: Timestamp,
        policy: LockoutPolicy,
    ) -> SignIn {
        // A locked-out employee is refused before the (costly) Argon2 verification even runs.
        {
            let now_ms = now.as_milliseconds_since_epoch();
            let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(record) = guard.get_mut(&employee)
                && let Some(until) = record.locked_until_ms
                && now_ms < until
            {
                return SignIn::LockedOut { until_ms: until };
            }
        }
        self.record(employee, verify_pin(phc_hash, pin), now, policy)
    }
}

/// How long a signed-in device may sit idle before its sign-in stops counting
/// ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)).
///
/// Thirty minutes: long enough that a reboot, a shift-change lull or a quiet hour does not sign
/// anyone out mid-service; short enough that a device which left the store is not still trading as
/// that person the next day. A store sets its own window with the `session.sign_in_idle_timeout_minutes`
/// setting ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md));
/// one that does not runs the local file's deprecated value, if it sets one
/// ([`EdgeConfig::sign_in_idle_timeout`](crate::EdgeConfig::sign_in_idle_timeout)), and this
/// otherwise. The same thirty as
/// [`DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES`](pos_proto::session::DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES),
/// which a test holds it to.
pub const DEFAULT_SIGN_IN_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How far `last_seen_at` must move before it is written through to the registry.
///
/// A sign-in is touched on **every** authenticated request, and writing each one would put a SQLite
/// round-trip on the hot path for no benefit: the value is only ever compared against a
/// thirty-minute window. Flushing at a minute's granularity bounds the writes to about one per
/// device per minute, and bounds the error a restart can introduce to under a minute — nothing
/// against the window it feeds.
pub const LAST_SEEN_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Whether a sign-in last seen at `last_seen` has gone idle by `now`.
///
/// # Fails closed on a clock that misbehaves
///
/// Worth being explicit, because it is easy to assume otherwise: **the edge measures its clock but
/// never corrects it.** The SNTP probe ([`crate::sntp`]) reports drift and leaves setting the time
/// to the operating system. So the clock behind this comparison is the host OS clock, which a
/// system NTP daemon or a person with `date` can step at any moment, in either direction.
///
/// So: a **negative** interval (the clock went backwards, or the row was written by a box whose
/// clock was ahead) and an **implausibly large** one both expire the session. A clock that jumps
/// must never be a way to hold a session open — the same fail-closed posture ADR-0030 took for the
/// PIN lockout, whose stored `until_ms` has the mirror-image problem.
#[must_use]
pub fn has_gone_idle(last_seen: Timestamp, now: Timestamp, timeout: Duration) -> bool {
    let elapsed_ms = now
        .as_milliseconds_since_epoch()
        .saturating_sub(last_seen.as_milliseconds_since_epoch());
    // Backwards in time: refuse rather than treat it as "just seen".
    if elapsed_ms < 0 {
        return true;
    }
    let Ok(timeout_ms) = i64::try_from(timeout.as_millis()) else {
        // A timeout that does not fit in an i64 is a misconfiguration, not a licence to never
        // expire.
        return true;
    };
    elapsed_ms >= timeout_ms
}

/// Who is signed in on each paired device (S0b, [ADR-0084](../../../docs/adr/0084-device-authentication.md)),
/// durable across a restart (S0d, [ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)).
///
/// Device authentication ([`crate::pairing`]) proves *which tablet* is talking to the edge; this
/// records *which employee* is acting on it, so a command runs under a real
/// [`Actor`](pos_core::decision::Actor) rather than a placeholder. A device signs one employee in at
/// a time; signing another in replaces the first, and signing out clears the binding.
///
/// # Durable, guarded by an idle timeout
///
/// Reads answer from memory and every change writes through to the [`DurableAuth`] registry, exactly
/// as [`Pairing`](crate::pairing::Pairing) does — so a power blip or an OTA install no longer makes
/// every member of staff re-enter a PIN mid-service. The cost of that is a till carried off while
/// signed in as a manager, and [`has_gone_idle`] is what bounds it: past the window, the binding is
/// reported as absent and the device gets the same `403` an unsigned one gets.
///
/// With no registry ([`Sessions::new`]) this is memory-only, as it was before S0d.
///
/// # Whose window
///
/// The store's, when its `session` node sets one
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)):
/// [`Sessions::following`] points this at the live configuration, which it reads at every check, so
/// a window the console publishes applies from the next request without a restart. A store whose
/// node sets none runs the fallback it was built with — the local file's deprecated value or
/// [`DEFAULT_SIGN_IN_IDLE_TIMEOUT`].
///
/// It holds identifiers only — never a PIN or a hash.
pub struct Sessions {
    by_device: Mutex<HashMap<DeviceId, DeviceSession>>,
    registry: Option<Arc<dyn DurableAuth>>,
    /// How long a device may idle when the store's configuration sets no window: the local file's
    /// deprecated value, or thirty minutes.
    fallback_idle_timeout: Duration,
    /// Where the store's published `session` node is read from, when this follows one.
    settings: Option<Arc<dyn SessionSettingsSource>>,
}

/// Where [`Sessions`] reads the store's `session` node from: the live configuration, which the
/// config-pull loop swaps while the store trades.
///
/// A seam rather than an `Arc<Edge<S>>`, so `Sessions` carries no store type parameter — the same
/// shape as [`DurableAuth`].
pub trait SessionSettingsSource: Send + Sync {
    /// The `session` node the store is running now.
    fn session_settings(&self) -> PublishedSession;
}

impl<S: Send + Sync> SessionSettingsSource for Edge<S> {
    fn session_settings(&self) -> PublishedSession {
        self.session().session_settings
    }
}

/// The window a `session` node sets, if it sets one.
#[must_use]
pub fn published_idle_timeout(settings: &PublishedSession) -> Option<Duration> {
    settings
        .sign_in_idle_timeout_minutes()
        .map(|minutes| Duration::from_secs(u64::from(minutes).saturating_mul(60)))
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "`by_device` is omitted deliberately: a Debug that enumerated who is signed in across \
              the floor is an unnecessary thing to find in a log. The count stands in for it."
)]
impl fmt::Debug for Sessions {
    /// A count and the policy, never who is signed in: employee ids are identifiers, but a `{:?}`
    /// that enumerated the floor's staff would be an unnecessary thing to find in a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sessions")
            .field("signed_in", &self.count())
            .field("idle_timeout", &self.idle_timeout())
            .field("durable", &self.registry.is_some())
            .finish()
    }
}

impl Default for Sessions {
    /// [`Sessions::new`]. Written out rather than derived, because a derived default would hold a
    /// zero window, which signs every device out between its own two requests.
    fn default() -> Self {
        Self::new()
    }
}

impl Sessions {
    /// No device signed in, in memory only, with the default idle timeout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_device: Mutex::new(HashMap::new()),
            registry: None,
            fallback_idle_timeout: DEFAULT_SIGN_IN_IDLE_TIMEOUT,
            settings: None,
        }
    }

    /// Sign-in state that survives a restart, expiring a device idle past `idle_timeout` — or past
    /// the store's own window, once it [follows](Self::following) a configuration that sets one.
    #[must_use]
    pub fn durable(registry: Arc<dyn DurableAuth>, idle_timeout: Duration) -> Self {
        Self {
            by_device: Mutex::new(HashMap::new()),
            registry: Some(registry),
            fallback_idle_timeout: idle_timeout,
            settings: None,
        }
    }

    /// Takes the idle window from the store's `session` node whenever it sets one, read from
    /// `settings` at every check, and falls back to the window this was built with when it does not.
    #[must_use]
    pub fn following(mut self, settings: Arc<dyn SessionSettingsSource>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Refills the map from the registry at boot. Returns how many sign-ins were restored,
    /// **counting only those still inside the idle window** — a device that was signed in and then
    /// sat untouched past the timeout comes back signed out, which is the whole point of the window.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the registry cannot be read.
    pub async fn load(&self, now: Timestamp) -> Result<usize, PortError> {
        let Some(registry) = self.registry.as_ref() else {
            return Ok(0);
        };
        let sessions = registry.sign_ins().await?;
        let idle_timeout = self.idle_timeout();
        let mut by_device = self
            .by_device
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        by_device.clear();
        for session in sessions {
            if !has_gone_idle(session.last_seen_at, now, idle_timeout) {
                by_device.insert(session.device_id, session);
            }
        }
        Ok(by_device.len())
    }

    /// Binds `device` to the signed-in `employee`, replacing any earlier sign-in on that device.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the sign-in could not be recorded durably. Recorded **before** it is
    /// believed in memory, so a failure refuses the sign-in rather than granting one that the next
    /// restart forgets — a person who is told they are signed in and then silently is not would
    /// discover it mid-sale.
    pub async fn sign_in(
        &self,
        device: DeviceId,
        employee: EmployeeId,
        now: Timestamp,
    ) -> Result<(), PortError> {
        let session = DeviceSession {
            device_id: device,
            employee_id: employee,
            signed_in_at: now,
            last_seen_at: now,
        };
        if let Some(registry) = self.registry.as_ref() {
            registry.record_sign_in(session).await?;
        }
        self.by_device
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(device, session);
        Ok(())
    }

    /// Clears the sign-in on `device`, if any. Idempotent — signing out a device that is not signed
    /// in is a no-op, not an error.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the registry could not be written. Memory is cleared **regardless**, and
    /// this is the one place the order is deliberately the other way round: a sign-out that half
    /// fails must leave the device signed *out* on this box, because the alternative is a till the
    /// operator believes is locked and is not.
    pub async fn sign_out(&self, device: DeviceId) -> Result<(), PortError> {
        self.by_device
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&device);
        if let Some(registry) = self.registry.as_ref() {
            registry.clear_sign_in(device).await?;
        }
        Ok(())
    }

    /// The employee signed in on `device`, or `None` if nobody is **or the device has gone idle** —
    /// which is what the command gate (ADR-0084) turns into a `403`.
    ///
    /// Reads memory only: the idle rule is a comparison against the stored instant, so this needs no
    /// database and stays on the request path.
    #[must_use]
    pub fn employee_for(&self, device: DeviceId, now: Timestamp) -> Option<EmployeeId> {
        let held = self
            .by_device
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&device)
            .copied()?;
        // Read after the map's lock is released, so the two locks are never held together.
        (!has_gone_idle(held.last_seen_at, now, self.idle_timeout())).then_some(held.employee_id)
    }

    /// Records that `device` was just heard from, so it does not go idle while in use.
    ///
    /// Memory always; the registry only once the stored value is [`LAST_SEEN_FLUSH_INTERVAL`]
    /// behind, so the gate does not put a write on every request. Returns whether a flush is due,
    /// which the caller performs — keeping this method synchronous means the gate can call it
    /// without awaiting.
    pub fn touch(&self, device: DeviceId, now: Timestamp) -> bool {
        let mut by_device = self
            .by_device
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(held) = by_device.get_mut(&device) else {
            // Touching creates nothing: a touch must never be a way to sign a device in.
            return false;
        };
        let flushed_ms = held.last_seen_at.as_milliseconds_since_epoch();
        held.last_seen_at = now;
        let behind = now.as_milliseconds_since_epoch().saturating_sub(flushed_ms);
        let due = i64::try_from(LAST_SEEN_FLUSH_INTERVAL.as_millis()).unwrap_or(i64::MAX);
        self.registry.is_some() && behind >= due
    }

    /// Writes a device's `last_seen_at` through to the registry. Called when [`Self::touch`] says a
    /// flush is due.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the registry could not be written. A failed flush is **not** fatal to the
    /// request: the in-memory value has already moved, so the device keeps working, and the only
    /// cost is that a restart in the next minute sees a slightly stale instant.
    pub async fn flush_last_seen(&self, device: DeviceId, now: Timestamp) -> Result<(), PortError> {
        if let Some(registry) = self.registry.as_ref() {
            registry.touch_session(device, now).await?;
        }
        Ok(())
    }

    /// How long a device may idle before its sign-in stops counting: the store's published window
    /// when it sets one, and the fallback this was built with otherwise.
    #[must_use]
    pub fn idle_timeout(&self) -> Duration {
        self.published_idle_timeout()
            .unwrap_or(self.fallback_idle_timeout)
    }

    /// The window the store's `session` node sets, or `None` when this follows no configuration or
    /// the node sets none.
    #[must_use]
    pub fn published_idle_timeout(&self) -> Option<Duration> {
        self.settings
            .as_ref()
            .and_then(|settings| published_idle_timeout(&settings.session_settings()))
    }

    /// Whether sign-ins survive a restart.
    #[must_use]
    pub fn is_durable(&self) -> bool {
        self.registry.is_some()
    }

    /// How many devices are signed in, ignoring the idle window — for [`fmt::Debug`].
    fn count(&self) -> usize {
        self.by_device
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    /// A fixed 16-byte salt, so a hashed fixture is deterministic. Tests only.
    const SALT: &[u8] = b"a-fixed-test-slt";

    use super::{Lockout, LockoutPolicy, SignIn, verify_pin};
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    use pos_proto::ids::EmployeeId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid instant")
    }

    fn employee() -> EmployeeId {
        EmployeeId::new(Ulid::from_u128(7))
    }

    /// The numbers every store ran before the `session` node: five wrong PINs, five minutes.
    fn usual() -> LockoutPolicy {
        LockoutPolicy::default()
    }

    const MAX_FAILURES: u32 = 5;

    /// A real Argon2id PHC hash of `pin`, computed with a fixed salt so the test needs no RNG.
    fn hash_of(pin: &str) -> String {
        Argon2::default()
            .hash_password_with_salt(pin.as_bytes(), SALT)
            .expect("hash")
            .to_string()
    }

    #[test]
    fn a_correct_pin_verifies_and_a_wrong_one_does_not() {
        let hash = hash_of("2468");
        assert!(verify_pin(&hash, "2468"));
        assert!(!verify_pin(&hash, "1357"));
    }

    #[test]
    fn a_malformed_stored_hash_is_never_a_way_in() {
        assert!(!verify_pin("not-a-phc-string", "2468"));
        assert!(!verify_pin("", ""));
    }

    #[test]
    fn five_failures_lock_the_employee_out_for_five_minutes() {
        let lockout = Lockout::new();
        let who = employee();

        // Four wrong attempts count down without locking.
        for expected_remaining in (1..MAX_FAILURES).rev() {
            match lockout.record(who, false, at(0), usual()) {
                SignIn::Wrong { remaining } => assert_eq!(remaining, expected_remaining),
                other => panic!("expected Wrong, got {other:?}"),
            }
        }
        // The fifth locks out, five minutes from now.
        match lockout.record(who, false, at(1_000), usual()) {
            SignIn::LockedOut { until_ms } => assert_eq!(until_ms, 1_000 + 5 * 60 * 1_000),
            other => panic!("expected LockedOut, got {other:?}"),
        }
    }

    #[test]
    fn a_correct_pin_while_locked_out_is_still_refused() {
        let lockout = Lockout::new();
        let who = employee();
        for _ in 0..MAX_FAILURES {
            let _ = lockout.record(who, false, at(0), usual());
        }
        // Even a correct PIN one second later is refused: the lockout must be served.
        assert!(matches!(
            lockout.record(who, true, at(1_000), usual()),
            SignIn::LockedOut { .. }
        ));
    }

    #[test]
    fn the_lockout_lifts_after_the_window_and_the_count_resets() {
        let lockout = Lockout::new();
        let who = employee();
        for _ in 0..MAX_FAILURES {
            let _ = lockout.record(who, false, at(0), usual());
        }
        // Just past five minutes, a correct PIN is accepted again.
        let after = 5 * 60 * 1_000 + 1;
        assert_eq!(lockout.record(who, true, at(after), usual()), SignIn::Ok);
        // And the counter reset: one failure now reports four remaining, not zero.
        assert_eq!(
            lockout.record(who, false, at(after + 1), usual()),
            SignIn::Wrong {
                remaining: MAX_FAILURES - 1
            }
        );
    }

    #[test]
    fn a_success_resets_the_failure_count() {
        let lockout = Lockout::new();
        let who = employee();
        let _ = lockout.record(who, false, at(0), usual());
        let _ = lockout.record(who, false, at(0), usual());
        assert_eq!(lockout.record(who, true, at(0), usual()), SignIn::Ok);
        // Back to a full allowance.
        assert_eq!(
            lockout.record(who, false, at(0), usual()),
            SignIn::Wrong {
                remaining: MAX_FAILURES - 1
            }
        );
    }

    #[test]
    fn authenticate_combines_verification_and_lockout() {
        let lockout = Lockout::new();
        let who = employee();
        let hash = hash_of("2468");

        assert_eq!(
            lockout.authenticate(who, &hash, "2468", at(0), usual()),
            SignIn::Ok
        );
        assert_eq!(
            lockout.authenticate(who, &hash, "0000", at(0), usual()),
            SignIn::Wrong {
                remaining: MAX_FAILURES - 1
            }
        );
    }

    /// A `session` node carrying `fields`, as a store publishes it (ADR-0160).
    fn published(fields: serde_json::Value) -> pos_proto::session::PublishedSession {
        serde_json::from_value(fields).expect("the node parses")
    }

    #[test]
    fn a_store_locks_out_after_its_own_number_of_attempts_for_its_own_minutes() {
        let lockout = Lockout::new();
        let who = employee();
        let strict = LockoutPolicy::of(&published(
            serde_json::json!({ "lockout_attempts": 3, "lockout_minutes": 1 }),
        ));

        assert_eq!(
            lockout.record(who, false, at(0), strict),
            SignIn::Wrong { remaining: 2 }
        );
        assert_eq!(
            lockout.record(who, false, at(0), strict),
            SignIn::Wrong { remaining: 1 }
        );
        assert_eq!(
            lockout.record(who, false, at(1_000), strict),
            SignIn::LockedOut {
                until_ms: 1_000 + 60 * 1_000
            },
            "the third wrong PIN locks out, for one minute"
        );
        assert_eq!(
            lockout.record(who, true, at(1_000 + 60 * 1_000), strict),
            SignIn::Ok,
            "and a minute later the right PIN is let in"
        );
    }

    #[test]
    fn a_number_published_mid_count_applies_from_the_next_attempt() {
        let lockout = Lockout::new();
        let who = employee();
        for _ in 0..3 {
            let _ = lockout.record(who, false, at(0), usual());
        }
        // Three failures counted against five; the store now allows three, so the next failure is
        // past the number and locks.
        let strict = LockoutPolicy::of(&published(serde_json::json!({ "lockout_attempts": 3 })));
        assert_eq!(
            lockout.record(who, false, at(0), strict),
            SignIn::LockedOut {
                until_ms: 5 * 60 * 1_000
            }
        );
        // A lockout already running keeps the end it was given, whatever the store sets next.
        let lenient = LockoutPolicy::of(&published(serde_json::json!({ "lockout_minutes": 1 })));
        assert_eq!(
            lockout.record(who, true, at(2 * 60 * 1_000), lenient),
            SignIn::LockedOut {
                until_ms: 5 * 60 * 1_000
            }
        );
    }

    #[test]
    fn no_published_number_switches_the_lockout_off() {
        for fields in [
            serde_json::json!({ "lockout_attempts": 0, "lockout_minutes": 0 }),
            serde_json::json!({ "lockout_attempts": -5, "lockout_minutes": -5 }),
            serde_json::json!({ "lockout_attempts": 1_000_000, "lockout_minutes": 1_000_000 }),
        ] {
            assert_eq!(
                LockoutPolicy::of(&published(fields.clone())),
                usual(),
                "{fields} reads as the defaults"
            );
        }
        assert_eq!(usual().max_failures, MAX_FAILURES);
        assert_eq!(usual().lockout, std::time::Duration::from_secs(5 * 60));
    }

    #[test]
    fn the_default_window_is_the_registers_thirty_minutes() {
        use super::DEFAULT_SIGN_IN_IDLE_TIMEOUT;
        use pos_proto::session::DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES;

        assert_eq!(
            DEFAULT_SIGN_IN_IDLE_TIMEOUT.as_secs(),
            u64::from(DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES) * 60
        );
    }

    /// The store's live configuration, as the config-pull loop swaps it.
    #[derive(Default)]
    struct LiveSettings(std::sync::Mutex<pos_proto::session::PublishedSession>);

    impl LiveSettings {
        fn publish(&self, fields: serde_json::Value) {
            *self.0.lock().expect("lock") = published(fields);
        }
    }

    impl super::SessionSettingsSource for LiveSettings {
        fn session_settings(&self) -> pos_proto::session::PublishedSession {
            *self.0.lock().expect("lock")
        }
    }

    #[test]
    fn the_sign_in_window_is_the_stores_then_the_fallback_and_follows_a_publish() {
        use super::Sessions;
        use pos_proto::ids::DeviceId;
        use std::sync::Arc;
        use std::time::Duration;

        let live = Arc::new(LiveSettings::default());
        // Built on a fallback of an hour, as a local file that sets one builds it.
        let sessions = Sessions {
            fallback_idle_timeout: Duration::from_secs(60 * 60),
            ..Sessions::new()
        }
        .following(Arc::<LiveSettings>::clone(&live));
        let device = DeviceId::new(Ulid::from_u128(0xD));
        let alice = EmployeeId::new(Ulid::from_u128(1));
        let minute = 60 * 1_000;
        block_on(sessions.sign_in(device, alice, at(0))).expect("no registry");

        // The node sets nothing: the fallback holds.
        assert_eq!(sessions.idle_timeout(), Duration::from_secs(60 * 60));
        assert_eq!(sessions.employee_for(device, at(59 * minute)), Some(alice));

        // The store publishes ten minutes: the next check uses it, with no restart.
        live.publish(serde_json::json!({ "sign_in_idle_timeout_minutes": 10 }));
        assert_eq!(sessions.idle_timeout(), Duration::from_secs(10 * 60));
        assert_eq!(
            sessions.employee_for(device, at(10 * minute - 1)),
            Some(alice)
        );
        assert_eq!(sessions.employee_for(device, at(10 * minute)), None);

        // A window outside the bounds is no window: the fallback holds again.
        live.publish(serde_json::json!({ "sign_in_idle_timeout_minutes": 0 }));
        assert_eq!(sessions.idle_timeout(), Duration::from_secs(60 * 60));
    }

    /// Drives a future on a current-thread runtime. `Sessions`' write paths are async because they
    /// may reach a registry; with none composed they never actually suspend, so one poll is enough
    /// and the test needs no timing.
    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build a current-thread runtime")
            .block_on(future)
    }

    #[test]
    fn a_device_signs_one_employee_in_and_out() {
        use super::Sessions;
        use pos_proto::ids::DeviceId;

        let sessions = Sessions::new();
        let device = DeviceId::new(Ulid::from_u128(0xD));
        let alice = EmployeeId::new(Ulid::from_u128(1));
        let bao = EmployeeId::new(Ulid::from_u128(2));
        let now = at(1_000);

        assert_eq!(
            sessions.employee_for(device, now),
            None,
            "nobody is signed in yet"
        );

        block_on(sessions.sign_in(device, alice, now)).expect("no registry, so no failure");
        assert_eq!(sessions.employee_for(device, now), Some(alice));

        // Signing another employee in replaces the first — one person per device.
        block_on(sessions.sign_in(device, bao, now)).expect("no registry, so no failure");
        assert_eq!(sessions.employee_for(device, now), Some(bao));

        block_on(sessions.sign_out(device)).expect("no registry, so no failure");
        assert_eq!(sessions.employee_for(device, now), None);
        // Signing out an already-signed-out device is a no-op, not a panic.
        block_on(sessions.sign_out(device)).expect("no registry, so no failure");
    }

    #[test]
    fn a_device_idle_past_the_window_is_reported_signed_out() {
        // The mitigation the durable sign-in decision rests on (ADR-0091): a till carried off while
        // signed in as a manager stops being that manager once it goes quiet.
        use super::{DEFAULT_SIGN_IN_IDLE_TIMEOUT, Sessions};
        use pos_proto::ids::DeviceId;

        let sessions = Sessions::new();
        let device = DeviceId::new(Ulid::from_u128(0xD));
        let alice = EmployeeId::new(Ulid::from_u128(1));
        let signed_in_at = at(1_000_000);
        block_on(sessions.sign_in(device, alice, signed_in_at)).expect("no registry");

        let window_ms = i64::try_from(DEFAULT_SIGN_IN_IDLE_TIMEOUT.as_millis()).expect("fits");
        let inside = at(signed_in_at.as_milliseconds_since_epoch() + window_ms - 1);
        let outside = at(signed_in_at.as_milliseconds_since_epoch() + window_ms);

        assert_eq!(sessions.employee_for(device, inside), Some(alice));
        assert_eq!(
            sessions.employee_for(device, outside),
            None,
            "at the window it has expired: the boundary is closed, not open"
        );

        // A touch inside the window moves it, so a device in use never expires.
        sessions.touch(device, inside);
        let extended = at(inside.as_milliseconds_since_epoch() + window_ms - 1);
        assert_eq!(
            sessions.employee_for(device, extended),
            Some(alice),
            "a device that is being used does not go idle"
        );
    }

    #[test]
    fn touching_an_unsigned_device_creates_nothing() {
        // Otherwise a touch would be a way to sign a device in without a PIN.
        use super::Sessions;
        use pos_proto::ids::DeviceId;

        let sessions = Sessions::new();
        let device = DeviceId::new(Ulid::from_u128(0xD));
        assert!(
            !sessions.touch(device, at(1_000)),
            "nothing to flush, because nothing was created"
        );
        assert_eq!(sessions.employee_for(device, at(1_000)), None);
    }

    #[test]
    fn a_clock_that_jumps_cannot_hold_a_session_open() {
        // The SNTP probe measures and never corrects, so the host clock is what this reads and a
        // daemon or a person can step it either way. Both directions must expire rather than extend.
        use super::{DEFAULT_SIGN_IN_IDLE_TIMEOUT, has_gone_idle};

        let last_seen = at(1_000_000);
        // Backwards: `now` is before the stored instant.
        assert!(
            has_gone_idle(last_seen, at(500_000), DEFAULT_SIGN_IN_IDLE_TIMEOUT),
            "a clock that went backwards must not read as just-seen"
        );
        // Forwards past the window.
        assert!(has_gone_idle(
            last_seen,
            at(9_000_000),
            DEFAULT_SIGN_IN_IDLE_TIMEOUT
        ));
        // Inside the window, unchanged.
        assert!(!has_gone_idle(
            last_seen,
            at(1_000_001),
            DEFAULT_SIGN_IN_IDLE_TIMEOUT
        ));
        // A timeout too large to compare is a misconfiguration, not a licence never to expire.
        assert!(has_gone_idle(
            last_seen,
            at(1_000_001),
            core::time::Duration::from_secs(u64::MAX / 1_000)
        ));
    }
}
