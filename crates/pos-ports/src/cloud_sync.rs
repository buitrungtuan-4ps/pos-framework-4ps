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

use pos_proto::ids::{DeviceId, StoreId, TenantId};
use pos_proto::text::ReleaseTag;

use crate::error::PortError;
use crate::key_vault::Secret;
use crate::signer::Signature;

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

/// An update artifact and the detached signature that judges it, fetched together.
///
/// They arrive as one value on purpose. The alternative — bytes from one call, signature from
/// another — lets a caller hold tens of megabytes of executable code while holding nothing to
/// verify it with, and the type system has no objection: every such call site then depends on
/// somebody remembering the rule. Pairing them means the only way to obtain the bytes is to also be
/// handed the thing that judges them, so a caller who wants to skip verification has to visibly
/// discard it ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)).
///
/// This is the one boundary in the system where forgetting a step means installing someone else's
/// code, which is why it is worth a type rather than a comment.
///
/// Holding both is *not* verification. [`crate::Signer::verify`] against a key baked into this
/// binary is what makes the bytes trustworthy; a transport is never a trust boundary, and a
/// compromised cloud can supply a matched artifact-and-signature pair it forged for its own key.
/// That is exactly what the baked-in trusted keys refuse.
#[derive(Debug)]
pub struct SignedArtifact {
    /// The artifact bytes, exactly as the cloud served them — unverified until
    /// [`crate::Signer::verify`] says otherwise.
    pub bytes: Vec<u8>,
    /// The detached signature over [`Self::bytes`].
    pub signature: Signature,
}

/// What the edge tells the cloud after applying (or rolling back) an update: which store is
/// reporting, the version it is now running, and whether the post-install self-test passed.
///
/// This is how the cloud learns rollout-ring progress ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)).
/// A report is pure telemetry — it never changes what the edge runs — and carries only ids the port
/// already names plus the two facts the cloud needs, never a customer identifier.
#[derive(Debug, Clone)]
pub struct UpdateReport {
    /// The tenant the reporting store belongs to.
    pub tenant: TenantId,
    /// The store now running `installed`.
    pub store: StoreId,
    /// The release the store is running after the update cycle — the target on a successful install,
    /// the prior version on a rollback.
    pub installed: ReleaseTag,
    /// Whether the post-install self-test passed. `false` is a rollback the cloud should surface, not
    /// an error.
    pub self_test_passed: bool,
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

    /// Fetches the update artifact for `release` **with** its detached signature, to verify and
    /// stage.
    ///
    /// The caller verifies the signature with [`crate::Signer`] before trusting the bytes, because a
    /// transport is not a trust boundary — a compromised or spoofed cloud must not be able to install
    /// code. Returning the two together is what stops a caller obtaining the bytes alone; see
    /// [`SignedArtifact`].
    ///
    /// # Errors
    ///
    /// [`PortError::not_found`] if the cloud publishes no such release, and [`PortError::unavailable`]
    /// if the cloud could not be reached **or answered without a signature** — bytes with nothing to
    /// judge them are unusable, and the retryable status is right because a proxy stripping a header
    /// or a cloud mid-deploy is the likely cause. It must never be read as permission to install
    /// unverified code.
    fn fetch_update(
        &self,
        release: &ReleaseTag,
    ) -> impl Future<Output = Result<SignedArtifact, PortError>> + Send;

    /// Reports the outcome of an update the edge applied — the version now running and whether the
    /// self-test passed — so the cloud can track rollout-ring progress
    /// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)). Fire-and-forget from the edge's
    /// point of view: a report that does not reach the cloud is retried or dropped, never a reason to
    /// undo an install that already happened.
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] if the cloud rejected the report as malformed, and
    /// [`PortError::unavailable`] if the cloud could not be reached.
    fn report(&self, report: &UpdateReport) -> impl Future<Output = Result<(), PortError>> + Send;
}
