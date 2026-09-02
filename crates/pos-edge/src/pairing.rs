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

use core::fmt::{self, Write as _};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pos_ports::device_registry::{PairedDevice, TokenDigest};
use pos_ports::error::PortError;
use pos_proto::ids::DeviceId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use sha2::{Digest as _, Sha256};

use crate::durable_auth::DurableAuth;

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

    /// The SHA-256 of this token — what the durable registry stores in place of it (ADR-0091).
    ///
    /// A digest and not a password KDF: the token is 128 bits from the OS CSPRNG, so there is no
    /// dictionary to run and no salt to add, and this is computed on the gate every request crosses.
    #[must_use]
    pub fn digest(&self) -> TokenDigest {
        TokenDigest::from_bytes(Sha256::digest(self.0.as_bytes()).into())
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

/// The edge's live pairing codes and the devices it has admitted.
///
/// # Reads are in memory; writes go through to the registry
///
/// [`Self::device_for`] runs on the front of every request, so it answers from a map without
/// touching a database. Every *change* — a redeem, a revoke — is written through to the
/// [`DurableAuth`] registry when one is composed ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)),
/// and [`Self::load`] refills the map at boot. So a restart no longer unpairs the store, and the hot
/// path costs what it always did.
///
/// With no registry ([`Pairing::new`]) the behaviour is exactly what it was before S0d: memory only,
/// cleared by a restart. That is what the tests and the on-fakes example use.
///
/// # The map is keyed by digest, so the edge holds no device token anywhere
///
/// It has to be: a restart can only restore what was stored, and what is stored is a SHA-256
/// (ADR-0091). Keying the live map the same way means loaded rows and fresh redeems are
/// indistinguishable, and it removes the token from the process's memory as well as from its disk —
/// [`Self::device_for`] hashes what the client presented and looks *that* up. The token exists for
/// exactly as long as it takes [`Self::redeem`] to hand it back to the device that will hold it.
#[derive(Default)]
pub struct Pairing {
    /// Active codes to their expiry (ms since epoch). Deliberately **not** persisted: a code lives
    /// five minutes and is single-use, so surviving a restart would buy nothing and would keep a
    /// credential-shaped value on disk for no reason.
    codes: Mutex<HashMap<Code, i64>>,
    /// Digests of the tokens issued to paired devices, each bound to the device it authenticates as.
    issued: Mutex<HashMap<TokenDigest, DeviceId>>,
    /// Where issued tokens are recorded so they survive a restart. `None` keeps the pre-S0d
    /// behaviour.
    registry: Option<Arc<dyn DurableAuth>>,
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "`codes` and `issued` are summarised as counts on purpose: a live pairing code is a \
              secret, and a token digest correlates a device across restarts. Neither belongs in a log."
)]
impl fmt::Debug for Pairing {
    /// Counts, never contents. There is no token here to leak any more, but a digest still
    /// correlates a device across restarts, so `{:?}` reports sizes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pairing")
            .field("live_codes", &self.code_count())
            .field("issued", &self.issued_count())
            .field("durable", &self.registry.is_some())
            .finish()
    }
}

impl Pairing {
    /// A fresh pairing state with no active codes, in memory only.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pairing state that records what it issues, so a restart does not unpair the store.
    #[must_use]
    pub fn durable(registry: Arc<dyn DurableAuth>) -> Self {
        Self {
            codes: Mutex::new(HashMap::new()),
            issued: Mutex::new(HashMap::new()),
            registry: Some(registry),
        }
    }

