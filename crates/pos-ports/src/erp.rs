// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! ERP posting.
//!
//! `docs/architecture.md` §6.1: nightly posting of revenue and consumption. That cadence is
//! the design, not a compromise — an ERP is a system of record for accounting periods, and
//! posting a sale to it in real time would put a finance system on the sales path, which
//! [ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md) forbids for anything.
//!
//! # Posting is keyed by business date, not by calendar date
//!
//! This is the opposite of [`crate::Fiscalization`], and both are right. A tax authority
//! recognises calendar days. An accounting period recognises **trading** days, and a bar that
//! closes at 02:00 posts those sales to the day it opened. `docs/pos-spec.md` §4 names
//! computing daily figures in the wrong timezone as the classic revenue-skewing bug; posting to
//! the wrong day is the same bug wearing a different hat.
//!
//! # A posting is a claim about a period, so it has to be replaceable
//!
//! A late void, a corrected stocktake, or a reprocessed day changes what a period contained.
//! So a batch carries a `revision` and the ERP is expected to supersede rather than
//! accumulate — see [`ErpBatch`].

use core::fmt;
use core::future::Future;

use pos_proto::money::Money;
use pos_proto::{BusinessDate, Quantity, StoreId};

use crate::error::PortError;

/// The ERP's account code for a posting line.
///
/// Opaque text, because a chart of accounts belongs to the customer's finance function and any
/// structure the framework imposed on it would be wrong somewhere.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountCode(Box<str>);

impl AccountCode {
    /// Wraps an account code.
    #[must_use]
    pub fn new(code: impl Into<Box<str>>) -> Self {
        Self(code.into())
    }

    /// The code as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for AccountCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AccountCode({})", self.0)
    }
}

/// One line of a posting.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErpLine {
    /// Money earned, against an account.
    Revenue {
        /// Where it posts.
        account_code: AccountCode,
        /// How much.
        amount: Money,
    },
    /// Tax collected, against an account.
    Tax {
        /// Where it posts.
        account_code: AccountCode,
        /// How much.
        amount: Money,
    },
    /// Stock consumed, in quantity rather than money — the ERP values it, not this framework,
    /// because costing method is an accounting policy.
    Consumption {
        /// Where it posts.
        account_code: AccountCode,
        /// What was used, in thousandths.
        quantity: Quantity,
    },
}

impl ErpLine {
    /// The account this line posts to.
    #[must_use]
    pub const fn account_code(&self) -> &AccountCode {
        match self {
            Self::Revenue { account_code, .. }
            | Self::Tax { account_code, .. }
            | Self::Consumption { account_code, .. } => account_code,
        }
    }
}

/// A day's postings for one store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErpBatch {
    /// Which store.
    pub store_id: StoreId,
    /// Which **trading** day. See this module's documentation for why this is not a calendar
    /// date.
    pub business_date: BusinessDate,
    /// Which attempt at this day.
    ///
    /// Starts at zero and increases when a period is reposted after a late correction. The ERP
    /// is expected to supersede an earlier revision of the same
    /// `(store_id, business_date)` rather than add to it, and
    /// [`ErpSink::post`] returning [`PortError::already_exists`] for a revision it has already
    /// seen is how the framework learns a repost was unnecessary.
    pub revision: u32,
    /// The lines.
    pub lines: Vec<ErpLine>,
}

impl ErpBatch {
    /// The batch's idempotency key, as the ERP should treat it.
    ///
    /// Spelled out here rather than left to each adapter, because three adapters inventing
    /// three keys would make "post this day again" mean three different things.
    #[must_use]
    pub fn idempotency_key(&self) -> String {
        format!("{}:{}:{}", self.store_id, self.business_date, self.revision)
    }
}

/// The ERP's receipt for a posting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErpPostingRef {
    /// The ERP's own document reference, quoted when finance asks.
    pub document_ref: Box<str>,
    /// Which revision it corresponds to.
    pub revision: u32,
}

/// Posts revenue and consumption to an ERP.
///
/// # Contract
///
/// 1. **`post` is idempotent by [`ErpBatch::idempotency_key`].** Posting the same batch twice
///    posts once, and the second call returns the same [`ErpPostingRef`].
/// 2. **A higher revision supersedes a lower one for the same day.** An adapter that appends
///    instead produces double-counted revenue, which is the worst available failure in this
///    port.
/// 3. **A batch is posted whole or not at all.** Half a day's revenue in an accounting period
///    is worse than none, because none is visibly missing and half is not.
pub trait ErpSink: Send + Sync {
    /// Which ERP this is, for logs and per-adapter metrics.
    #[must_use]
    fn erp_name(&self) -> &'static str;

    /// Posts a day.
    ///
    /// # Errors
    ///
    /// [`PortError::already_exists`] if this exact revision has already posted — which a caller
    /// treats as success, and which is how a retried nightly job stays harmless;
    /// [`PortError::invalid_argument`] if an account code is unknown to the ERP;
    /// [`PortError::failed_precondition`] if the accounting period is closed, which is a
    /// finance conversation rather than a retry; [`PortError::unavailable`] if the ERP cannot be
    /// reached.
    fn post(
        &self,
        batch: &ErpBatch,
    ) -> impl Future<Output = Result<ErpPostingRef, PortError>> + Send;

    /// What was posted for a day, if anything.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the ERP cannot be reached.
    fn posted(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
    ) -> impl Future<Output = Result<Option<ErpPostingRef>, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{AccountCode, ErpBatch, ErpLine};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::{BusinessDate, Quantity, StoreId, Ulid};

    fn batch(revision: u32) -> ErpBatch {
        ErpBatch {
            store_id: StoreId::new(Ulid::from_u128(1)),
            business_date: BusinessDate::from_ymd(2026, 8, 18).expect("valid date"),
            revision,
            lines: vec![
                ErpLine::Revenue {
                    account_code: AccountCode::new("511"),
                    amount: Money::new(CurrencyCode::VND, 12_000_000),
                },
                ErpLine::Tax {
                    account_code: AccountCode::new("3331"),
                    amount: Money::new(CurrencyCode::VND, 1_200_000),
                },
                ErpLine::Consumption {
                    account_code: AccountCode::new("152"),
                    quantity: Quantity::from_milli(4_500),
                },
            ],
        }
    }

    #[test]
    fn every_line_kind_names_its_account() {
        for line in batch(0).lines {
            assert!(!line.account_code().as_str().is_empty());
        }
    }

    #[test]
    fn the_idempotency_key_changes_with_the_revision_and_nothing_else() {
        // Spelled out in one place so three adapters cannot invent three meanings for
        // "post this day again".
        let first = batch(0).idempotency_key();
        assert_eq!(first, batch(0).idempotency_key(), "same day, same key");
        assert_ne!(
            first,
            batch(1).idempotency_key(),
            "a repost after a late correction is a different posting"
        );
        assert!(first.ends_with(":0"));
    }

    #[test]
    fn the_key_is_keyed_on_the_trading_day() {
        // Not the calendar day. A bar closing at 02:00 posts those sales to the day it
        // opened, and getting this wrong is the revenue-skewing bug pos-spec.md §4 names.
        let key = batch(0).idempotency_key();
        assert!(key.contains("2026-08-18"), "got {key}");
    }
}
