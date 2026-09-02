// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge over-the-air updater ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md)).
//!
//! This is the on-box orchestration that ties three things together: the pure rollout *decision*
//! ([`decide_rollout`](pos_core::ota::decide_rollout), [ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)),
//! signature *verification* (the [`Signer`] port, [ADR-0047](../../../docs/adr/0047-minisign-verification.md)),
//! and artifact *fetch* ([`CloudSync`], [ADR-0053](../../../docs/adr/0053-cloud-sync-port.md)). The real
//! machine operations — the `.pre-update` database copy, writing the binary, the self-test, the reboot,
//! the revert — sit behind the [`UpdateInstaller`] seam, which the shipped binary implements against
//! the OS and which is the one part not exercised in the pull-request gate (it needs a real box; see
//! `docs/roadmap.md` P9).
//!
//! # Verify before the disk is touched
//!
//! The order is the safety argument: a demoted box ([`LeaseStanding::Superseded`]) never updates; a
//! failed self-test rolls back rather than commits; and an artifact is verified — right key, not
//! revoked, signature valid — *before* [`UpdateInstaller::stage_backup`] or
//! [`UpdateInstaller::apply`] run. A spoofed cloud fails at verification, having written nothing.

use core::fmt;

use pos_core::lease::LeaseStanding;
use pos_core::ota::{
    DeviceState, PublishedUpdate, ReleaseVersion, RolloutDecision, SigningKeyId, SkipReason,
    decide_rollout,
};
use pos_ports::PortError;
use pos_ports::cloud_sync::CloudSync;
use pos_ports::signer::{PublicKey, Signature, Signer};
use pos_proto::text::ReleaseTag;

/// A failure of the real-machine install steps — as distinct from a refusal or a routine rollback.
#[derive(Debug, thiserror::Error)]
#[error("the update installer failed: {0}")]
pub struct InstallError(String);

impl InstallError {
    /// An install failure carrying a human-readable reason.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// What went wrong when the updater could not carry an eligible update through.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The artifact could not be fetched from the cloud.
    #[error("fetching the update artifact failed: {0}")]
    Fetch(PortError),
    /// The signature did not verify against the trusted key.
    #[error("verifying the update signature failed: {0}")]
    Verify(PortError),
    /// The artifact is signed by a key on the revocation list.
    #[error("the update is signed by a revoked key")]
    Revoked,
    /// The artifact is signed by a key this binary does not have baked in.
    #[error("the update is signed by a key this binary does not trust")]
    UntrustedKey,
    /// A real-machine install step failed.
    #[error("installing the update failed: {0}")]
    Install(InstallError),
}

/// What the updater did — exactly one outcome per run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// The target version was verified, installed, self-tested, and committed.
    Installed {
        /// The version now running.
        version: ReleaseVersion,
    },
    /// The running version was reverted — a failed self-test, or a [`RolloutDecision::RollBack`].
    RolledBack,
    /// The kill switch is engaged; nothing was installed.
    Halted,
    /// The update is signed by a revoked key; nothing was installed.
    Refused,
    /// Not this box's turn, for the given reason.
    Skipped(SkipReason),
    /// The box has lost its lease and is read-only; it does not update.
    ReadOnly,
}

/// The real-machine steps of an install, behind a seam so the orchestration can be tested without a
/// box ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md)).
///
/// The shipped binary implements these against the OS; they are the one part not run in the
/// pull-request gate.
pub trait UpdateInstaller: Send + Sync {
    /// Copies the live database to a `.pre-update` sidecar before staging (`docs/roadmap.md` P9).
    ///
    /// # Errors
    ///
    /// [`InstallError`] if the copy could not be made.
    fn stage_backup(&self) -> Result<(), InstallError>;

    /// Writes the verified `artifact` as the next binary. The real OS install.
    ///
    /// # Errors
    ///
    /// [`InstallError`] if the write failed.
    fn apply(&self, artifact: &[u8]) -> Result<(), InstallError>;

    /// Runs the post-install self-test.
    ///
    /// # Errors
    ///
    /// [`InstallError`] if the test could not be run — a test that *ran and failed* is `Ok(false)`,
    /// which is a routine rollback, not an error.
    fn self_test(&self) -> Result<bool, InstallError>;

    /// Commits the staged install as the running binary (may reboot).
    ///
    /// # Errors
    ///
    /// [`InstallError`] if the commit failed.
    fn commit(&self) -> Result<(), InstallError>;

    /// Reverts to the last-good binary and the `.pre-update` database.
    ///
    /// # Errors
    ///
    /// [`InstallError`] if the revert failed.
    fn rollback(&self) -> Result<(), InstallError>;
}

