// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's keys
//! ([ADR-0217](../../../docs/adr/0217-secrets-are-sealed-under-versioned-tenant-keys.md)).
//!
//! Three layers: an **environment key** behind the [`pos_ports::KeyKeeper`] port, which wraps
//! tenant keys and nothing else; a random **tenant key** per tenant, stored only wrapped; and each
//! **secret** (a vendor credential, a gateway header) sealed under its tenant's key. Every key has
//! a version, and every wrapped or sealed value names the version that made it.
//!
//! This module holds the keepers this binary carries. [`EnvironmentKeys`] is the keeper over key
//! material it is handed: the **file keeper**, whose one version is `integration_secret` from
//! `cloud.toml`, for development and demonstrations only. A production cloud must not run on it:
//! ADR-0217 decision 5 names a `systemd-creds` keeper, the key sealed to the host's TPM2, and a
//! managed KMS for that.
//!
//! [`Keys`] puts the layers together: it reads a tenant's key versions from the
//! [`TenantKeyStore`], unwraps them through the keeper, and hands a request a [`TenantSealer`]
//! that seals under the newest and opens under any (`sealer.rs`).

mod environment;
mod reseal;
mod sealer;
mod tenant;

pub use environment::{ENVIRONMENT_KEY_LEN, EnvironmentKeys};
pub use reseal::{ResealError, ResealReport, reseal_tenant};
pub use sealer::{KEY_VERSIONS_MAX, Keys, TenantSealer, sealed_version};
pub use tenant::{MemoryTenantKeys, TenantKey, TenantKeyStore, TenantKeyStoreError};
