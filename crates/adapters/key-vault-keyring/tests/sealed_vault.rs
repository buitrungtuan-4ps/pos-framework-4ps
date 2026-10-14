// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The shared `KeyVault` contract suite against the vault a store box runs on with a vault
//! directory: [`OsVault`] in front of sealed files.
//!
//! On Linux the vault key is a raw file, as the systemd helper leaves it (ADR-0151). On Windows
//! there is no file to begin with, and the first call makes one protected with DPAPI (ADR-0174), so
//! this runs the real DPAPI on the Windows CI job. No OS keyring is needed on either: the sealed
//! store answers first, and a keyring this process cannot reach holds nothing to move.

#[cfg(test)]
mod suite {
    use std::future::Future;
    use std::path::PathBuf;

    use key_vault_keyring::{OsVault, VaultStanding};
    use pos_contract_tests::harness::{KeyVaultHarness, Setup};

    /// Each case gets its own directory and vault key, so no case sees another's secrets.
    struct SealedVaultHarness;

    fn scratch() -> PathBuf {
        let dir = tempfile::tempdir().expect("scratch").keep();
        dir.join("data")
    }

    impl KeyVaultHarness for SealedVaultHarness {
        type Vault = OsVault;

        async fn fresh(&self) -> Setup<Self::Vault> {
            let data = scratch();
            let key = data.join("vault.key");
            #[cfg(not(windows))]
            {
                std::fs::create_dir_all(&data).expect("data");
                std::fs::write(&key, [9_u8; 32]).expect("key");
            }
            let vault = OsVault::from_paths(Some(key), Some(data.join("vault")));
            assert!(
                matches!(vault.standing(), VaultStanding::Sealed(_)),
                "a sealed vault, not {:?}",
                vault.standing()
            );
            Ok(vault)
        }
    }

    /// `OsVault` runs its file I/O on the blocking pool, so each case runs on a runtime of its own.
    fn on_a_runtime<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    pos_contract_tests::key_vault_suite!(SealedVaultHarness, on_a_runtime);
}
