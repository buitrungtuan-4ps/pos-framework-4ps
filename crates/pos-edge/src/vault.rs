// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The vault at start: where the secrets are this run, and the move of any still in the OS keyring
//! into the sealed vault ([ADR-0151](../../../docs/adr/0151-a-headless-linux-box-seals-its-secrets-with-systemd-creds.md),
//! [ADR-0174](../../../docs/adr/0174-a-windows-store-keeps-its-state-to-its-own-service.md)).

use key_vault_keyring::{OsVault, VaultStanding};

/// Moves any secret still only in the OS keyring into the sealed vault, each proven before its
/// keyring copy goes, so a Windows box that is about to change service account leaves nothing
/// behind in the Credential Manager of `LocalSystem` (ADR-0174 decision 7). A failure keeps the keyring
/// copy, which is tried again at the next start.
pub(crate) async fn migrate_vault(vault: &OsVault) {
    match vault.migrate_from_keyring().await {
        Ok(0) => {}
        Ok(moved) => tracing::info!(
            moved,
            "moved secrets from the OS keyring into the sealed vault"
        ),
        Err(error) => tracing::warn!(
            %error,
            "could not move a secret from the OS keyring into the sealed vault; it stays in the \
             keyring and is tried again at the next start (ADR-0174)"
        ),
    }
}

/// Says where this run keeps its secrets, once at start: the one line that tells an operator why a
/// Linux box asked for a new activation code after a reboot, and what fixes it.
pub(crate) fn log_vault(standing: &VaultStanding) {
    match standing {
        VaultStanding::Sealed(dir) => tracing::info!(
            dir = %dir.display(),
            "secrets are sealed under this machine's vault key (systemd-creds or DPAPI; ADR-0151, ADR-0174)"
        ),
        #[cfg(target_os = "linux")]
        VaultStanding::Keyring => tracing::warn!(
            "secrets are in the kernel keyring, which a reboot empties: run the installer again so \
             it seals a vault key and the activation survives a power cut (ADR-0151)"
        ),
        #[cfg(not(target_os = "linux"))]
        VaultStanding::Keyring => tracing::info!("secrets are in the OS credential store"),
        VaultStanding::Unusable(reason) => tracing::error!(
            %reason,
            "the vault key could not be used, so the vault answers errors: the counter keeps \
             trading and cloud sync waits until it is fixed; run the installer again (ADR-0151)"
        ),
    }
}
