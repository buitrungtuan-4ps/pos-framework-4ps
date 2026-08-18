// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `PaymentTerminal` suite.
//!
//! [`an_ambiguous_result_is_never_a_decline`] is the most valuable case in this crate. An adapter
//! that maps a timeout onto `Declined` tells a cashier the card was refused, the cashier asks for
//! the card again, and the customer is charged twice — with the store's own records saying it
//! happened once. Nothing downstream can find that; only the customer's statement can.
//!
//! And [`is_idempotent_by_payment_id`] is invisible through the port: a deduplicated retry and a
//! second charge both return an attempt. Only
//! [`PaymentTerminalHarness::authorisation_count`] tells them apart.

use pos_ports::PortName;
use pos_ports::payment::{PaymentAttempt, PaymentRequest, PaymentTerminal};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::{BillId, ErrorStatus, PaymentId, PaymentMethod, PaymentOutcome, StoreId, Ulid};

use crate::harness::PaymentTerminalHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `PaymentTerminal` case as a `#[test]`.
#[macro_export]
macro_rules! payment_terminal_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_PAYMENT_TERMINAL,
            module = payment,
            cases = [
                captures_a_payment,
                reports_a_decline_without_taking_money,
                an_ambiguous_result_is_never_a_decline,
                an_ambiguous_result_always_carries_a_reference,
                is_idempotent_by_payment_id,
                resolves_an_ambiguous_result_by_looking_it_up,
                reports_an_unknown_reference_as_not_found,
            ]
        }
    };
}

fn unknown_branch() -> Obligation {
    Obligation::new(
        PortName::PaymentTerminal,
        "a timeout is Unknown, never Declined",
    )
}

fn idempotency() -> Obligation {
    Obligation::new(
        PortName::PaymentTerminal,
        "idempotency by payment identifier",
    )
}

fn resolution() -> Obligation {
    Obligation::new(PortName::PaymentTerminal, "look-up is the resolution path")
}

fn request(seed: u32) -> PaymentRequest {
    PaymentRequest {
        payment_id: PaymentId::new(Ulid::from_u128(u128::from(seed))),
        store_id: StoreId::new(Ulid::from_u128(1)),
        bill_id: BillId::new(Ulid::from_u128(1)),
        amount: Money::new(CurrencyCode::VND, 120_000),
        method: PaymentMethod::Card,
    }
}

/// The happy path, so the unhappy ones have something to differ from.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn captures_a_payment<H: PaymentTerminalHarness>(harness: &H) -> Result<(), CaseFailure> {
    let terminal = harness.fresh().await?;
    harness
        .stage_outcome(&terminal, PaymentOutcome::Captured)
        .await?;
    let attempt = terminal.authorize(&request(1)).await?;
    let obligation = unknown_branch();
    obligation.require(attempt.is_captured(), "a captured payment reports captured")?;
    obligation.require(attempt.is_resolved(), "and needs no reconciliation")?;
    obligation.require(
        !attempt.reference.as_str().is_empty(),
        "and still carries a reference, because reconciliation reads every attempt",
    )
}

/// A decline is a real answer, and distinguishable from no answer.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_a_decline_without_taking_money<H: PaymentTerminalHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let terminal = harness.fresh().await?;
    harness
        .stage_outcome(&terminal, PaymentOutcome::Declined)
        .await?;
    let attempt = terminal.authorize(&request(1)).await?;
    let obligation = unknown_branch();
    obligation.require(!attempt.is_captured(), "a decline moved no money")?;
    obligation.require(
        attempt.is_resolved(),
        "and it is a settled answer — the terminal was asked and said no, which is different \
         from not knowing",
    )
}

/// An indeterminate result stays indeterminate.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn an_ambiguous_result_is_never_a_decline<H: PaymentTerminalHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let terminal = harness.fresh().await?;
    harness.stage_unknown(&terminal).await?;

    let attempt = terminal.authorize(&request(1)).await;
    let obligation = unknown_branch();
    let attempt: PaymentAttempt = match attempt {
        Ok(attempt) => attempt,
        Err(error) => {
            return obligation.require(
                false,
                format!(
                    "the terminal was reached and could not say, which is a success value, not an \
                     error — a caller reads an error as \"nothing happened\" and will take the \
                     money again. Got: {error}"
                ),
            );
        }
    };

    obligation.require(
        !attempt.is_resolved(),
        "an ambiguous result is unresolved. Reporting it as declined tells a cashier to ask for \
         the card again, and the customer is then charged twice with the store's own records \
         saying once",
    )?;
    obligation.require(!attempt.is_captured(), "and it is not a capture either")?;
    obligation.require(
        attempt.needs_reconciliation(),
        "so it goes on the reconciliation list — docs/ui-ux.md §4 parks the bill amber with two \
         guided exits, and that list is where it comes from",
    )
}

/// An unresolvable attempt with no reference is unreconcilable.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn an_ambiguous_result_always_carries_a_reference<H: PaymentTerminalHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let terminal = harness.fresh().await?;
    harness.stage_unknown(&terminal).await?;
    let attempt = terminal.authorize(&request(1)).await?;
    unknown_branch().require(
        !attempt.reference.as_str().is_empty(),
        "an unknown outcome with no reference can never be resolved — the reconciliation job has \
         nothing to ask about, so the bill stays amber forever. An adapter that cannot produce a \
         reference must fail the call instead",
    )
}

/// Retrying the same request does not take the money twice.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_idempotent_by_payment_id<H: PaymentTerminalHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let terminal = harness.fresh().await?;
    harness
        .stage_outcome(&terminal, PaymentOutcome::Captured)
        .await?;
    let request = request(1);

    let first = terminal.authorize(&request).await?;
    let second = terminal.authorize(&request).await?;
    let obligation = idempotency();

    obligation.require_eq(
        &harness.authorisation_count(&terminal).await?,
        &1,
        "the same payment identifier must reach the acquirer once. This is invisible from the \
         port — both calls return an attempt — and on this port the difference is a customer \
         being charged twice",
    )?;
    obligation.require_eq(
        &second.reference,
        &first.reference,
        "and the retry reports the original attempt's reference",
    )
}

/// Looking up an ambiguous attempt is how it gets settled.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn resolves_an_ambiguous_result_by_looking_it_up<H: PaymentTerminalHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let terminal = harness.fresh().await?;
    harness.stage_unknown(&terminal).await?;
    let attempt = terminal.authorize(&request(1)).await?;
    let obligation = resolution();

    // Twice, because the nightly reconciliation runs every night and a resolution path that only
    // works once is a resolution path that fails on the second attempt at the same bill.
    let first = terminal.look_up(&attempt.reference).await?;
    let second = terminal.look_up(&attempt.reference).await?;
    obligation.require_eq(
        &first.reference,
        &attempt.reference,
        "a look-up returns the attempt it was asked about",
    )?;
    obligation.require_eq(
        &second.outcome.known(),
        &first.outcome.known(),
        "and repeating it gives the same answer — the reconciliation job runs nightly, and an \
         answer that changes on re-asking cannot settle anything",
    )
}

/// A reference the acquirer never saw means the money did not move.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_an_unknown_reference_as_not_found<H: PaymentTerminalHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let terminal = harness.fresh().await?;
    resolution().require_error(
        terminal
            .look_up(&pos_ports::PaymentReference::new("never-issued"))
            .await,
        ErrorStatus::NotFound,
        "an unknown reference is not_found, and for a reference the framework holds that means \
         the attempt never reached the acquirer — so the reconciliation can close the bill \
         rather than leaving it amber",
    )
}
