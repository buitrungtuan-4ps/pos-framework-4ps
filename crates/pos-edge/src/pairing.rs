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

use pos_proto::time::Timestamp;

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
    /// Tokens issued to paired devices.
    issued: Mutex<Vec<DeviceToken>>,
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
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes)?;
        let token = DeviceToken::from_entropy(bytes);
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(token.clone());
        Ok(Some(token))
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
}
