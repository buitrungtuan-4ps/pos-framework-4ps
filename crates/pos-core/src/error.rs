// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The one error type the domain returns.
//!
//! `pos-core` is sans-I/O, so its errors are never about a database or a network — they are about a
//! rule the caller's inputs broke: money that overflowed, payments that do not sum to the total, a
//! tax class nobody configured a rate for, a transition the state machine refuses. Each variant
//! names the rule, so a caller (and a test) learns *which* invariant failed rather than only that
//! one did.

use pos_proto::money::MoneyError;

use crate::state_machine::TransitionError;

/// A domain rule was broken by the inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DomainError {
    /// Money arithmetic overflowed or mixed currencies. Wraps the primitive's own error so the
    /// specific cause survives.
    Money(MoneyError),

    /// A state machine refused a transition — a settled bill asked to settle again, a closed shift
    /// asked to take a transaction.
    Transition(TransitionError),

    /// The applied payments do not sum to what is owed. `SETTLED` requires exact equality
    /// ([ADR-0028](../../../docs/adr/0028-settlement-and-payment-invariant.md)); this is the
    /// violation.
    PaymentsDoNotSumToTotal {
        /// What the payments applied to the bill.
        applied_minor: i64,
        /// What the bill owed.
        total_due_minor: i64,
    },

    /// Change would be negative: the guest tendered less than was applied plus tipped. A sign that
    /// `applied_to_bill` was set above `tendered`, which is not a real payment.
    NegativeChange,

    /// A line carries a tax class the store's rate table does not price on this channel. Silently
    /// charging no tax would be an audit finding, so the domain refuses instead
    /// ([ADR-0028](../../../docs/adr/0028-settlement-and-payment-invariant.md)).
    TaxRateNotConfigured {
        /// The class with no rate, as its ULID text.
        tax_class_id: String,
        /// The channel it was looked up on, as its wire token.
        sales_channel: String,
    },

    /// An input that should have carried at least one element was empty — a bill with no lines, a
    /// settlement with no payments.
    Empty {
        /// What was empty, for the message.
        what: &'static str,
    },

    /// The acting role's [`PermissionSet`](crate::permission::PermissionSet) does not grant the
    /// permission the action needs. Deny by default: every gated action fails this way unless the
    /// set explicitly carries the permission
    /// ([`docs/pos-spec.md` §9](../../../docs/pos-spec.md)).
    PermissionDenied {
        /// The denied permission's stable id, e.g. `billing.bill.void`.
        permission: &'static str,
    },
}

impl core::fmt::Display for DomainError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Money(error) => write!(f, "money arithmetic: {error}"),
            Self::Transition(error) => write!(f, "{error}"),
            Self::PaymentsDoNotSumToTotal {
                applied_minor,
                total_due_minor,
            } => write!(
                f,
                "payments applied {applied_minor} but the bill owes {total_due_minor}"
            ),
            Self::NegativeChange => {
                f.write_str("change would be negative: tendered is less than applied plus tips")
            }
            Self::TaxRateNotConfigured {
                tax_class_id,
                sales_channel,
            } => write!(
                f,
                "no tax rate configured for class {tax_class_id} on channel {sales_channel}"
            ),
            Self::Empty { what } => write!(f, "{what} must not be empty"),
            Self::PermissionDenied { permission } => {
                write!(f, "permission denied: {permission}")
            }
        }
    }
}

impl core::error::Error for DomainError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Money(error) => Some(error),
            Self::Transition(error) => Some(error),
            Self::PaymentsDoNotSumToTotal { .. }
            | Self::NegativeChange
            | Self::TaxRateNotConfigured { .. }
            | Self::Empty { .. }
            | Self::PermissionDenied { .. } => None,
        }
    }
}

impl From<MoneyError> for DomainError {
    fn from(error: MoneyError) -> Self {
        Self::Money(error)
    }
}

impl From<TransitionError> for DomainError {
    fn from(error: TransitionError) -> Self {
        Self::Transition(error)
    }
}
