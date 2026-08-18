// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `Signer` suite.
//!
//! Synchronous, like the port. Two of the four cases exist because a *wrong key* and a *bad
//! signature* mean different things and the port promises different statuses for them: the first
//! says "try the other baked-in key", the second says "this artifact is not what it claims to be".
//! Collapsing them makes a two-key rollout indistinguishable from an attack.
//!
//! [`is_total_over_hostile_input`] is the case that pays for the backbone's `-F clippy::panic` and
//! `-F clippy::indexing_slicing` passes: verification runs on bytes an attacker chose, at startup,
//! before anything else works.

use pos_ports::PortName;
use pos_ports::signer::{Signature, Signer};
use pos_proto::ErrorStatus;

use crate::harness::SignerHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `Signer` case as a `#[test]`.
#[macro_export]
macro_rules! signer_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_SIGNER,
            module = signer,
            cases = [
                verifies_a_valid_signature,
                rejects_a_tampered_artifact,
                distinguishes_a_wrong_key_from_a_bad_signature,
                reports_the_key_a_signature_claims,
                is_total_over_hostile_input,
            ]
        }
    };
}

fn verification() -> Obligation {
    Obligation::new(PortName::Signer, "a signature is valid or it is an error")
}

fn key_selection() -> Obligation {
    Obligation::new(
        PortName::Signer,
        "a key-id mismatch is not a verification failure",
    )
}

fn totality() -> Obligation {
    Obligation::new(PortName::Signer, "verification is total")
}

/// The happy path.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn verifies_a_valid_signature<H: SignerHarness>(harness: &H) -> Result<(), CaseFailure> {
    let signer = harness.fresh().await?;
    let (artifact, signature, key) = harness.valid_triple()?;
    signer.verify(&artifact, &signature, &key)?;
    Ok(())
}

/// One flipped byte and it fails.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn rejects_a_tampered_artifact<H: SignerHarness>(harness: &H) -> Result<(), CaseFailure> {
    let signer = harness.fresh().await?;
    let (artifact, signature, key) = harness.valid_triple()?;
    let obligation = verification();

    let mut tampered = artifact.clone();
    match tampered.last_mut() {
        Some(byte) => *byte ^= 0x01,
        None => tampered.push(0x01),
    }
    obligation.require_error(
        signer.verify(&tampered, &signature, &key),
        ErrorStatus::PermissionDenied,
        "a modified artifact must fail as permission_denied, which is not retryable — an update \
         whose signature does not verify must never be retried into being installed",
    )?;

    // And appended bytes, which is the shape a real attack takes: keep the signed prefix and add
    // a payload after it.
    let mut extended = artifact;
    extended.extend_from_slice(b"appended");
    obligation.require_error(
        signer.verify(&extended, &signature, &key),
        ErrorStatus::PermissionDenied,
        "appending to a signed artifact must fail too — signing a prefix is not signing the file",
    )
}

/// Wrong key and bad signature are different answers.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn distinguishes_a_wrong_key_from_a_bad_signature<H: SignerHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let signer = harness.fresh().await?;
    let (artifact, signature, _) = harness.valid_triple()?;
    let other = harness.other_key()?;
    key_selection().require_error(
        signer.verify(&artifact, &signature, &other),
        ErrorStatus::InvalidArgument,
        "verifying against the wrong key is invalid_argument, meaning \"try the other one\". \
         Reporting it as permission_denied makes a two-key rollout — the whole reason both public \
         keys are baked into the binary — indistinguishable from an attack",
    )
}

/// A signature names its key, so revocation can be checked before trust.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_the_key_a_signature_claims<H: SignerHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let signer = harness.fresh().await?;
    let (_, signature, key) = harness.valid_triple()?;
    key_selection().require_eq(
        &signer.key_id_of(&signature)?,
        &key.key_id(),
        "the key id read from a signature matches the key that verifies it. This is what lets a \
         revocation check happen before the artifact is trusted rather than after",
    )
}

/// No input panics.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_total_over_hostile_input<H: SignerHarness>(harness: &H) -> Result<(), CaseFailure> {
    let signer = harness.fresh().await?;
    let (artifact, valid, key) = harness.valid_triple()?;
    let obligation = totality();

    // Empty, truncated, one byte, and far too long. Every one of these arrives from a network at
    // startup, and a panic here is a store that will not boot.
    let mut hostile = vec![
        Signature::new(Vec::new()),
        Signature::new(vec![0]),
        Signature::new(vec![0xff; 4096]),
    ];
    for cut in [1_usize, 2, 8] {
        let bytes = valid.as_bytes();
        if let Some(prefix) = bytes.get(..bytes.len().saturating_sub(cut)) {
            hostile.push(Signature::new(prefix.to_vec()));
        }
    }

    for signature in &hostile {
        let outcome = signer.verify(&artifact, signature, &key);
        obligation.require(
            outcome.is_err(),
            format!(
                "a malformed signature of {} bytes must be rejected, not accepted",
                signature.as_bytes().len()
            ),
        )?;
        // key_id_of must also be total. It runs before verification, on the same hostile bytes.
        let _ = signer.key_id_of(signature);
    }

    // An empty artifact is legitimate input too — a zero-byte release is absurd but reachable.
    obligation.require(
        signer.verify(&[], &valid, &key).is_err(),
        "an empty artifact does not verify against a real signature",
    )
}
