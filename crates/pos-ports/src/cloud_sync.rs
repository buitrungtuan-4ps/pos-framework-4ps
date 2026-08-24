// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's request/response channel to the cloud
//! ([ADR-0053](../../../docs/adr/0053-cloud-sync-port.md)).
//!
//! [`crate::MessageLink`] is outbound-only — the store pushes events and never waits on the cloud
//! (that is what lets it sell offline). But two flows genuinely need an answer back: exchanging a
//! single-use activation code for the machine's long-lived credential on first boot
//! ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)), and fetching an update artifact
//! for an over-the-air rollout ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)). `CloudSync`
//! is that channel — a small request/response port the edge calls and the cloud answers — kept
//! distinct from the event pipeline so the offline-first guarantee stays a property of `MessageLink`.
//!
//! # Compile-time selected, so no object-safe mirror
//!
//! One adapter per binary, chosen at startup like [`crate::MessageLink`] and [`crate::KeyVault`], so
//! this port has no `Dyn` mirror (`docs/adr/0013-async-strategy.md`).
//!
//! # `pos-core` names none of this
//!
//! The activation code arrives as a plain `&str` — the edge parses and normalises it with
//! `pos_core::activation::ActivationCode` before calling — and the credential comes back as a
//! [`crate::Secret`], ready to store in the [`crate::KeyVault`] under `SecretName::DeviceCredential`
//! and never a file.

use core::future::Future;

use pos_proto::ids::DeviceId;
use pos_proto::text::ReleaseTag;

use crate::error::PortError;
use crate::key_vault::Secret;

/// What the cloud returns when it redeems an activation code: the identity the machine now
/// authenticates as, and the credential to store.
#[derive(Debug)]
pub struct ActivationGrant {
    /// The device id the credential authenticates as.
    pub device_id: DeviceId,
    /// The long-lived device credential — redacted in [`core::fmt::Debug`]; store it in the
    /// [`crate::KeyVault`], never on disk.
    pub credential: Secret,
}

/// The store's request/response channel to the cloud
/// ([ADR-0053](../../../docs/adr/0053-cloud-sync-port.md)).
pub trait CloudSync: Send + Sync {
    /// Exchanges a single-use activation code for this machine's long-lived credential.
    ///
    /// # Errors
    ///
    /// [`PortError::permission_denied`] if the cloud refused the code — spent, revoked, or unknown;
    /// the cloud gives no oracle, so the three are indistinguishable here. [`PortError::invalid_argument`]
    /// if the code is malformed, and [`PortError::unavailable`] if the cloud could not be reached.
    fn activate(
        &self,
        activation_code: &str,
    ) -> impl Future<Output = Result<ActivationGrant, PortError>> + Send;

    /// Fetches the signed update artifact for `release`, as raw bytes to verify and stage.
    ///
    /// The bytes are the *signed* artifact: the caller verifies the signature with [`crate::Signer`]
    /// before trusting them, because a transport is not a trust boundary — a compromised or spoofed
    /// cloud must not be able to install code.
    ///
    /// # Errors
    ///
    /// [`PortError::not_found`] if the cloud publishes no such release, and [`PortError::unavailable`]
    /// if the cloud could not be reached.
    fn fetch_update(
        &self,
        release: &ReleaseTag,
    ) -> impl Future<Output = Result<Vec<u8>, PortError>> + Send;
}
