// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `einvoice` node and the invoice a settle issues
//! ([ADR-0225](../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md),
//! [ADR-0229](../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md)).
//!
//! When an invoice is issued is a store's schedule, `einvoice.issue_moment` with its delay and batch
//! minute (ADR-0229 decision 1), so it is a setting (ADR-0160), seeded by the country pack. How late
//! the law allows is the country's, and is not here. The symbol a store numbers under is the cloud's
//! to give each issuing point (decision 2), so it travels on the same node but is not a setting: the
//! console never edits it. That the sequence is gapless, that a bill has one invoice, and that no
//! buyer detail enters an event are fixed rules, and are not here.

use serde::{Deserialize, Serialize};

use crate::ids::SubjectId;

use crate::money::Money;
use crate::time::{CalendarDate, Timestamp};
use crate::ulid::Ulid;
use crate::wire_enum;
use crate::wire_enum::{Open, WireEnum};

mod correction;
pub use correction::{
    CORRECTION_DECISIONS_MAX, CorrectionDecision, CorrectionRequest, DecidedCorrection,
};

wire_enum! {
    /// When a settled bill issues an e-invoice (ADR-0229 decision 1).
    EInvoiceIssueMoment, prefix = "EINVOICE_ISSUE_MOMENT";
    /// None is issued: the default, and every store before the setting.
    Off = "OFF",
    /// In the settle's own transaction. What the Vietnamese pack seeds.
    Immediate = "IMMEDIATE",
    /// `delay_minutes` after the settle, bill by bill.
    ScheduledPerOrder = "SCHEDULED_PER_ORDER",
    /// Together with every bill waiting, at the store's daily `batch_minute`.
    BatchWindow = "BATCH_WINDOW",
    /// Only a bill whose guest gave buyer details, at once.
    OnRequest = "ON_REQUEST",
}

impl EInvoiceIssueMoment {
    /// Whether a bill settled under this moment issues an invoice at all.
    #[must_use]
    pub const fn issues(self, has_buyer: bool) -> bool {
        match self {
            Self::Immediate | Self::ScheduledPerOrder | Self::BatchWindow => true,
            Self::OnRequest => has_buyer,
            Self::Unspecified | Self::Off => false,
        }
    }

    /// Whether the invoice waits for a later moment rather than issuing with the settle.
    #[must_use]
    pub const fn is_deferred(self) -> bool {
        matches!(self, Self::ScheduledPerOrder | Self::BatchWindow)
    }
}

wire_enum! {
    /// Whether the cloud allocates a store's invoice symbol by itself
    /// ([ADR-0240](../../../docs/adr/0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md)
    /// decision 4). The cloud reads it; the edge does not.
    EInvoiceSymbolAllocation, prefix = "EINVOICE_SYMBOL_ALLOCATION";
    /// HQ gives each symbol, by hand or with the console's allocate button. The default.
    Manual = "MANUAL",
    /// The cloud allocates one when a box is activated for a store with none, and with each lease
    /// bump. What the Vietnamese pack seeds.
    Automatic = "AUTOMATIC",
}

wire_enum! {
    /// How a bill with an issued invoice is voided or refunded
    /// ([ADR-0245](../../../docs/adr/0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md)
    /// decision 1). Every statement of law behind it is to be confirmed by legal or the accountant.
    InvoicedCorrection, prefix = "INVOICED_CORRECTION";
    /// The till refuses; HQ corrects in the console. The default, and every store before it.
    HqOnly = "HQ_ONLY",
    /// A manager requests it from the till, and HQ or the accountant approves it in a queue.
    Request = "REQUEST",
    /// A manager voids at the till up to `manager_correction_limit`; the cloud corrects.
    ManagerUpToLimit = "MANAGER_UP_TO_LIMIT",
    /// A manager voids at the till while the invoice is younger than `cancel_window_minutes`.
    CancelWindow = "CANCEL_WINDOW",
}

/// The values `manager_correction_limit` may take, in the minor unit.
pub const MANAGER_CORRECTION_LIMIT: std::ops::RangeInclusive<i64> = 0..=1_000_000_000_000;

/// The values `cancel_window_minutes` may take: up to a day.
pub const CANCEL_WINDOW_MINUTES: std::ops::RangeInclusive<i64> = 0..=1_440;

/// The latest a country's law lets an invoice be issued after its sale (ADR-0229 decision 2): fixed
/// law from the country pack, which the cloud refuses a setting past and the edge clamps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueLimit {
    /// By the end of the sale's calendar day, in the store's timezone.
    EndOfSaleDay,
    /// Within this many minutes of the sale.
    MinutesAfterSale(u32),
}

