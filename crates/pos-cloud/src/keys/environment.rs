// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A [`KeyKeeper`] over environment-key material this process holds, by version: XChaCha20-Poly1305
//! with a random nonce per wrap, bound to the version that wraps (ADR-0217 decisions 2 and 5).
//!
//! The file keeper is this over `integration_secret`, as version 1. The key material is zeroized
//! when dropped and never printed: [`fmt::Debug`] names the versions only.

use core::fmt;
use std::collections::BTreeMap;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use pos_ports::key_keeper::{KeyKeeper, WrappedKey};
use pos_ports::key_vault::Secret;
use pos_ports::{PortError, PortName};

use crate::connections::ConnectionSecret;

/// Bytes in one environment key.
pub const ENVIRONMENT_KEY_LEN: usize = 32;

/// Bytes of nonce: XChaCha20-Poly1305, so a random nonce per wrap needs no counter.
const NONCE_LEN: usize = 24;

/// What every wrap is bound to besides its version.
const WRAP_CONTEXT: &[u8] = b"pos-cloud/environment-key/v1\0";

/// The environment keys this process holds, by version; it wraps under the newest.
pub struct EnvironmentKeys {
    keys: BTreeMap<u32, Zeroizing<[u8; ENVIRONMENT_KEY_LEN]>>,
}

impl EnvironmentKeys {
    /// A keeper over `keys`, by version. `None` when there is none, or a version is 0, which
    /// names the format before tenant keys and is never an environment key.
    #[must_use]
    pub fn new(keys: impl IntoIterator<Item = (u32, [u8; ENVIRONMENT_KEY_LEN])>) -> Option<Self> {
        let keys: BTreeMap<u32, Zeroizing<[u8; ENVIRONMENT_KEY_LEN]>> = keys
            .into_iter()
            .map(|(version, key)| (version, Zeroizing::new(key)))
            .collect();
        (!keys.is_empty() && !keys.contains_key(&0)).then_some(Self { keys })
    }

    /// The file keeper: `integration_secret` as version 1. For development and demonstrations
    /// only; `None` when the key is malformed.
    #[must_use]
    pub fn from_file_key(integration_secret: &ConnectionSecret) -> Option<Self> {
        let material = integration_secret.material()?;
        Self::new([(1, *material)])
    }

    /// The version a new wrap uses.
    #[must_use]
    pub fn newest_version(&self) -> u32 {
        self.keys.keys().next_back().copied().unwrap_or_default()
    }

    fn cipher(&self, version: u32) -> Result<XChaCha20Poly1305, PortError> {
        let key = self.keys.get(&version).ok_or_else(|| {
            PortError::not_found(
                PortName::KeyKeeper,
                format!("this cloud holds no environment key v{version}"),
            )
        })?;
        Ok(XChaCha20Poly1305::new((&**key).into()))
    }
}

/// The bytes a wrap under `version` is bound to.
fn associated_data(version: u32) -> Vec<u8> {
    let mut aad = WRAP_CONTEXT.to_vec();
    aad.extend_from_slice(&version.to_be_bytes());
    aad
}

impl fmt::Debug for EnvironmentKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvironmentKeys")
            .field("versions", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl KeyKeeper for EnvironmentKeys {
    async fn wrap(&self, key: &Secret) -> Result<WrappedKey, PortError> {
        let version = self.newest_version();
        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_error| {
            PortError::unavailable(PortName::KeyKeeper, "the OS entropy source is unavailable")
        })?;
        let aad = associated_data(version);
        let sealed = self
            .cipher(version)?
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: key.expose(),
                    aad: &aad,
                },
            )
            .map_err(|_error| {
                PortError::internal(PortName::KeyKeeper, "a tenant key could not be wrapped")
            })?;
        let mut bytes = Vec::with_capacity(NONCE_LEN + sealed.len());
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&sealed);
        Ok(WrappedKey {
            keeper_version: version,
            bytes,
        })
    }

    async fn unwrap(&self, wrapped: &WrappedKey) -> Result<Secret, PortError> {
        let version = wrapped.keeper_version;
        let cipher = self.cipher(version)?;
        let (nonce, body) = wrapped
            .bytes
            .split_at_checked(NONCE_LEN)
            .filter(|(_, body)| !body.is_empty())
            .ok_or_else(|| {
                PortError::invalid_argument(PortName::KeyKeeper, "a wrapped key is too short")
            })?;
        let nonce = XNonce::try_from(nonce).map_err(|_error| {
            PortError::invalid_argument(PortName::KeyKeeper, "a wrapped key is malformed")
        })?;
        let aad = associated_data(version);
        let opened = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: body,
                    aad: &aad,
                },
            )
            .map_err(|_error| {
                PortError::permission_denied(
                    PortName::KeyKeeper,
                    format!("a wrapped key does not open under environment key v{version}"),
                )
            })?;
        Ok(Secret::new(opened))
    }
}

#[cfg(test)]
mod tests {
    use pos_contract_tests::harness::{KeyKeeperHarness, Setup};
    use pos_fakes::executor::run_ready;

    use super::{ENVIRONMENT_KEY_LEN, EnvironmentKeys};
    use crate::connections::ConnectionSecret;

    struct FileKeeper;

    impl KeyKeeperHarness for FileKeeper {
        type Keeper = EnvironmentKeys;

        async fn fresh(&self) -> Setup<Self::Keeper> {
            let secret = ConnectionSecret::new("11".repeat(ENVIRONMENT_KEY_LEN));
            EnvironmentKeys::from_file_key(&secret)
                .ok_or_else(|| pos_contract_tests::harness::HarnessError::new("a file key"))
        }

        async fn other(&self) -> Setup<Self::Keeper> {
            let secret = ConnectionSecret::new("22".repeat(ENVIRONMENT_KEY_LEN));
            EnvironmentKeys::from_file_key(&secret)
                .ok_or_else(|| pos_contract_tests::harness::HarnessError::new("a file key"))
        }
    }

    mod file_keeper {
        use super::{FileKeeper, run_ready};
        pos_contract_tests::key_keeper_suite!(FileKeeper, run_ready);
    }

    #[test]
    fn a_keeper_wraps_under_its_newest_version_and_opens_every_version_it_holds() {
        let older = EnvironmentKeys::new([(1, [7; ENVIRONMENT_KEY_LEN])]).expect("a keeper");
        let rotated =
            EnvironmentKeys::new([(1, [7; ENVIRONMENT_KEY_LEN]), (2, [9; ENVIRONMENT_KEY_LEN])])
                .expect("a keeper");
        let key = pos_ports::Secret::new(vec![5; 32]);
        let wrapped_before = run_ready(pos_ports::KeyKeeper::wrap(&older, &key)).expect("wraps");
        let wrapped_after = run_ready(pos_ports::KeyKeeper::wrap(&rotated, &key)).expect("wraps");
        assert_eq!(wrapped_before.keeper_version, 1);
        assert_eq!(wrapped_after.keeper_version, 2);
        let opened =
            run_ready(pos_ports::KeyKeeper::unwrap(&rotated, &wrapped_before)).expect("opens");
        assert!(opened == key);
        assert!(format!("{rotated:?}").contains("[1, 2]"));
    }

    #[test]
    fn version_zero_and_a_malformed_file_key_are_no_keeper() {
        assert!(EnvironmentKeys::new([(0, [1; ENVIRONMENT_KEY_LEN])]).is_none());
        assert!(EnvironmentKeys::new([]).is_none());
        assert!(EnvironmentKeys::from_file_key(&ConnectionSecret::new("not hex")).is_none());
    }
}
