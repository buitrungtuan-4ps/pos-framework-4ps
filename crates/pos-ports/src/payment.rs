// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Card terminals.
//!
//! # The unknown-result branch always exists
//!
//! `docs/architecture.md` §6.1 states it as a property of the domain, not of any particular
//! acquirer: a terminal can be asked and be unable to say whether the card was charged. The
//! cable is pulled, the terminal reboots, the network drops between authorisation and
//! response. No amount of adapter quality removes it.
//!
//! So [`pos_proto::PaymentOutcome::Unknown`] is a first-class success value, not an error.
//! The bill parks amber with two guided exits (`docs/ui-ux.md` §4), appears in the nightly
//! reconciliation list, and is resolved by [`PaymentTerminal::look_up`] — never by assuming.
//! An adapter that maps a timeout onto `Declined` is the single most expensive bug available
//! in this port: it tells a cashier to take the money again.
//!
//! # This framework never processes payments
//!
//! `docs/architecture.md`'s rejected list includes payment processing, and that is a
//! compliance boundary rather than a scope preference. No card number, expiry, CVV or track
//! data appears in any type here, and none may be added: the terminal holds the card data
//! and this port holds a reference to what the terminal did.

use core::fmt;
use core::future::Future;

use pos_proto::money::Money;
use pos_proto::wire_enum::Open;
use pos_proto::{BillId, PaymentId, PaymentMethod, PaymentOutcome, StoreId, Timestamp};

use crate::error::PortError;

/// The terminal's own reference for an attempt.
///
/// Opaque and vendor-defined. Stored so a later [`PaymentTerminal::look_up`] can resolve an
/// unknown result, which is the only reason it exists — and the reason an adapter must return
/// one even when it does not know the outcome. **An attempt with no reference is
/// unreconcilable**, so an adapter that cannot produce one must fail the call rather than
/// return a reference-less unknown.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaymentReference(Box<str>);

impl PaymentReference {
    /// Wraps a vendor reference.
    #[must_use]
    pub fn new(reference: impl Into<Box<str>>) -> Self {
        Self(reference.into())
    }

    /// The reference as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PaymentReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for PaymentReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PaymentReference({})", self.0)
    }
}

/// What the framework asks a terminal to do.
///
/// `payment_id` is the idempotency key, minted by the framework before the terminal is
/// touched. That ordering is deliberate: a key minted *after* a response cannot deduplicate
/// the response that never arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct PaymentRequest {
    /// Idempotency key. A retry of the same attempt reuses it.
    pub payment_id: PaymentId,
    /// Which store.
    pub store_id: StoreId,
    /// Which bill.
    pub bill_id: BillId,
    /// How much to take.
    pub amount: Money,
    /// Which method, for a terminal that supports more than cards.
    pub method: PaymentMethod,
}

/// What a terminal did, or could not say.
#[derive(Debug, Clone, PartialEq)]
pub struct PaymentAttempt {
    /// The attempt this describes.
    pub payment_id: PaymentId,
    /// The terminal's reference, for reconciliation. Always present — see
    /// [`PaymentReference`].
    pub reference: PaymentReference,
    /// What happened. `Open` because an acquirer may report a state this build predates, and
    /// an unrecognised token must not be silently read as a decline.
    pub outcome: Open<PaymentOutcome>,
    /// The amount the terminal reports. May differ from the request — a partial
    /// authorisation, or a tip added on the terminal's own keypad — so it is reported rather
    /// than assumed equal.
    pub amount: Money,
    /// When the terminal says it happened.
    pub at: Timestamp,
}

impl PaymentAttempt {
    /// Whether this attempt is settled either way.
    ///
    /// `false` for `Unknown`, for `Unspecified`, and for a token this build does not
    /// recognise — which is the point: all three are the same to a caller, in that the
    /// outcome is not known and must be reconciled.
    ///
    /// No separate check for an unrecognised token is needed, because
    /// [`Open::known`](pos_proto::wire_enum::Open::known) already reports `Unspecified` for
    /// one. That is the property that makes an acquirer's new status token fail safe here
    /// instead of reading as a decline.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        matches!(
            self.outcome.known(),
            PaymentOutcome::Captured | PaymentOutcome::Declined
        )
    }

    /// Whether the money moved.
    #[must_use]
    pub fn is_captured(&self) -> bool {
        self.outcome.known() == PaymentOutcome::Captured
    }

    /// Whether this attempt must go to the reconciliation list.
    #[must_use]
    pub fn needs_reconciliation(&self) -> bool {
        !self.is_resolved()
    }
}