impl IssueLimit {
    /// The longest `delay_minutes` the limit can ever allow: a delay above it crosses the limit
    /// whenever the sale was made.
    #[must_use]
    pub const fn max_delay_minutes(self) -> u32 {
        match self {
            Self::EndOfSaleDay => 1_439,
            Self::MinutesAfterSale(minutes) => minutes,
        }
    }
}

/// The values `delay_minutes` may take: up to a day.
pub const DELAY_MINUTES: std::ops::RangeInclusive<i64> = 0..=1_440;

/// The values `batch_minute` may take: a minute of the local day.
pub const BATCH_MINUTE: std::ops::RangeInclusive<i64> = 0..=1_439;

/// The batch minute where the store sets none: 22:00.
pub const DEFAULT_BATCH_MINUTE: i64 = 1_320;

wire_enum! {
    /// What an invoice is (ADR-0225 decision 1). A store issues originals; the cloud issues the
    /// corrections.
    EInvoiceKind, prefix = "EINVOICE_KIND";
    /// The first invoice for a sale.
    Original = "ORIGINAL",
    /// Changes an earlier invoice's amounts, up or down.
    Adjustment = "ADJUSTMENT",
    /// Takes an earlier invoice's place.
    Replacement = "REPLACEMENT",
}

/// The symbol an invoice is numbered under, such as Vietnam's `1C26MAA`.
///
/// A newtype, not text, so the event log's no-personal-data rule holds: it is at most 16 ASCII
/// letters, digits, `/` or `-`. The country checks its own shape before the cloud publishes one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(try_from = "String", into = "String")]
pub struct InvoiceSymbol(Box<str>);

impl InvoiceSymbol {
    /// The longest symbol any country writes here.
    pub const MAX_LEN: usize = 16;

    /// A symbol from `text`, trimmed, or `None` if it is empty, too long, or not letters, digits,
    /// `/` and `-`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let allowed = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'/' || byte == b'-';
        (!text.is_empty() && text.len() <= Self::MAX_LEN && text.bytes().all(allowed))
            .then(|| Self(text.into()))
    }

    /// The symbol as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InvoiceSymbol {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text).ok_or("an invoice symbol is 1 to 16 letters, digits, `/` or `-`")
    }
}

impl From<InvoiceSymbol> for String {
    fn from(symbol: InvoiceSymbol) -> Self {
        symbol.0.into()
    }
}

/// The `einvoice` node.
///
/// No `deny_unknown_fields`, as for every published node. A value of the wrong kind reads as absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct PublishedEInvoice {
    /// When a settled bill issues an invoice. Read it through [`Self::issue_moment`].
    #[serde(default, deserialize_with = "token", skip_serializing_if = "is_absent")]
    pub issue_moment: Open<EInvoiceIssueMoment>,
    /// How long after its settle a bill is issued under `SCHEDULED_PER_ORDER`, in minutes. Read it
    /// through [`Self::delay_minutes`].
    #[serde(
        default,
        deserialize_with = "whole",
        skip_serializing_if = "Option::is_none"
    )]
    pub delay_minutes: Option<i64>,
    /// The minute of the local day a `BATCH_WINDOW` store issues at. Read it through
    /// [`Self::batch_minute`].
    #[serde(
        default,
        deserialize_with = "whole",
        skip_serializing_if = "Option::is_none"
    )]
    pub batch_minute: Option<i64>,
    /// The symbol this store numbers its invoices under, which the cloud gives each issuing point.
    /// Absent until the cloud publishes one, and then a store that must issue refuses to settle.
    #[serde(
        default,
        deserialize_with = "text",
        skip_serializing_if = "Option::is_none"
    )]
    pub symbol: Option<String>,
    /// Whether the cloud allocates the symbol by itself. Read it through
    /// [`Self::symbol_allocation`].
    #[serde(default, deserialize_with = "token", skip_serializing_if = "is_absent")]
    pub symbol_allocation: Open<EInvoiceSymbolAllocation>,
    /// What the cloud issues when an invoiced bill is voided or refunded
    /// ([ADR-0245](../../../docs/adr/0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md)
    /// decision 2). Read it through [`Self::correction_kind`].
    #[serde(default, deserialize_with = "token", skip_serializing_if = "is_absent")]
    pub correction_kind: Open<EInvoiceKind>,
    /// How an invoiced bill is voided (ADR-0245 decision 1). Read it through
    /// [`Self::invoiced_correction`].
    #[serde(default, deserialize_with = "token", skip_serializing_if = "is_absent")]
    pub invoiced_correction: Open<InvoicedCorrection>,
    /// The bill total a manager may void at the till under `MANAGER_UP_TO_LIMIT`, in the minor
    /// unit. Read it through [`Self::manager_correction_limit`].
    #[serde(
        default,
        deserialize_with = "whole",
        skip_serializing_if = "Option::is_none"
    )]
    pub manager_correction_limit: Option<i64>,
    /// How many minutes after its issue an invoice may still be voided at the till under
    /// `CANCEL_WINDOW`. Read it through [`Self::cancel_window_minutes`].
    #[serde(
        default,
        deserialize_with = "whole",
        skip_serializing_if = "Option::is_none"
    )]
    pub cancel_window_minutes: Option<i64>,
    /// Whether a correction request needs a reason (ADR-0245 decision 2). Read it through
    /// [`Self::correction_reason_required`].
    #[serde(
        default,
        deserialize_with = "flag",
        skip_serializing_if = "Option::is_none"
    )]
    pub correction_reason_required: Option<bool>,
    /// HQ's decisions on this store's correction requests, newest last, at most
    /// [`CORRECTION_DECISIONS_MAX`]. Written by the cloud on the Store layer alone, and not a
    /// setting ([ADR-0250](../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)).
    #[serde(
        default,
        deserialize_with = "correction::decisions",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub correction_decisions: Vec<DecidedCorrection>,
}

