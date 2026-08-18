// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `ErpSink` suite.
//!
//! [`a_higher_revision_supersedes_rather_than_adds`] is the case with the worst failure attached.
//! An ERP that appends a repost instead of replacing it double-counts a day's revenue, and the
//! discrepancy surfaces in a finance close weeks later with nothing left to reconstruct it from.

use pos_ports::PortName;
use pos_ports::erp::{AccountCode, ErpBatch, ErpLine, ErpSink};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::{BusinessDate, ErrorStatus, StoreId};

use crate::harness::ErpSinkHarness;
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `ErpSink` case as a `#[test]`.
#[macro_export]
macro_rules! erp_sink_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_ERP_SINK,
            module = erp,
            cases = [
                posts_a_day,
                is_idempotent_by_revision,
                a_higher_revision_supersedes_rather_than_adds,
                refuses_an_unknown_account,
                reports_nothing_posted_for_an_unposted_day,
                keys_postings_on_the_trading_day,
            ]
        }
    };
}

fn idempotency() -> Obligation {
    Obligation::new(PortName::ErpSink, "idempotency by revision")
}

fn supersession() -> Obligation {
    Obligation::new(
        PortName::ErpSink,
        "a higher revision supersedes a lower one",
    )
}

fn validation() -> Obligation {
    Obligation::new(PortName::ErpSink, "a batch is posted whole or not at all")
}

fn batch(store_id: StoreId, account: AccountCode, revision: u32, amount_minor: i64) -> ErpBatch {
    ErpBatch {
        store_id,
        business_date: fixtures::business_date(),
        revision,
        lines: vec![ErpLine::Revenue {
            account_code: account,
            amount: Money::new(CurrencyCode::VND, amount_minor),
        }],
    }
}

/// A day posts and reports a document reference.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn posts_a_day<H: ErpSinkHarness>(harness: &H) -> Result<(), CaseFailure> {
    let erp = harness.fresh().await?;
    let posted = erp
        .post(&batch(
            harness.store_id(),
            harness.known_account(),
            0,
            12_000_000,
        ))
        .await?;
    let obligation = idempotency();
    obligation.require(
        !posted.document_ref.is_empty(),
        "a posting returns the ERP's document reference, which is what finance quotes",
    )?;
    obligation.require_eq(&posted.revision, &0, "and the revision it corresponds to")
}

/// The nightly job is retried, so a repeat must be harmless.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_idempotent_by_revision<H: ErpSinkHarness>(harness: &H) -> Result<(), CaseFailure> {
    let erp = harness.fresh().await?;
    let batch = batch(harness.store_id(), harness.known_account(), 0, 12_000_000);
    let first = erp.post(&batch).await?;
    let obligation = idempotency();

    match erp.post(&batch).await {
        Ok(second) => obligation.require_eq(
            &second.document_ref,
            &first.document_ref,
            "a repeated posting returns the same document rather than creating a second",
        ),
        Err(error) => obligation.require(
            error.status() == ErrorStatus::AlreadyExists,
            format!(
                "a repeated posting may report already_exists, which a caller treats as success. \
                 Anything else makes a retried nightly job look like a failure. Got {}",
                pos_proto::wire_enum::WireEnum::as_wire(error.status())
            ),
        ),
    }
}

/// A repost after a late correction replaces the day.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_higher_revision_supersedes_rather_than_adds<H: ErpSinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let erp = harness.fresh().await?;
    let account = harness.known_account();
    let obligation = supersession();

    erp.post(&batch(harness.store_id(), account.clone(), 0, 12_000_000))
        .await?;
    // A late void reduced the day. Revision 1 is the corrected figure, not an adjustment to add.
    erp.post(&batch(harness.store_id(), account, 1, 11_000_000))
        .await?;

    let posted = erp
        .posted(harness.store_id(), fixtures::business_date())
        .await?;
    let posted = obligation.require_nth(posted.as_slice(), 0, "the day's posting")?;
    obligation.require_eq(
        &posted.revision,
        &1,
        "the day reports the latest revision. An ERP that appended instead would show 23,000,000 \
         for a day that took 11,000,000, and the discrepancy surfaces in a finance close weeks \
         later with nothing left to reconstruct it from",
    )
}

/// An unknown account fails the whole batch.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_an_unknown_account<H: ErpSinkHarness>(harness: &H) -> Result<(), CaseFailure> {
    let erp = harness.fresh().await?;
    let obligation = validation();

    obligation.require_error(
        erp.post(&batch(
            harness.store_id(),
            harness.unknown_account(),
            0,
            12_000_000,
        ))
        .await,
        ErrorStatus::InvalidArgument,
        "an account the ERP does not know is invalid_argument, not something to retry",
    )?;

    let posted = erp
        .posted(harness.store_id(), fixtures::business_date())
        .await?;
    obligation.require(
        posted.is_none(),
        "and nothing was posted. Half a day's revenue in an accounting period is worse than none, \
         because none is visibly missing and half is not",
    )
}

/// An unposted day says so.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_nothing_posted_for_an_unposted_day<H: ErpSinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let erp = harness.fresh().await?;
    validation().require(
        erp.posted(harness.store_id(), fixtures::business_date())
            .await?
            .is_none(),
        "a day nothing was posted for reports None — that is what the nightly job checks before \
         deciding whether to post",
    )
}

/// The key is the trading day, and only the revision varies it.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn keys_postings_on_the_trading_day<H: ErpSinkHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let erp = harness.fresh().await?;
    let account = harness.known_account();
    let obligation = supersession();

    let today = batch(harness.store_id(), account.clone(), 0, 12_000_000);
    let yesterday = ErpBatch {
        business_date: BusinessDate::from_ymd(2025, 12, 31)
            .map_err(|error| CaseFailure::new(format!("fixture date: {error}")))?,
        ..batch(harness.store_id(), account, 0, 9_000_000)
    };
    obligation.require(
        today.idempotency_key() != yesterday.idempotency_key(),
        "two trading days are two postings",
    )?;

    erp.post(&today).await?;
    erp.post(&yesterday).await?;
    let looked_up = erp
        .posted(harness.store_id(), yesterday.business_date)
        .await?;
    obligation.require(
        looked_up.is_some(),
        "and each day is retrievable by its own trading date. A bar closing at 02:00 posts those \
         sales to the day it opened, so this key is the business date and never the calendar one",
    )
}