/// One update to weigh: the published rollout, the release tag to fetch, and the revocation list —
/// everything [`OtaUpdater::run`] needs beyond the device's own state.
///
/// # Why the signature is not in here
///
/// It used to be, as `signature: &'a Signature`, and **nothing in production could fill it in**:
/// `CloudSync::fetch_update` returned the artifact bytes alone, so the only `UpdatePlan` ever
/// constructed was in this crate's own tests. The fix is not to find a producer for the field but to
/// delete it — the signature belongs to the artifact, arrives with it as a
/// [`SignedArtifact`](pos_ports::SignedArtifact), and a plan built *before* the fetch has no
/// business claiming to know it ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)).
///
/// Keeping both would have been worse than either: two signatures with no rule for which one wins,
/// and a caller free to pass the plan's while the bytes came with another.
#[derive(Debug)]
pub struct UpdatePlan<'a> {
    /// The rollout the cloud published ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)).
    pub published: &'a PublishedUpdate,
    /// The release tag [`CloudSync::fetch_update`] fetches the artifact by.
    pub release: &'a ReleaseTag,
    /// The revoked signing-key ids ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)).
    pub revoked_keys: &'a [SigningKeyId],
}

/// The edge updater: decide, fetch, verify, and drive the install seam.
pub struct OtaUpdater<C, S, I> {
    cloud: C,
    signer: S,
    installer: I,
    trusted_keys: Vec<PublicKey>,
}

impl<C, S, I> fmt::Debug for OtaUpdater<C, S, I> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OtaUpdater")
            .field("trusted_keys", &self.trusted_keys.len())
            .finish_non_exhaustive()
    }
}

impl<C, S, I> OtaUpdater<C, S, I> {
    /// Composes an updater over its three seams and the public keys baked into this binary
    /// ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)).
    #[must_use]
    pub fn new(cloud: C, signer: S, installer: I, trusted_keys: Vec<PublicKey>) -> Self {
        Self {
            cloud,
            signer,
            installer,
            trusted_keys,
        }
    }
}

impl<C: CloudSync, S: Signer, I: UpdateInstaller> OtaUpdater<C, S, I> {
    /// Runs one update cycle for `device` against `plan`, given the box's `lease` standing.
    ///
    /// # Errors
    ///
    /// [`UpdateError`] if an eligible update could not be fetched, failed verification, or an install
    /// step failed. A refusal, a halt, a skip, a routine rollback, and a read-only box are outcomes,
    /// not errors.
    pub async fn run(
        &self,
        device: &DeviceState,
        plan: &UpdatePlan<'_>,
        lease: LeaseStanding,
    ) -> Result<UpdateOutcome, UpdateError> {
        // A box that has lost the store to a replacement is read-only; it must not install anything.
        match lease {
            LeaseStanding::Active => {}
            LeaseStanding::Superseded | LeaseStanding::Invalid => {
                return Ok(UpdateOutcome::ReadOnly);
            }
        }

        match decide_rollout(device, plan.published, plan.revoked_keys) {
            RolloutDecision::RollBack => {
                self.installer.rollback().map_err(UpdateError::Install)?;
                Ok(UpdateOutcome::RolledBack)
            }
            RolloutDecision::Halt => Ok(UpdateOutcome::Halted),
            RolloutDecision::Refuse => Ok(UpdateOutcome::Refused),
            RolloutDecision::Skip(reason) => Ok(UpdateOutcome::Skipped(reason)),
            RolloutDecision::Install => self.install(plan).await,
        }
    }

    /// Fetch → verify → stage → apply → self-test → commit-or-rollback. Verification gates the disk.
    ///
    /// The signature verified here is the one that came back **with** the bytes, so there is no
    /// arrangement of this function in which unverified bytes reach `apply`: obtaining the artifact
    /// and obtaining the thing that judges it are the same call.
    async fn install(&self, plan: &UpdatePlan<'_>) -> Result<UpdateOutcome, UpdateError> {
        let fetched = self
            .cloud
            .fetch_update(plan.release)
            .await
            .map_err(UpdateError::Fetch)?;
        let artifact = fetched.bytes;
        self.verify(&artifact, &fetched.signature, plan.revoked_keys)?;

        self.installer
            .stage_backup()
            .map_err(UpdateError::Install)?;
        self.installer
            .apply(&artifact)
            .map_err(UpdateError::Install)?;
        if self.installer.self_test().map_err(UpdateError::Install)? {
            self.installer.commit().map_err(UpdateError::Install)?;
            Ok(UpdateOutcome::Installed {
                version: plan.published.target,
            })
        } else {
            self.installer.rollback().map_err(UpdateError::Install)?;
            Ok(UpdateOutcome::RolledBack)
        }
    }

    /// Verifies the artifact before it is trusted: read the claimed key id, refuse a revoked one,
    /// select the matching baked-in key, and check the signature.
    fn verify(
        &self,
        artifact: &[u8],
        signature: &Signature,
        revoked_keys: &[SigningKeyId],
    ) -> Result<(), UpdateError> {
        let claimed = self
            .signer
            .key_id_of(signature)
            .map_err(UpdateError::Verify)?;
        if revoked_keys.contains(claimed.as_bytes()) {
            return Err(UpdateError::Revoked);
        }
        let key = self
            .trusted_keys
            .iter()
            .find(|trusted| trusted.key_id() == claimed)
            .ok_or(UpdateError::UntrustedKey)?;
        self.signer
            .verify(artifact, signature, key)
            .map_err(UpdateError::Verify)
    }
}