impl PublishedEInvoice {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "einvoice";

    /// When a settled bill issues an invoice: the node's moment, and [`EInvoiceIssueMoment::Off`]
    /// for an absent, unspecified or unknown one.
    #[must_use]
    pub fn issue_moment(&self) -> EInvoiceIssueMoment {
        match self.issue_moment.known() {
            EInvoiceIssueMoment::Unspecified | EInvoiceIssueMoment::Off => EInvoiceIssueMoment::Off,
            EInvoiceIssueMoment::Immediate => EInvoiceIssueMoment::Immediate,
            EInvoiceIssueMoment::ScheduledPerOrder => EInvoiceIssueMoment::ScheduledPerOrder,
            EInvoiceIssueMoment::BatchWindow => EInvoiceIssueMoment::BatchWindow,
            EInvoiceIssueMoment::OnRequest => EInvoiceIssueMoment::OnRequest,
        }
    }

    /// The delay after settle: the node's minutes within [`DELAY_MINUTES`], and 0 otherwise.
    #[must_use]
    pub fn delay_minutes(&self) -> u32 {
        self.delay_minutes
            .filter(|minutes| DELAY_MINUTES.contains(minutes))
            .and_then(|minutes| u32::try_from(minutes).ok())
            .unwrap_or(0)
    }

    /// The daily batch minute: the node's within [`BATCH_MINUTE`], and 22:00 otherwise.
    #[must_use]
    pub fn batch_minute(&self) -> u32 {
        self.batch_minute
            .filter(|minute| BATCH_MINUTE.contains(minute))
            .and_then(|minute| u32::try_from(minute).ok())
            .unwrap_or(1_320)
    }

    /// Whether the cloud allocates the symbol by itself: [`EInvoiceSymbolAllocation::Automatic`]
    /// only when the node says so, and [`EInvoiceSymbolAllocation::Manual`] otherwise.
    #[must_use]
    pub fn symbol_allocation(&self) -> EInvoiceSymbolAllocation {
        match self.symbol_allocation.known() {
            EInvoiceSymbolAllocation::Automatic => EInvoiceSymbolAllocation::Automatic,
            EInvoiceSymbolAllocation::Unspecified | EInvoiceSymbolAllocation::Manual => {
                EInvoiceSymbolAllocation::Manual
            }
        }
    }

    /// The kind of correction the cloud issues for a voided invoiced bill:
    /// [`EInvoiceKind::Replacement`] only when the node says so, and [`EInvoiceKind::Adjustment`]
    /// otherwise. Which errors need which kind is to be confirmed by legal or the accountant.
    #[must_use]
    pub fn correction_kind(&self) -> EInvoiceKind {
        match self.correction_kind.known() {
            EInvoiceKind::Replacement => EInvoiceKind::Replacement,
            EInvoiceKind::Unspecified | EInvoiceKind::Original | EInvoiceKind::Adjustment => {
                EInvoiceKind::Adjustment
            }
        }
    }

