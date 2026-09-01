// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Device pairing: a single-use, short-lived 6-digit code (ADR-0030).
//!
//! A tablet joins the store by presenting a code the edge minted; the edge issues it a device token.
//! The code is worth little if shoulder-surfed — it is **single use** and **expires** in
//! [`CODE_TTL`] — and the whole flow works with the cable unplugged
//! ([ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)), because pairing a device
//! during an internet outage is exactly when a store needs it.
//!
//! This is device-level trust (which tablets may reach the edge), distinct from which employee is
//! acting ([`crate::auth`]).
//!
//! # Secrets
//!
//! A pairing code and a device token are secrets and never enter a log or the fan-out. The pairing
//! **URL** the operator scans is shown once on the edge's own console.

use core::fmt::Write as _;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use pos_proto::ids::DeviceId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

/// How long a freshly minted pairing code stays valid.
pub const CODE_TTL: Duration = Duration::from_secs(5 * 60);

/// A six-digit pairing code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Code(String);

impl Code {
    /// Derives a code from three bytes of entropy — pure, so it is tested without the OS RNG.
    ///
    /// A pairing code is not a cryptographic key: the single-use rule and the five-minute expiry are
    /// its defence, so the negligible modulo bias in mapping 24 bits onto a million values does not
    /// matter here.
    #[must_use]
    pub fn from_entropy(bytes: [u8; 3]) -> Self {
        let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        Self(format!("{:06}", value % 1_000_000))
    }

    /// Parses a submitted code, accepting exactly six ASCII digits.
    ///
    /// `None` for anything else, so a malformed code is rejected before it reaches the code table —
    /// a code is looked up by equality, and only a well-formed one can match a minted code.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() == 6 && text.bytes().all(|b| b.is_ascii_digit()) {
            Some(Self(text.to_owned()))
        } else {
            None
        }
    }

    /// The six digits, for display in the pairing URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque bearer token a paired device presents on later requests. 128 bits, hex-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceToken(String);

impl DeviceToken {
    /// Builds a token from sixteen bytes of entropy.
    #[must_use]
    fn from_entropy(bytes: [u8; 16]) -> Self {
        let hex = bytes
            .iter()
            .fold(String::with_capacity(32), |mut acc, byte| {
                let _ = write!(acc, "{byte:02x}");
                acc
            });
        Self(hex)
    }

    /// The token string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses a presented token: exactly the 32 lowercase hex characters [`from_entropy`] produces.
    ///
    /// `None` for anything else, so a malformed token is rejected before it reaches the issued table
    /// — a token is looked up by equality, and only a well-formed one can match an issued one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let well_formed = text.len() == 32
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        well_formed.then(|| Self(text.to_owned()))
    }
}

/// The raw-IP pairing URL an operator scans or types — the discovery path that needs no name
/// resolution (ADR-0030). `code` is embedded so a scan pairs in one step.
#[must_use]
pub fn pairing_url(host: IpAddr, port: u16, code: &Code) -> String {
    format!("http://{host}:{port}/pair?code={}", code.as_str())
}

/// The edge's live pairing codes and the device tokens it has issued.
#[derive(Debug, Default)]
pub struct Pairing {
    /// Active codes to their expiry (ms since epoch).
    codes: Mutex<HashMap<Code, i64>>,
    /// Tokens issued to paired devices, each bound to the device id it authenticates as. In memory
    /// only: a restart clears them, so every device re-pairs (persisting the table is a flagged
    /// follow-up, ADR-0084).
    issued: Mutex<HashMap<DeviceToken, DeviceId>>,
}

impl Pairing {
    /// A fresh pairing state with no active codes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mints a new code valid for [`CODE_TTL`] from `now`.
    ///
    /// # Errors
    ///
    /// [`getrandom::Error`] if the OS entropy source is unavailable — a code is never faked.
    pub fn mint(&self, now: Timestamp) -> Result<Code, getrandom::Error> {
        let mut bytes = [0_u8; 3];
        getrandom::fill(&mut bytes)?;
        let code = Code::from_entropy(bytes);
        let ttl = i64::try_from(CODE_TTL.as_millis()).unwrap_or(i64::MAX);
        let expiry = now.as_milliseconds_since_epoch().saturating_add(ttl);
        self.codes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(code.clone(), expiry);
        Ok(code)
    }

    /// Redeems a code, issuing a device token if it is live and unexpired.
    ///
    /// Single use: a redeemed or expired code is removed, so it cannot pair a second device. Returns
    /// `Ok(None)` when the code is unknown or expired — an ordinary rejection, not an error.
    ///
    /// # Errors
    ///
    /// [`getrandom::Error`] if the OS entropy source is unavailable — a token is never faked.
    pub fn redeem(
        &self,
        code: &Code,
        now: Timestamp,
    ) -> Result<Option<DeviceToken>, getrandom::Error> {
        let now_ms = now.as_milliseconds_since_epoch();
        let live = {
            let mut codes = self.codes.lock().unwrap_or_else(PoisonError::into_inner);
            match codes.remove(code) {
                Some(expiry) => now_ms < expiry,
                None => false,
            }
        };
        if !live {
            return Ok(None);
        }
        // Sixteen bytes seed the bearer token; ten more seed the device id it binds to. Two fixed
        // arrays rather than one sliced buffer, so no indexing or unwrap can panic here.
        let mut token_bytes = [0_u8; 16];
        getrandom::fill(&mut token_bytes)?;
        let token = DeviceToken::from_entropy(token_bytes);
        let mut device_bytes = [0_u8; 10];
        getrandom::fill(&mut device_bytes)?;
        let device_id = mint_device_id(now, &device_bytes);
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(token.clone(), device_id);
        Ok(Some(token))
    }