    /// Refills the in-memory table from the registry — the boot step that makes a restart invisible
    /// to a device that had already paired. Returns how many devices were restored.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the registry cannot be read. `serve` treats that as fatal: starting with an
    /// empty table would silently unpair a store that *is* paired, and an operator would then be
    /// re-pairing tills to fix a problem that was never theirs.
    pub async fn load(&self) -> Result<usize, PortError> {
        let Some(registry) = self.registry.as_ref() else {
            return Ok(0);
        };
        let devices = registry.paired_devices().await?;
        let mut issued = self.issued.lock().unwrap_or_else(PoisonError::into_inner);
        issued.clear();
        for device in &devices {
            issued.insert(device.token_digest, device.device_id);
        }
        Ok(devices.len())
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
    /// # Durability
    ///
    /// The device is recorded in the registry **before** the token is returned, so a crash between
    /// the two leaves the device recorded but the operator without a token — they pair again, which
    /// is the safe direction. The reverse order would hand out a credential the box would forget.
    ///
    /// # Errors
    ///
    /// [`PairError::Entropy`] if the OS entropy source is unavailable — a token is never faked — or
    /// [`PairError::Registry`] if the device could not be recorded. Both refuse the pairing rather
    /// than issuing a token that might not survive.
    pub async fn redeem(
        &self,
        code: &Code,
        now: Timestamp,
    ) -> Result<Option<DeviceToken>, PairError> {
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
        getrandom::fill(&mut token_bytes).map_err(PairError::Entropy)?;
        let token = DeviceToken::from_entropy(token_bytes);
        let mut device_bytes = [0_u8; 10];
        getrandom::fill(&mut device_bytes).map_err(PairError::Entropy)?;
        let device_id = mint_device_id(now, &device_bytes);
        let digest = token.digest();

        if let Some(registry) = self.registry.as_ref() {
            registry
                .record_pairing(PairedDevice {
                    device_id,
                    token_digest: digest,
                    paired_at: now,
                })
                .await
                .map_err(PairError::Registry)?;
        }
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(digest, device_id);
        Ok(Some(token))
    }

    /// The device a presented token authenticates as, or `None` if it was never issued or has been
    /// revoked. This is the check every gated route makes before acting (ADR-0084).
    ///
    /// Hashes the presented token and looks the digest up, which is the same single map read it
    /// always was plus one SHA-256 of 32 bytes.
    #[must_use]
    pub fn device_for(&self, token: &DeviceToken) -> Option<DeviceId> {
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&token.digest())
            .copied()
    }