    /// How an invoiced bill is voided: the node's route, and [`InvoicedCorrection::HqOnly`] for an
    /// absent, unspecified or unknown one.
    #[must_use]
    pub fn invoiced_correction(&self) -> InvoicedCorrection {
        match self.invoiced_correction.known() {
            InvoicedCorrection::Request => InvoicedCorrection::Request,
            InvoicedCorrection::ManagerUpToLimit => InvoicedCorrection::ManagerUpToLimit,
            InvoicedCorrection::CancelWindow => InvoicedCorrection::CancelWindow,
            InvoicedCorrection::Unspecified | InvoicedCorrection::HqOnly => {
                InvoicedCorrection::HqOnly
            }
        }
    }

    /// The manager's limit: the node's within [`MANAGER_CORRECTION_LIMIT`], and 0 otherwise.
    #[must_use]
    pub fn manager_correction_limit(&self) -> i64 {
        self.manager_correction_limit
            .filter(|limit| MANAGER_CORRECTION_LIMIT.contains(limit))
            .unwrap_or(0)
    }

    /// Whether a correction request needs a reason: on unless the node turns it off.
    #[must_use]
    pub fn correction_reason_required(&self) -> bool {
        self.correction_reason_required.unwrap_or(true)
    }

    /// HQ's latest decision on `bill_id`'s correction request, if the cloud has published one.
    #[must_use]
    pub fn correction_decision(&self, bill_id: crate::ids::BillId) -> Option<&DecidedCorrection> {
        self.correction_decisions
            .iter()
            .rev()
            .find(|decided| decided.bill_id == bill_id)
    }

    /// The cancel window: the node's minutes within [`CANCEL_WINDOW_MINUTES`], and 0 otherwise.
    #[must_use]
    pub fn cancel_window_minutes(&self) -> i64 {
        self.cancel_window_minutes
            .filter(|minutes| CANCEL_WINDOW_MINUTES.contains(minutes))
            .unwrap_or(0)
    }

    /// The symbol to number under, or `None` while the cloud has published none, or one that is
    /// not a symbol.
    #[must_use]
    pub fn symbol(&self) -> Option<InvoiceSymbol> {
        self.symbol.as_deref().and_then(InvoiceSymbol::parse)
    }
}

/// The invoice a settle issued, as `fiscal.invoice.issued` carries it (ADR-0225 decision 5).
///
/// No personal data: a buyer is named by `buyer_subject_id`, and the details stay in the subject
/// store. The figures by rate are the settled bill's own `tax_lines`, committed in the same
/// transaction, so they are not repeated here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct FiscalInvoice {
    /// What the invoice is. A store issues `EINVOICE_KIND_ORIGINAL`.
    pub kind: Open<EInvoiceKind>,
    /// The symbol it is numbered under.
    pub invoice_symbol: InvoiceSymbol,
    /// Its place in the symbol's gapless sequence, from 1.
    pub invoice_sequence: u32,
    /// The calendar date it was issued, never the business date.
    pub issued_on: CalendarDate,
    /// What the guest owes, as the settled bill says.
    pub total: Money,
    /// The buyer it names, when the guest gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buyer_subject_id: Option<SubjectId>,
    /// The e-invoice connection that served the store when it was issued, from the `integrations`
    /// node: the one the cloud submits it through, so a provider switch reaches only later invoices
    /// (ADR-0229 decision 5). Absent where none was published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<Ulid>,
}

/// A bill whose invoice waits for its moment, as `fiscal.invoice.scheduled` carries it (ADR-0229
/// decision 4). It has no number yet: the number is taken when it is issued.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct InvoiceSchedule {
    /// When it is due, already held to the country's legal limit.
    pub due_time: Timestamp,
    /// The buyer it will name, when the guest gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buyer_subject_id: Option<SubjectId>,
}

fn token<'de, D, T>(deserializer: D) -> Result<Open<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: WireEnum,
{
    Ok(serde_json::Value::deserialize(deserializer)?
        .as_str()
        .map_or_else(Open::default, Open::parse))
}

fn text<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?
        .as_str()
        .map(str::to_owned))
}

fn whole<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?.as_i64())
}

fn flag<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)?.as_bool())
}

fn is_absent<T: WireEnum + PartialEq>(moment: &Open<T>) -> bool {
    *moment == Open::default()
}

#[cfg(test)]
mod tests {
    use super::{EInvoiceIssueMoment, PublishedEInvoice};

