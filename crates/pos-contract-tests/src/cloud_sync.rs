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

/// A published release's bytes come back intact.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn fetch_update_returns_the_published_artifact<H: CloudSyncHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let channel = harness.fresh().await?;
    let bytes = channel.fetch_update(&harness.known_release()).await?;
    obligation().require_eq(
        &bytes,
        &harness.update_bytes(),
        "the published artifact bytes come back intact",
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