    /// The device a presented token authenticates as, or `None` if it was never issued (or was
    /// issued by a since-restarted edge process — tokens are in-memory). This is the check every
    /// command route makes before acting (ADR-0084).
    #[must_use]
    pub fn device_for(&self, token: &DeviceToken) -> Option<DeviceId> {
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(token)
            .copied()
    }

    /// How many devices have been paired — for the pairing screen and tests.
    #[must_use]
    pub fn issued_count(&self) -> usize {
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// Mints a device id for a freshly paired device: a ULID timestamped `now`, with 80 random bits
/// drawn from the same OS entropy the token took. Unique per pairing; the cloud's approved-device
/// registry (propose→approve) is a separate identity this local id does not claim to be.
fn mint_device_id(now: Timestamp, randomness: &[u8]) -> DeviceId {
    let ms = u64::try_from(now.as_milliseconds_since_epoch()).unwrap_or(0);
    let random = randomness
        .iter()
        .take(10)
        .fold(0_u128, |acc, &byte| (acc << 8) | u128::from(byte));
    DeviceId::new(Ulid::from_parts(ms, random))
}

#[cfg(test)]
mod tests {
    use super::{CODE_TTL, Code, DeviceToken, Pairing, pairing_url};
    use pos_proto::time::Timestamp;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid instant")
    }

    #[test]
    fn a_code_is_always_six_digits() {
        assert_eq!(Code::from_entropy([0, 0, 0]).as_str(), "000000");
        assert_eq!(Code::from_entropy([0, 0, 1]).as_str(), "000001");
        let code = Code::from_entropy([0xFF, 0xFF, 0xFF]);
        assert_eq!(code.as_str().len(), 6);
        assert!(code.as_str().chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn a_device_token_is_thirty_two_hex_characters() {
        let token = DeviceToken::from_entropy([0xAB; 16]);
        assert_eq!(token.as_str(), "abababababababababababababababab");
    }

    #[test]
    fn the_pairing_url_carries_the_code_over_raw_ip() {
        let code = Code::from_entropy([0, 0, 42]);
        let url = pairing_url("192.168.1.42".parse().expect("ip"), 8787, &code);
        assert_eq!(url, "http://192.168.1.42:8787/pair?code=000042");
    }

    #[test]
    fn a_minted_code_redeems_once() {
        let pairing = Pairing::new();
        let code = pairing.mint(at(0)).expect("mint");

        let token = pairing.redeem(&code, at(1_000)).expect("redeem");
        assert!(token.is_some(), "a fresh code pairs a device");
        assert_eq!(pairing.issued_count(), 1);

        // Single use: the same code cannot pair a second device.
        assert!(
            pairing
                .redeem(&code, at(2_000))
                .expect("redeem again")
                .is_none(),
            "a redeemed code is spent"
        );
        assert_eq!(pairing.issued_count(), 1);
    }

    #[test]
    fn an_expired_code_does_not_redeem() {
        let pairing = Pairing::new();
        let code = pairing.mint(at(0)).expect("mint");
        let past_ttl = i64::try_from(CODE_TTL.as_millis()).expect("fits") + 1;
        assert!(
            pairing
                .redeem(&code, at(past_ttl))
                .expect("redeem")
                .is_none(),
            "a code past its TTL is dead"
        );
    }

    #[test]
    fn an_unknown_code_does_not_redeem() {
        let pairing = Pairing::new();
        let stranger = Code::from_entropy([1, 2, 3]);
        assert!(pairing.redeem(&stranger, at(0)).expect("redeem").is_none());
    }

    #[test]
    fn parse_accepts_the_shape_from_entropy_produces_and_rejects_others() {
        let issued = DeviceToken::from_entropy([0xAB; 16]);
        assert_eq!(DeviceToken::parse(issued.as_str()), Some(issued));
        assert!(DeviceToken::parse("").is_none());
        assert!(DeviceToken::parse("tooshort").is_none());
        assert!(
            DeviceToken::parse("ABABABABABABABABABABABABABABABAB").is_none(),
            "uppercase is not the lowercase hex from_entropy emits"
        );
        assert!(
            DeviceToken::parse("zbababababababababababababababab").is_none(),
            "a non-hex character is rejected"
        );
    }

    #[test]
    fn a_redeemed_token_resolves_to_its_device_and_a_stranger_does_not() {
        let pairing = Pairing::new();
        let code = pairing.mint(at(0)).expect("mint");
        let token = pairing
            .redeem(&code, at(1_000))
            .expect("redeem")
            .expect("a fresh code pairs");

        let device = pairing.device_for(&token);
        assert!(device.is_some(), "the issued token authenticates a device");

        // A well-formed but never-issued token authenticates nothing.
        let stranger = DeviceToken::from_entropy([0x11; 16]);
        assert_eq!(pairing.device_for(&stranger), None);
    }
}
