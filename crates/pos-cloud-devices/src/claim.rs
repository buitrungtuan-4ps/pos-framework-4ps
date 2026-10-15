// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Claiming a box that was installed with no store
//! ([ADR-0148](../../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md)),
//! the device-authorisation pattern of RFC 8628.
//!
//! 1. A box with no `config.toml` opens a claim (`POST /claim`, unauthenticated and rate-limited).
//!    It gets a `claim_id`, a short [`UserCode`] to show, and a 256-bit secret it keeps to itself.
//! 2. A console user with `console.devices.manage` types the code and picks the device slot
//!    (`POST /admin/claims/bind`).
//! 3. The box collects its device credential once (`POST /claim/{claim_id}/collect`), presenting the
//!    secret. The credential is minted for the slot in the same transaction that marks the claim
//!    collected, ADR-0051's rule for activation.
//!
//! The cloud keeps `SHA-256` of the code and of the secret, never either value. A claim can be bound
//! and collected for [`CLAIM_TTL_MS`] after it was opened, and each step happens once.

use core::fmt;
use core::future::Future;

use sha2::{Digest, Sha256};

use pos_proto::ids::{DeviceId, StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

use crate::activation::DeviceCredential;

/// How long a claim can be bound and collected after the box opened it: one hour.
pub const CLAIM_TTL_MS: i64 = 3_600_000;

/// The number of characters in a user code.
pub const USER_CODE_LEN: usize = 8;

/// How many random bytes a user code is drawn from: eight 5-bit characters.
pub const USER_CODE_ENTROPY: usize = 5;

/// Crockford's base32 alphabet: no `I`, `L`, `O` or `U`, so nothing on the screen reads two ways.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The code a box shows while it waits to be claimed: eight characters, shown as `XXXX-XXXX`.
///
/// Forty bits is short enough to type from across a counter and plenty for what it guards: it lives
/// an hour, it can be bound once, and binding it needs a console session with device rights. The
/// secret that collects the credential is the box's alone and never shown.
#[derive(Clone, PartialEq, Eq)]
pub struct UserCode(String);

impl UserCode {
    /// A code drawn from `entropy`, five bytes of OS randomness.
    #[must_use]
    pub fn from_entropy(entropy: [u8; USER_CODE_ENTROPY]) -> Self {
        let bits = entropy
            .iter()
            .fold(0_u64, |acc, byte| (acc << 8) | u64::from(*byte));
        let code = (0..USER_CODE_LEN)
            .rev()
            .map(|index| {
                let digit = (bits >> (index * 5)) & 0x1f;
                ALPHABET
                    .get(usize::try_from(digit).unwrap_or(0))
                    .map_or('0', |byte| char::from(*byte))
            })
            .collect();
        Self(code)
    }

    /// Reads what a person typed: case, spaces and dashes are ignored, and Crockford's look-alikes
    /// read as the digits they resemble (`I` and `L` as `1`, `O` as `0`). `None` unless exactly eight
    /// characters of the alphabet remain.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut code = String::with_capacity(USER_CODE_LEN);
        for character in text.chars() {
            let character = match character.to_ascii_uppercase() {
                ' ' | '-' => continue,
                'I' | 'L' => '1',
                'O' => '0',
                other => other,
            };
            if !character.is_ascii() || !ALPHABET.contains(&u8::try_from(character).ok()?) {
                return None;
            }
            code.push(character);
        }
        (code.len() == USER_CODE_LEN).then_some(Self(code))
    }

    /// The eight canonical characters, which are what is hashed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The code as a person reads it: `XXXX-XXXX`.
    #[must_use]
    pub fn display(&self) -> String {
        let (head, tail) = self.0.split_at(USER_CODE_LEN / 2);
        format!("{head}-{tail}")
    }
}

impl fmt::Debug for UserCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UserCode(<redacted>)")
    }
}

/// `SHA-256` of a user code's canonical characters: what the claim is found by at binding.
#[must_use]
pub fn hash_user_code(code: &UserCode) -> [u8; 32] {
    Sha256::digest(code.as_str().as_bytes()).into()
}

/// `SHA-256` of a claim secret: what collection compares against.
#[must_use]
pub fn hash_claim_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// What binding a user code came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindOutcome {
    /// The claim is now bound to the slot.
    Bound,
    /// No claim has this code.
    Unknown,
    /// The claim's hour is over.
    Expired,
    /// The claim was already bound, possibly by someone else. A code binds once.
    AlreadyBound,
}