/// Drives one card terminal.
///
/// # Contract
///
/// 1. **A timeout is `Unknown`, never `Declined`.** An adapter that cannot get an answer
///    returns [`PaymentOutcome::Unknown`] in the success path with a usable
///    [`PaymentReference`]. Returning [`PortError`] instead is acceptable **only** when the
///    terminal was never reached at all, because a caller treats an error as "nothing
///    happened".
/// 2. **`authorize` is idempotent by [`PaymentRequest::payment_id`].** Retrying the same
///    request must not take the money twice, whatever the first attempt's fate.
/// 3. **`look_up` is the resolution path and must be safe to call repeatedly**, including
///    hours later, including after a process restart. It is what the nightly reconciliation
///    in `docs/architecture.md` §8 runs.
/// 4. **No card data crosses this boundary**, in either direction, ever.
pub trait PaymentTerminal: Send + Sync {
    /// Asks the terminal to take a payment.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] **only** if the terminal was never reached — that is the
    /// one case a caller may read as "nothing happened". Anything ambiguous belongs in the
    /// success path as [`PaymentOutcome::Unknown`]. [`PortError::invalid_argument`] for an
    /// amount or method the terminal cannot accept.
    fn authorize(
        &self,
        request: &PaymentRequest,
    ) -> impl Future<Output = Result<PaymentAttempt, PortError>> + Send;

    /// Asks what became of an attempt.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the terminal or acquirer cannot be reached, or
    /// [`PortError::not_found`] if the reference is unknown to them — which, for a reference
    /// the framework holds, means the attempt never reached the acquirer and the money did
    /// not move.
    fn look_up(
        &self,
        reference: &PaymentReference,
    ) -> impl Future<Output = Result<PaymentAttempt, PortError>> + Send;

    /// Reverses an attempt that has not yet settled.
    ///
    /// Distinct from a refund, which is a new movement of money against a settled payment and
    /// belongs to the domain rather than to the terminal.
    ///
    /// # Errors
    ///
    /// [`PortError::failed_precondition`] if the attempt has already settled,
    /// [`PortError::not_found`] if the reference is unknown, or
    /// [`PortError::unavailable`] if the terminal cannot be reached.
    fn void(
        &self,
        reference: &PaymentReference,
    ) -> impl Future<Output = Result<PaymentAttempt, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{PaymentAttempt, PaymentReference};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::wire_enum::Open;
    use pos_proto::{PaymentId, PaymentOutcome, Timestamp, Ulid};

    fn attempt(outcome: Open<PaymentOutcome>) -> PaymentAttempt {
        PaymentAttempt {
            payment_id: PaymentId::new(Ulid::from_u128(1)),
            reference: PaymentReference::new("acquirer-ref-1"),
            outcome,
            amount: Money::new(CurrencyCode::VND, 120_000),
            at: Timestamp::from_milliseconds_since_epoch(1_767_225_600_000).expect("builds"),
        }
    }

    #[test]
    fn an_unknown_outcome_is_a_success_that_needs_reconciliation() {
        // The costliest possible bug in this port is mapping a timeout onto Declined: it
        // tells a cashier to take the money a second time.
        let unknown = attempt(Open::from_known(PaymentOutcome::Unknown));
        assert!(!unknown.is_resolved());
        assert!(!unknown.is_captured());
        assert!(unknown.needs_reconciliation());
    }

    #[test]
    fn a_settled_outcome_needs_nothing_further() {
        let captured = attempt(Open::from_known(PaymentOutcome::Captured));
        assert!(captured.is_resolved());
        assert!(captured.is_captured());
        assert!(!captured.needs_reconciliation());

        let declined = attempt(Open::from_known(PaymentOutcome::Declined));
        assert!(declined.is_resolved());
        assert!(!declined.is_captured());
        assert!(!declined.needs_reconciliation());
    }

    #[test]
    fn a_token_from_the_future_reconciles_rather_than_declines() {
        // An acquirer adding PAYMENT_OUTCOME_REVERSED must not make an older build conclude
        // "not captured" — it must make it conclude "I do not know".
        let future = attempt(Open::parse("PAYMENT_OUTCOME_REVERSED"));
        assert!(future.outcome.is_unrecognised());
        assert!(!future.is_resolved());
        assert!(!future.is_captured());
        assert!(future.needs_reconciliation());
    }

    #[test]
    fn an_unspecified_outcome_is_also_unresolved() {
        let absent = attempt(Open::from_known(PaymentOutcome::Unspecified));
        assert!(!absent.is_resolved());
        assert!(absent.needs_reconciliation());
    }

    #[test]
    fn a_reference_prints_for_an_operator_reconciling_by_hand() {
        let reference = PaymentReference::new("A1B2C3");
        assert_eq!(reference.to_string(), "A1B2C3");
        assert_eq!(format!("{reference:?}"), "PaymentReference(A1B2C3)");
    }
}
