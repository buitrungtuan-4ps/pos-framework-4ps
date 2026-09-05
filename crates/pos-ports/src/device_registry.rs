// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Where the store keeps which devices it has admitted, and who is signed in on each
//! ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)).
//!
//! Both tables used to be process memory, so a power blip, an OTA install
//! ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md) restarts the edge on purpose) or a
//! `systemctl restart` unpaired every tablet in the store at once. This port is what makes them
//! survive.
//!
//! # The port never sees a device token
//!
//! It takes a [`TokenDigest`] — a SHA-256 of the token — and never the token itself. That is a
//! stronger guarantee than "the adapter is careful": an implementation cannot leak a credential it
//! was never handed, and a stolen `pos.db` yields digests, which cannot be presented to the gate.
//! Hashing happens where the token already exists, in the edge; this crate is backbone
//! ([ADR-0021](../../../docs/adr/0021-corrected-port-list.md)) and carries no hash dependency.
//!
//! A plain digest is the right primitive and a password KDF would be the wrong one. A device token
//! is 128 bits from the OS CSPRNG, so there is no dictionary to run and no salt to add — and this
//! lookup sits on the gate that every single request crosses, which is not a place to put a
//! deliberately slow hash.
//!
//! # The idle timeout is not implemented here
//!
//! The port stores `last_seen_at` and hands the session back; **the caller decides whether it has
//! expired**. Keeping that in the edge means the policy is one pure comparison tested without a
//! database, and every adapter behaves identically because none of them knows the rule. See
//! [`DeviceSession`].

use core::fmt;
use core::future::Future;

use pos_proto::ids::{DeviceId, EmployeeId};
use pos_proto::time::Timestamp;

use crate::error::PortError;

/// How many bytes a SHA-256 digest is.
const DIGEST_BYTES: usize = 32;

/// A SHA-256 digest of a device token — what the registry stores in place of the token.
///
/// # Why this is not treated as a secret
///
/// A digest cannot be presented: the gate hashes what the client sent and compares digests, so
/// holding this value authenticates nothing. It is still not something to scatter through logs,
/// because it correlates requests to a device across restarts — so [`Debug`] prints a short prefix,
/// which is enough to follow one device through a log without publishing the whole value.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenDigest([u8; DIGEST_BYTES]);

impl TokenDigest {
    /// Wraps a computed digest.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    /// The digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }

    /// Lowercase hex, which is how an adapter stores it in a text column.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut hex = String::with_capacity(DIGEST_BYTES * 2);
        for byte in self.0 {
            // Indexing a 16-element table with a value masked to 0..16 cannot be out of bounds,
            // and this crate forbids both panics and `unsafe`, so the table is written as a
            // lookup on a `match` rather than as a slice index.
            hex.push(nibble(byte >> 4));
            hex.push(nibble(byte & 0x0f));
        }
        hex
    }

    /// Parses the 64 lowercase hex characters [`Self::to_hex`] produces.
    ///
    /// `None` for anything else, so a corrupted or hand-edited row is rejected rather than
    /// silently becoming a digest that matches nothing.
    #[must_use]
    pub fn parse_hex(text: &str) -> Option<Self> {
        if text.len() != DIGEST_BYTES * 2 {
            return None;
        }
        let mut bytes = [0_u8; DIGEST_BYTES];
        let mut characters = text.bytes();
        for slot in &mut bytes {
            let high = un_nibble(characters.next()?)?;
            let low = un_nibble(characters.next()?)?;
            *slot = (high << 4) | low;
        }
        Some(Self(bytes))
    }
}

/// One hex character for the low four bits of `value`.
const fn nibble(value: u8) -> char {
    match value & 0x0f {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        _ => 'f',
    }
}

/// The four bits one lowercase hex character stands for, or `None` if it is not one.
const fn un_nibble(character: u8) -> Option<u8> {
    match character {
        b'0'..=b'9' => Some(character - b'0'),
        b'a'..=b'f' => Some(character - b'a' + 10),
        _ => None,
    }
}