/// What a collection attempt came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectOutcome {
    /// The claim was bound, and its credential is now minted for this slot. Answered exactly once.
    Collected {
        /// The tenant the box belongs to.
        tenant_id: TenantId,
        /// The store the box was claimed for.
        store_id: StoreId,
        /// The device slot it fills.
        device_id: DeviceId,
    },
    /// Nobody has bound the code yet: poll again.
    Pending,
    /// The claim's hour is over: open a new one.
    Expired,
    /// No such claim, a wrong secret, or a claim already collected. Told apart from nothing, so
    /// the answer teaches a prober nothing.
    Refused,
}

/// A failure of the store behind the claims themselves. The caller answers retryably.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the claim store failed: {0}")]
pub struct ClaimStoreError(String);

impl ClaimStoreError {
    /// A failure carrying a reason for the log.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The store behind claiming (a table in `store-postgres`; a fake in tests).
pub trait ClaimStore {
    /// Records a newly opened claim. Only the hashes are kept.
    ///
    /// # Errors
    ///
    /// [`ClaimStoreError`] if the store cannot be written, including the vanishingly rare case of a
    /// user code another live claim already holds: the box asks again and draws a new one.
    fn open(
        &self,
        claim_id: Ulid,
        user_code_hash: [u8; 32],
        secret_hash: [u8; 32],
        expires_at: Timestamp,
    ) -> impl Future<Output = Result<(), ClaimStoreError>> + Send;

    /// Binds the pending, unexpired claim holding `user_code_hash` to a device slot.
    ///
    /// # Errors
    ///
    /// [`ClaimStoreError`] only if the store cannot be read or written.
    fn bind(
        &self,
        user_code_hash: [u8; 32],
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
        now: Timestamp,
    ) -> impl Future<Output = Result<BindOutcome, ClaimStoreError>> + Send;

    /// Collects a bound claim: in one transaction, marks it collected and stores `credential` for
    /// its slot. Anything but a bound, unexpired claim whose secret matches changes nothing.
    ///
    /// # Errors
    ///
    /// [`ClaimStoreError`] only if the store cannot be read or written.
    fn collect(
        &self,
        claim_id: Ulid,
        secret_hash: [u8; 32],
        credential: &DeviceCredential,
        now: Timestamp,
    ) -> impl Future<Output = Result<CollectOutcome, ClaimStoreError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{UserCode, hash_user_code};

    /// Every code drawn is eight characters of the alphabet, and reads back as itself.
    #[test]
    fn a_drawn_code_parses_as_itself() {
        for seed in [[0_u8; 5], [0xff; 5], [0x12, 0x34, 0x56, 0x78, 0x9a]] {
            let code = UserCode::from_entropy(seed);
            assert_eq!(code.as_str().len(), 8);
            assert_eq!(UserCode::parse(code.as_str()), Some(code.clone()));
            assert_eq!(UserCode::parse(&code.display()), Some(code));
        }
        assert_eq!(UserCode::from_entropy([0; 5]).as_str(), "00000000");
        assert_eq!(UserCode::from_entropy([0xff; 5]).as_str(), "ZZZZZZZZ");
    }

    /// A person's typing is forgiven: case, dashes, spaces, and the letters that look like digits.
    #[test]
    fn what_a_person_types_is_read_kindly() {
        let exact = UserCode::parse("AB10-CD0Z").expect("parses");
        assert_eq!(UserCode::parse("ab1o cdoz"), Some(exact.clone()));
        assert_eq!(UserCode::parse("AB10CD0Z"), Some(exact.clone()));
        assert_eq!(UserCode::parse("abIO-cdOz"), Some(exact.clone()));
        assert_eq!(
            hash_user_code(&exact),
            hash_user_code(&UserCode::parse("ab10-cd0z").expect("parses"))
        );
        assert_eq!(UserCode::parse("AB10-CD0"), None, "seven characters");
        assert_eq!(UserCode::parse("AB10-CD0ZZ"), None, "nine characters");
        assert_eq!(
            UserCode::parse("AB1U-CD0Z"),
            None,
            "U is not in the alphabet"
        );
        assert_eq!(UserCode::parse("AB1é-CD0Z"), None);
    }

    /// The code never reaches a log whole.
    #[test]
    fn debug_redacts_the_code() {
        let code = UserCode::parse("AB10-CD0Z").expect("parses");
        assert!(!format!("{code:?}").contains("AB10"));
    }
}
