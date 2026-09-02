// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `CloudSync` suite.
//!
//! A transport port, so its contract is thin but load-bearing: a faithful request/response, and the
//! *right* [`PortError`](pos_ports::PortError) status on refusal — a caller branches on that status,
//! so a wrong one is a wrong retry policy. [`an_unrecognised_code_is_refused`] pins the no-oracle
//! posture ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)): a refused code surfaces
//! as `PermissionDenied`, indistinguishable from a spent or revoked one.

use pos_ports::PortName;
use pos_ports::cloud_sync::CloudSync;
use pos_proto::ErrorStatus;
use pos_proto::text::ReleaseTag;

use crate::harness::CloudSyncHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `CloudSync` case as a `#[test]`.
#[macro_export]
macro_rules! cloud_sync_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_CLOUD_SYNC,
            module = cloud_sync,
            cases = [
                activate_returns_the_granted_credential,
                an_unrecognised_code_is_refused,
                fetch_update_returns_the_published_artifact,
                an_unpublished_release_is_not_found,
                a_well_formed_report_is_accepted,
                a_report_with_no_self_test_is_accepted,
            ]
        }
    };
}

fn obligation() -> Obligation {
    Obligation::new(
        PortName::CloudSync,
        "request/response with the cloud: activate and fetch_update",
    )
}

/// A recognised code comes back as the device it names, with a credential to store.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn activate_returns_the_granted_credential<H: CloudSyncHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let channel = harness.fresh().await?;
    let grant = channel.activate(&harness.valid_code()).await?;
    let obligation = obligation();
    obligation.require_eq(
        &grant.device_id,
        &harness.granted_device(),
        "the grant names the activated device",
    )?;
    obligation.require(
        !grant.credential.expose().is_empty(),
        "the grant carries a non-empty credential to store in the vault",
    )
}

/// An unrecognised code is refused, and refusal is `PermissionDenied` — no oracle.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn an_unrecognised_code_is_refused<H: CloudSyncHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let channel = harness.fresh().await?;
    // A code the harness does not recognise; the fake accepts only its own `valid_code`.
    let outcome = channel.activate("XXXX-XXXX-XXXX").await;
    obligation().require_error(
        outcome,
        ErrorStatus::PermissionDenied,
        "an unrecognised code is refused, and a spent, revoked, or unknown code are indistinguishable",
    )
}

/// A published release's bytes come back intact, **and never without a signature**.
///
/// The second half is the obligation with teeth. `fetch_update` hands back a
/// [`SignedArtifact`](pos_ports::SignedArtifact) precisely so that a caller cannot end up holding
/// executable bytes with nothing to judge them ([ADR-0092](../../../docs/adr/0092-artifact-trust-chain.md)),
/// and a channel that returned an empty or wrong signature beside good bytes would satisfy the
/// intent of the type while defeating its purpose — the verify step would then fail on every
/// release, or worse, be skipped as "the transport's problem".
///
/// Whether the signature *verifies* is deliberately not checked here: that is
/// [`Signer`](pos_ports::Signer)'s question, a transport adapter holds no key, and asking a fake to
/// produce real Ed25519 output would test the fixture rather than the port.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn fetch_update_returns_the_published_artifact<H: CloudSyncHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let channel = harness.fresh().await?;
    let artifact = channel.fetch_update(&harness.known_release()).await?;
    let obligation = obligation();
    obligation.require_eq(
        &artifact.bytes,
        &harness.update_bytes(),
        "the published artifact bytes come back intact",
    )?;
    obligation.require(
        !artifact.signature.as_bytes().is_empty(),
        "an artifact came back with an empty signature, so nothing could ever verify it",
    )?;
    obligation.require_eq(
        &artifact.signature,
        &harness.update_signature(),
        "the signature that comes back is the one published beside the artifact",
    )
}

/// A release the cloud does not publish is `NotFound`, not an empty success.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn an_unpublished_release_is_not_found<H: CloudSyncHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let channel = harness.fresh().await?;
    let outcome = channel
        .fetch_update(&ReleaseTag::new("v0.0.0-does-not-exist"))
        .await;
    obligation().require_error(
        outcome,
        ErrorStatus::NotFound,
        "an unpublished release is a not-found error, so the caller does not install nothing",
    )
}

/// A well-formed update report is accepted. The report is telemetry, not a command, so a faithful
/// channel takes it — the closure that lets the cloud learn rollout progress
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)).
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_well_formed_report_is_accepted<H: CloudSyncHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let channel = harness.fresh().await?;
    let outcome = channel.report(&harness.sample_report()).await;
    obligation().require(
        outcome.is_ok(),
        "a well-formed update report is accepted, so the edge can tell the cloud what it is running",
    )
}

/// A report carrying **no** self-test is accepted too
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md) Amendment 1).
///
/// This is the shape most of a fleet is in most of the time: a store that has never installed an
/// update has no verdict, and a report exists chiefly to say which binary it is running. An
/// implementation that treated the absent verdict as malformed — refusing it, or requiring the field
/// on the wire — would leave the installed-version column empty for every store until its first
/// rollout, which is exactly when an operator most wants to see it. Binding it here means every
/// future `CloudSync` carries the obligation, not just today's adapter.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_report_with_no_self_test_is_accepted<H: CloudSyncHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let channel = harness.fresh().await?;
    let mut report = harness.sample_report();
    report.self_test_passed = None;
    let outcome = channel.report(&report).await;
    obligation().require(
        outcome.is_ok(),
        "a report with no self-test is accepted, so a store that has never updated can still say \
         which binary it runs",
    )
}