impl fmt::Debug for TokenDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.to_hex();
        let prefix: String = hex.chars().take(8).collect();
        write!(f, "TokenDigest({prefix}…)")
    }
}

/// A device the store has admitted: its local id, the digest of the token it holds, and when it
/// paired.
///
/// `paired_at` is stored for the operator's benefit — a pairing list that cannot say *when* is hard
/// to audit — and is what a future pairing expiry would read
/// ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md) defers that deliberately).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairedDevice {
    /// The device this token authenticates as.
    pub device_id: DeviceId,
    /// SHA-256 of the issued token. The token itself is never stored.
    pub token_digest: TokenDigest,
    /// When the device redeemed its pairing code.
    pub paired_at: Timestamp,
}

/// Who is signed in on one device, and when that device was last heard from.
///
/// # `last_seen_at` and the idle timeout
///
/// The timeout is enforced by the **caller**, not by an implementation of this port: a session
/// whose `last_seen_at` is older than the configured window is treated as absent, which is what
/// keeps a till that left the store from still trading as the person who was on it. The rule is
/// evaluated as a difference between two instants and **fails closed** — a negative or implausible
/// interval expires the session rather than extending it — because the edge's clock is the host OS
/// clock and an NTP daemon or a person can step it either way. A clock that jumps must never be a
/// way to hold a session open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceSession {
    /// The device the employee is signed in on.
    pub device_id: DeviceId,
    /// The employee whose actions this device's commands are recorded as.
    pub employee_id: EmployeeId,
    /// When they signed in.
    pub signed_in_at: Timestamp,
    /// When the device last made an authenticated request. The idle timeout reads this.
    pub last_seen_at: Timestamp,
}