    /// Retires one device: its token stops resolving here and in the registry. Idempotent.
    ///
    /// Removed from the registry **first**. If that fails the in-memory entry is left alone and the
    /// error is reported, because a device that looks revoked on this box but is restored by the
    /// next restart is the worst of the three outcomes — the operator believes a lost tablet is
    /// locked out when it is not.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the registry could not be written.
    pub async fn revoke(&self, device_id: DeviceId) -> Result<(), PortError> {
        if let Some(registry) = self.registry.as_ref() {
            registry.revoke_device(device_id).await?;
        }
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, held| *held != device_id);
        Ok(())
    }

    /// Retires every device — the break-glass that reproduces, on purpose, what a restart used to do
    /// by accident. Idempotent.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the registry could not be written. As with [`Self::revoke`], memory is
    /// cleared only after the durable table is.
    pub async fn revoke_all(&self) -> Result<(), PortError> {
        if let Some(registry) = self.registry.as_ref() {
            registry.revoke_all_devices().await?;
        }
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        Ok(())
    }

    /// Whether this state writes through to a registry — what the pairing screen reports so an
    /// operator knows whether a restart will cost them the fleet.
    #[must_use]
    pub fn is_durable(&self) -> bool {
        self.registry.is_some()
    }

    /// How many devices have been paired — for the pairing screen and tests.
    #[must_use]
    pub fn issued_count(&self) -> usize {
        self.issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// How many pairing codes are live, for [`fmt::Debug`].
    fn code_count(&self) -> usize {
        self.codes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// Why a pairing could not be completed.
///
/// Two causes rather than one, because they need different operator responses: no entropy is a
/// broken machine, and a registry failure is a full or unwritable disk. Both refuse the pairing —
/// issuing a token the box might forget would hand a device a credential that stops working at the
/// next restart, which is the failure S0d exists to remove.
#[derive(Debug, thiserror::Error)]
pub enum PairError {
    /// The OS entropy source was unavailable.
    #[error("the operating system's entropy source is unavailable")]
    Entropy(#[source] getrandom::Error),
    /// The device could not be recorded durably.
    #[error("the device registry could not record the pairing")]
    Registry(#[source] PortError),
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

    /// Drives a future on a current-thread runtime. `redeem` is async because it may write to a
    /// registry; with none composed it never actually suspends.
    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build a current-thread runtime")
            .block_on(future)
    }

    #[test]
    fn a_minted_code_redeems_once() {
        let pairing = Pairing::new();
        let code = pairing.mint(at(0)).expect("mint");

        let token = block_on(pairing.redeem(&code, at(1_000))).expect("redeem");
        assert!(token.is_some(), "a fresh code pairs a device");
        assert_eq!(pairing.issued_count(), 1);

        // Single use: the same code cannot pair a second device.
        assert!(
            block_on(pairing.redeem(&code, at(2_000)))
                .expect("redeem again")
                .is_none(),
            "a redeemed code is spent"
        );
        assert_eq!(pairing.issued_count(), 1);
    }

    #[test]
    fn a_digest_is_stable_per_token_and_differs_between_tokens() {
        // The property the digest-keyed map rests on: resolving is `device_for` hashing what the
        // client sent, so the same token must always hash the same way and two tokens must not
        // collide. (That the *resolution* works is the test below; this is about the key.)
        let token = DeviceToken::from_entropy([0xAB; 16]);
        let other = DeviceToken::from_entropy([0x11; 16]);
        assert_eq!(token.digest(), token.digest(), "stable");
        assert_ne!(token.digest(), other.digest(), "no collision");
        assert_eq!(token.digest().to_hex().len(), 64);
        assert!(
            !token.digest().to_hex().contains(token.as_str()),
            "the digest does not contain the token it came from"
        );
    }

    #[test]
    fn revoking_stops_a_token_resolving() {
        // With no registry composed this exercises the in-memory half only; the durable half is
        // proven by the DeviceRegistry contract suite, which both implementations pass.
        let pairing = Pairing::new();
        let code = pairing.mint(at(0)).expect("mint");
        let token = block_on(pairing.redeem(&code, at(1_000)))
            .expect("redeem")
            .expect("a fresh code pairs a device");
        let device = pairing.device_for(&token).expect("resolves");

        block_on(pairing.revoke(device)).expect("no registry, so no failure");
        assert!(
            pairing.device_for(&token).is_none(),
            "a revoked device's token authenticates nothing"
        );
        assert_eq!(pairing.issued_count(), 0);
        // Idempotent: an operator unsure it worked runs it again.
        block_on(pairing.revoke(device)).expect("revoking twice is a no-op");
    }

    #[test]
    fn revoke_all_is_the_break_glass() {
        let pairing = Pairing::new();
        let first = pairing.mint(at(0)).expect("mint");
        let second = pairing.mint(at(0)).expect("mint");
        let one = block_on(pairing.redeem(&first, at(1_000)))
            .expect("redeem")
            .expect("token");
        let two = block_on(pairing.redeem(&second, at(1_000)))
            .expect("redeem")
            .expect("token");
        assert_eq!(pairing.issued_count(), 2);

        block_on(pairing.revoke_all()).expect("no registry, so no failure");
        assert!(pairing.device_for(&one).is_none());
        assert!(pairing.device_for(&two).is_none());
        assert_eq!(pairing.issued_count(), 0);
    }

    #[test]
    fn debug_reports_counts_and_never_a_digest() {
        let pairing = Pairing::new();
        let code = pairing.mint(at(0)).expect("mint");
        let token = block_on(pairing.redeem(&code, at(1_000)))
            .expect("redeem")
            .expect("token");
        let shown = format!("{pairing:?}");
        assert!(shown.contains("issued: 1"), "got {shown}");
        assert!(shown.contains("durable: false"), "got {shown}");
        assert!(
            !shown.contains(token.as_str()),
            "a token must not reach a log through Debug"
        );
        assert!(
            !shown.contains(&token.digest().to_hex()),
            "nor a digest, which correlates a device across restarts"
        );
    }

    #[test]
    fn an_expired_code_does_not_redeem() {
        let pairing = Pairing::new();
        let code = pairing.mint(at(0)).expect("mint");
        let past_ttl = i64::try_from(CODE_TTL.as_millis()).expect("fits") + 1;
        assert!(
            block_on(pairing.redeem(&code, at(past_ttl)))
                .expect("redeem")
                .is_none(),
            "a code past its TTL is dead"
        );
    }

    #[test]
    fn an_unknown_code_does_not_redeem() {
        let pairing = Pairing::new();
        let stranger = Code::from_entropy([1, 2, 3]);
        assert!(
            block_on(pairing.redeem(&stranger, at(0)))
                .expect("redeem")
                .is_none()
        );
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
        let token = block_on(pairing.redeem(&code, at(1_000)))
            .expect("redeem")
            .expect("a fresh code pairs");

        let device = pairing.device_for(&token);
        assert!(device.is_some(), "the issued token authenticates a device");

        // A well-formed but never-issued token authenticates nothing.
        let stranger = DeviceToken::from_entropy([0x11; 16]);
        assert_eq!(pairing.device_for(&stranger), None);
    }
}