    #[test]
    fn nothing_is_issued_unless_the_node_says_when() {
        for (text, moment, symbol) in [
            ("{}", EInvoiceIssueMoment::Off, None),
            (
                r#"{ "issue_moment": "EINVOICE_ISSUE_MOMENT_IMMEDIATE", "symbol": "1C26MAA" }"#,
                EInvoiceIssueMoment::Immediate,
                Some("1C26MAA"),
            ),
            (
                r#"{ "issue_moment": "EINVOICE_ISSUE_MOMENT_ON_REQUEST", "symbol": "a name" }"#,
                EInvoiceIssueMoment::OnRequest,
                None,
            ),
            (
                r#"{ "issue_moment": "EINVOICE_ISSUE_MOMENT_LATER", "symbol": 7 }"#,
                EInvoiceIssueMoment::Off,
                None,
            ),
        ] {
            let node: PublishedEInvoice = serde_json::from_str(text).unwrap_or_default();
            assert_eq!(node.issue_moment(), moment, "{text}");
            assert_eq!(
                node.symbol().as_ref().map(super::InvoiceSymbol::as_str),
                symbol,
                "{text}"
            );
        }
    }

    #[test]
    fn a_deferred_moment_reads_its_delay_and_batch_minute_within_bounds() {
        for (text, delay, batch) in [
            ("{}", 0, 1_320),
            (r#"{ "delay_minutes": 30, "batch_minute": 0 }"#, 30, 0),
            (
                r#"{ "delay_minutes": 1441, "batch_minute": 1440 }"#,
                0,
                1_320,
            ),
            (r#"{ "delay_minutes": "30", "batch_minute": -1 }"#, 0, 1_320),
        ] {
            let node: PublishedEInvoice = serde_json::from_str(text).unwrap_or_default();
            assert_eq!(
                (node.delay_minutes(), node.batch_minute()),
                (delay, batch),
                "{text}"
            );
        }
        assert!(EInvoiceIssueMoment::ScheduledPerOrder.is_deferred());
        assert!(EInvoiceIssueMoment::BatchWindow.is_deferred());
        assert!(!EInvoiceIssueMoment::Immediate.is_deferred());
        assert!(!EInvoiceIssueMoment::OnRequest.is_deferred());
    }

    #[test]
    fn a_voided_bill_is_adjusted_unless_the_node_says_replace() {
        for (text, kind) in [
            ("{}", super::EInvoiceKind::Adjustment),
            (
                r#"{ "correction_kind": "EINVOICE_KIND_REPLACEMENT" }"#,
                super::EInvoiceKind::Replacement,
            ),
            (
                r#"{ "correction_kind": "EINVOICE_KIND_ORIGINAL" }"#,
                super::EInvoiceKind::Adjustment,
            ),
            (
                r#"{ "correction_kind": 3 }"#,
                super::EInvoiceKind::Adjustment,
            ),
        ] {
            let node: PublishedEInvoice = serde_json::from_str(text).unwrap_or_default();
            assert_eq!(node.correction_kind(), kind, "{text}");
        }
    }

    #[test]
    fn an_invoiced_bill_goes_to_hq_unless_the_node_names_a_route_within_bounds() {
        use super::InvoicedCorrection as Route;
        for (text, route, limit, window) in [
            ("{}", Route::HqOnly, 0, 0),
            (
                r#"{ "invoiced_correction": "INVOICED_CORRECTION_MANAGER_UP_TO_LIMIT", "manager_correction_limit": 200000 }"#,
                Route::ManagerUpToLimit,
                200_000,
                0,
            ),
            (
                r#"{ "invoiced_correction": "INVOICED_CORRECTION_CANCEL_WINDOW", "cancel_window_minutes": 15 }"#,
                Route::CancelWindow,
                0,
                15,
            ),
            (
                r#"{ "invoiced_correction": "INVOICED_CORRECTION_LATER", "manager_correction_limit": -1, "cancel_window_minutes": 1441 }"#,
                Route::HqOnly,
                0,
                0,
            ),
        ] {
            let node: PublishedEInvoice = serde_json::from_str(text).unwrap_or_default();
            assert_eq!(
                (
                    node.invoiced_correction(),
                    node.manager_correction_limit(),
                    node.cancel_window_minutes()
                ),
                (route, limit, window),
                "{text}"
            );
        }
    }

    #[test]
    fn every_mode_but_off_issues_and_on_request_needs_a_buyer() {
        for deferred in [
            EInvoiceIssueMoment::ScheduledPerOrder,
            EInvoiceIssueMoment::BatchWindow,
        ] {
            assert!(deferred.issues(false));
        }
        assert!(EInvoiceIssueMoment::Immediate.issues(false));
        assert!(!EInvoiceIssueMoment::OnRequest.issues(false));
        assert!(EInvoiceIssueMoment::OnRequest.issues(true));
        assert!(!EInvoiceIssueMoment::Off.issues(true));
        assert!(!EInvoiceIssueMoment::Unspecified.issues(true));
    }
}
