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

    /// A discount or comp would reduce the bill below nothing. The guest cannot be owed money by
    /// being sold food: taking money back after payment is a refund, which is its own signed
    /// movement ([ADR-0028](../../../docs/adr/0028-settlement-and-payment-invariant.md)) and not a
    /// reduction with a large number in it.
    ///
    /// Stated as "the reductions so far plus this one" rather than "this one", because two legal
    /// reductions can be illegal together and only the sum can say so.
    ReductionExceedsBill {
        /// What every reduction on the bill would come to, this one included.
        reduction_minor: i64,
        /// What there was to reduce.
        reducible_minor: i64,
    },

    /// A proposed split is not a partition of the bill's lines
    /// ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md) decision 3).
    ///
    /// One error rather than four, carrying which rule broke, because they are one question — *is
    /// this a partition?* — and a caller that has to handle four variants to say "that split does
    /// not add up" has been given the domain's reasoning instead of its answer.
    NotAPartition {
        /// Which rule the proposed split broke.
        reason: PartitionFault,
    },

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

    /// An action needs a store capability that this store's profile does not enable. The one gate
    /// [`CapabilityContext::require`](crate::capability::CapabilityContext::require) fails this way,
    /// which is why scattering `if flag` through the code is unnecessary and banned
    /// ([`docs/pos-spec.md` §10](../../../docs/pos-spec.md)).
    CapabilityDisabled {
        /// The disabled capability's config key, e.g. `seats_enabled`.
        capability: &'static str,
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
            Self::NotAPartition { reason } => write!(f, "the split is not a partition: {reason}"),
            Self::ReductionExceedsBill {
                reduction_minor,
                reducible_minor,
            } => write!(
                f,
                "reductions come to {reduction_minor} on a bill worth {reducible_minor}"
            ),
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
            Self::CapabilityDisabled { capability } => {
                write!(f, "capability disabled: {capability}")
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
            | Self::ReductionExceedsBill { .. }
            | Self::NotAPartition { .. }
            | Self::TaxRateNotConfigured { .. }
            | Self::Empty { .. }
            | Self::PermissionDenied { .. }
            | Self::CapabilityDisabled { .. } => None,
        }
    }
}

impl From<MoneyError> for DomainError {
    fn from(error: MoneyError) -> Self {
        Self::Money(error)
    }
}

/// Which of a partition's four rules a proposed split broke
/// ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md) decision 3).
///
/// Four rules and not one check, because each says something different to whoever has to fix it: a
/// split of one part is a screen that did not ask for a second, an empty part is a screen that let
/// somebody confirm nothing, an unbilled line is a stale screen, and a line left behind or claimed
/// twice is arithmetic that would charge a guest wrongly either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PartitionFault {
    /// Fewer than two parts. Splitting into one is not a split; it is the bill it already was.
    TooFewParts,
    /// A part covers no lines. A bill owing nothing is not something to hand a guest.
    EmptyPart,
    /// A part names a line the source bill does not cover — a screen acting on an order that has
    /// moved on, or on another bill's lines.
    LineNotOnTheBill,
    /// A line the source covers appears in no part, or in more than one.
    ///
    /// Both directions are this one fault because both are the same failure of arithmetic: the
    /// parts do not sum to the source. A line left behind would be food nobody is charged for; a
    /// line in two parts would be food charged twice.
    LinesDoNotPartition,
}

impl core::fmt::Display for PartitionFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::TooFewParts => "a split needs at least two parts",
            Self::EmptyPart => "a part covers no lines",
            Self::LineNotOnTheBill => "a part names a line this bill does not cover",
            Self::LinesDoNotPartition => {
                "every line must appear in exactly one part, and each part's lines must be the \
                 source's"
            }
        })
    }
}

impl From<TransitionError> for DomainError {
    fn from(error: TransitionError) -> Self {
        Self::Transition(error)
    }
}
