// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `MessageLink` suite.
//!
//! The obligations that matter here are all about what happens when the link is *unhealthy*,
//! because a link that works needs no contract. Three of the five cases sever or fill it.
//!
//! The one that would be most expensive to get wrong is
//! [`never_discards_on_an_ambiguous_result`]: an adapter that responds to a timeout by dropping
//! events converts at-least-once delivery into silent data loss, and nothing downstream can
//! detect it — the cloud simply never sees those sales.

use pos_ports::PortName;
use pos_ports::message_link::MessageLink;
use pos_proto::protocol::{Hello, HelloOutcome};
use pos_proto::{ErrorStatus, MIN_SUPPORTED_PROTOCOL_VERSION, PROTOCOL_VERSION};

use crate::harness::MessageLinkHarness;
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `MessageLink` case as a `#[test]`.
#[macro_export]
macro_rules! message_link_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_MESSAGE_LINK,
            module = message_link,
            cases = [
                accepts_a_current_protocol_version,
                requires_a_handshake_before_publishing,
                accepts_a_batch_as_a_prefix,
                never_discards_on_an_ambiguous_result,
                reports_back_pressure_when_full,
                reports_capacity_for_the_eighty_percent_alert,
            ]
        }
    };
}

fn handshake_rule() -> Obligation {
    Obligation::new(PortName::MessageLink, "handshake once per connection")
}

fn prefix_rule() -> Obligation {
    Obligation::new(PortName::MessageLink, "acceptance is a prefix")
}

fn at_least_once() -> Obligation {
    Obligation::new(PortName::MessageLink, "at-least-once, never at-most-once")
}

fn back_pressure() -> Obligation {
    Obligation::new(PortName::MessageLink, "back-pressure rather than growth")
}

/// A handshake claiming the window this build actually supports.
///
/// Both ends of the window rather than one version, because that is what ADR-0024 negotiates —
/// and a suite that sent a single version would never exercise the overlap logic that makes a
/// rolling fleet upgrade possible.
fn hello(store_id: pos_proto::StoreId) -> Hello {
    Hello {
        protocol_version_min: MIN_SUPPORTED_PROTOCOL_VERSION,
        protocol_version_max: PROTOCOL_VERSION,
        product_version: pos_proto::ReleaseTag::new("v0.1.0"),
        store_id,
        lease_token: None,
    }
}

/// A build's own protocol version is accepted.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn accepts_a_current_protocol_version<H: MessageLinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let link = harness.fresh().await?;
    let outcome = link.handshake(&hello(harness.store_id())).await?;
    let obligation = handshake_rule();
    let HelloOutcome::Accepted { protocol_version } = outcome else {
        return obligation.require(
            false,
            format!("the current protocol version must be accepted; got {outcome:?}"),
        );
    };
    obligation.require(
        (MIN_SUPPORTED_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&protocol_version),
        format!(
            "the negotiated version must be inside the window this build offered              ({MIN_SUPPORTED_PROTOCOL_VERSION}..={PROTOCOL_VERSION}); got {protocol_version}"
        ),
    )
}

/// Publishing before a successful handshake is refused.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn requires_a_handshake_before_publishing<H: MessageLinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let link = harness.fresh().await?;
    let events = fixtures::activations(harness.store_id(), 1, 2);
    handshake_rule().require_error(
        link.publish(&events).await,
        ErrorStatus::FailedPrecondition,
        "publishing without a handshake must fail as a precondition, not as unavailability — a \
         caller retries unavailability forever and would never reach the handshake",
    )
}

/// The accepted count names a prefix of the batch.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn accepts_a_batch_as_a_prefix<H: MessageLinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let link = harness.fresh().await?;
    link.handshake(&hello(harness.store_id())).await?;

    let batch = fixtures::activations(harness.store_id(), 1, 5);
    let outcome = link.publish(&batch).await?;
    let obligation = prefix_rule();
    obligation.require(
        outcome.accepted <= 5,
        format!("accepted {} of a batch of 5", outcome.accepted),
    )?;
    obligation.require(
        outcome.is_complete(5),
        "a healthy link accepts a whole batch; a partial accept here means the caller's cursor \
         and the far side disagree about what landed",
    )?;

    // A batch larger than the link's own limit must be refused or truncated to a prefix, never
    // reported as fully accepted.
    let limit = link.max_batch_size().get();
    let oversized = fixtures::activations(harness.store_id(), 100, limit.saturating_add(1));
    match link.publish(&oversized).await {
        Ok(outcome) => obligation.require(
            outcome.accepted <= limit,
            format!(
                "a link whose max_batch_size is {limit} reported accepting {} events",
                outcome.accepted
            ),
        )?,
        Err(error) => obligation.require(
            error.status() == ErrorStatus::InvalidArgument
                || error.status() == ErrorStatus::ResourceExhausted,
            format!(
                "an oversized batch may be refused, but as invalid_argument or \
                 resource_exhausted rather than {}",
                pos_proto::wire_enum::WireEnum::as_wire(error.status())
            ),
        )?,
    }
    Ok(())
}

/// A severed link fails retryably and keeps nothing.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn never_discards_on_an_ambiguous_result<H: MessageLinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let link = harness.fresh().await?;
    link.handshake(&hello(harness.store_id())).await?;
    harness.sever(&link).await?;

    let batch = fixtures::activations(harness.store_id(), 1, 3);
    let obligation = at_least_once();
    match link.publish(&batch).await {
        Ok(outcome) => obligation.require(
            outcome.accepted == 0,
            format!(
                "a severed link reported accepting {} events. Reporting acceptance the far side \
                 never saw makes the caller acknowledge its outbox, and those sales are then \
                 gone with nothing able to detect it",
                outcome.accepted
            ),
        ),
        Err(error) => obligation.require(
            error.is_retryable(),
            format!(
                "a severed link must fail retryably so the outbox holds; got {}",
                pos_proto::wire_enum::WireEnum::as_wire(error.status())
            ),
        ),
    }
}

/// A full far side pushes back rather than accepting or blocking.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_back_pressure_when_full<H: MessageLinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let link = harness.fresh().await?;
    link.handshake(&hello(harness.store_id())).await?;
    harness.fill(&link).await?;

    let batch = fixtures::activations(harness.store_id(), 1, 1);
    back_pressure().require_error(
        link.publish(&batch).await,
        ErrorStatus::ResourceExhausted,
        "a full stream must report resource_exhausted, which is retryable and leaves the events \
         in the outbox. docs/capacity-and-reliability.md's row for this says \"sync halts; \
         events wait in store outboxes\" — that only happens if the status says so",
    )
}

/// Capacity is observable from the store side.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_capacity_for_the_eighty_percent_alert<H: MessageLinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let link = harness.fresh().await?;
    link.handshake(&hello(harness.store_id())).await?;
    let obligation = back_pressure();

    let before = link.capacity().await?;
    obligation.require(
        !before.is_at_least(80),
        "a fresh link must not already read as 80% full",
    )?;

    harness.fill(&link).await?;
    let after = link.capacity().await?;
    obligation.require(
        after.is_at_least(80),
        "a filled link must read as at least 80% full. A full stream halts synchronisation \
         silently while stores keep selling, so this number has to be visible from the store \
         rather than only from the broker's own metrics",
    )
}