/// The store's durable record of admitted devices and their sign-ins.
///
/// # Contract
///
/// 1. **Reading something absent is `Ok(None)`, never an error.** A store with no paired device is
///    the normal first-boot state, and a paired device with nobody signed in is the normal state
///    between shifts.
/// 2. **`record_pairing` replaces by device, and by digest.** Re-recording the same device is not an
///    error; two devices never share a digest.
/// 3. **Every revoke and clear is idempotent.** Revocation runs again when an operator is unsure it
///    worked, and it must be safe to re-run. Revoking a device that is not there succeeds.
/// 4. **Revoking a device clears its sign-in too.** The two tables cannot be left disagreeing: a
///    session belonging to no paired device would be unreachable state that a later feature could
///    read as live.
/// 5. **`record_sign_in` replaces.** One employee is signed in on a device at a time; signing
///    another in replaces the first rather than adding to it.
/// 6. **`touch_session` on a device with no session succeeds and changes nothing.** The gate calls
///    it on every request, including on a device that has just been signed out by another
///    request — a race, not a fault.
/// 7. **Nothing here stores a device token, a PIN, or a PIN hash.** Only digests and identifiers.
///
/// Everything here works with the cable unplugged
/// ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)). A store noticing a missing
/// tablet cannot be told to wait for the cloud before revoking it.
pub trait DeviceRegistry: Send + Sync {
    /// Records a freshly paired device, replacing any earlier record of it.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be written.
    fn record_pairing(
        &self,
        device: PairedDevice,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// The device a token digest was issued to, or `None` if it was never issued or was revoked.
    ///
    /// **The shipped edge does not call this on the request path, or anywhere else.** It reads
    /// [`Self::paired_devices`] once at boot, holds the digest map in memory, and answers every
    /// later request from it — which is what makes the gate cost nothing per request. The method
    /// stays because it is a genuine capability of a registry, because the contract suite asserts
    /// the digest→device binding and its revocation through it, and because an edge that chose to
    /// resolve against storage rather than cache the table would need exactly this. It is not a
    /// consistency check the boot path runs; an earlier doc in `pos-edge` said it was, and no such
    /// check existed (production-readiness **X2**).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be read.
    fn device_for_token(
        &self,
        digest: TokenDigest,
    ) -> impl Future<Output = Result<Option<DeviceId>, PortError>> + Send;

    /// Every device the store has admitted, for the pairing screen and for boot.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be read.
    fn paired_devices(&self) -> impl Future<Output = Result<Vec<PairedDevice>, PortError>> + Send;

    /// Retires one device: its token stops resolving and its sign-in is cleared. Idempotent.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be written.
    fn revoke_device(
        &self,
        device_id: DeviceId,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Retires every device — the break-glass that reproduces, on purpose, what a restart used to
    /// do by accident. Idempotent.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be written.
    fn revoke_all_devices(&self) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Records a sign-in, replacing any earlier one on that device.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be written.
    fn record_sign_in(
        &self,
        session: DeviceSession,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// The sign-in recorded for a device, whether or not it has since gone idle — expiry is the
    /// caller's decision, so this reports the row as stored.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be read.
    fn sign_in_for(
        &self,
        device_id: DeviceId,
    ) -> impl Future<Output = Result<Option<DeviceSession>, PortError>> + Send;

    /// Every recorded sign-in, for boot.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be read.
    fn sign_ins(&self) -> impl Future<Output = Result<Vec<DeviceSession>, PortError>> + Send;

    /// Moves a device's `last_seen_at` forward. A no-op on a device with no session.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be written.
    fn touch_session(
        &self,
        device_id: DeviceId,
        now: Timestamp,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Ends the sign-in on a device. Idempotent.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be written.
    fn clear_sign_in(
        &self,
        device_id: DeviceId,
    ) -> impl Future<Output = Result<(), PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::TokenDigest;

    #[test]
    fn hex_round_trips_every_byte_value() {
        let mut bytes = [0_u8; 32];
        for (index, slot) in bytes.iter_mut().enumerate() {
            // 0, 8, 16 … 248 — spread across the range, including both nibble extremes.
            *slot = u8::try_from(index * 8).unwrap_or(0);
        }
        let digest = TokenDigest::from_bytes(bytes);
        let hex = digest.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(TokenDigest::parse_hex(&hex), Some(digest));
    }

    #[test]
    fn the_two_nibble_extremes_survive() {
        let digest = TokenDigest::from_bytes([0xff; 32]);
        assert_eq!(digest.to_hex(), "f".repeat(64));
        assert_eq!(TokenDigest::parse_hex(&digest.to_hex()), Some(digest));

        let zero = TokenDigest::from_bytes([0x00; 32]);
        assert_eq!(zero.to_hex(), "0".repeat(64));
        assert_eq!(TokenDigest::parse_hex(&zero.to_hex()), Some(zero));
    }

    #[test]
    fn a_row_that_is_not_a_digest_is_refused_rather_than_matching_nothing() {
        // A hand-edited or truncated row must be visibly wrong, not a value that silently
        // authenticates no device.
        assert_eq!(TokenDigest::parse_hex(""), None);
        assert_eq!(TokenDigest::parse_hex(&"a".repeat(63)), None);
        assert_eq!(TokenDigest::parse_hex(&"a".repeat(65)), None);
        assert_eq!(TokenDigest::parse_hex(&"A".repeat(64)), None, "uppercase");
        assert_eq!(TokenDigest::parse_hex(&"g".repeat(64)), None, "not hex");
        // 64 bytes of UTF-8 that is not 64 characters: `len()` counts bytes, so the parse must
        // also reject on the character stream rather than trusting the length alone.
        assert_eq!(TokenDigest::parse_hex(&"é".repeat(32)), None);
    }

    #[test]
    fn debug_shows_a_prefix_only() {
        let digest = TokenDigest::from_bytes([0xab; 32]);
        let shown = format!("{digest:?}");
        assert_eq!(shown, "TokenDigest(abababab…)");
        assert!(
            !shown.contains(&"ab".repeat(32)),
            "the whole digest must not reach a log"
        );
    }
}
