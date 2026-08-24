// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The retention + PII-masking cron (P7 / Track A6,
//! [ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)).
//!
//! Personal data — a marketplace order's name/phone/address, a corporate invoice's buyer fields —
//! lives in the subject store, keyed by [`SubjectId`](pos_proto::ids::SubjectId), never in the event
//! log ([`pos_proto::pii`]). Data-protection law (Vietnam's PDPD, GDPR, CCPA) requires it not be kept
//! past its retention period, so this cron **masks** every subject record older than the configured
//! period: personal values become `[REDACTED]`, while the id and timestamps survive so invoices still
//! reference a subject and the books still reconcile.
//!
//!  * [`subject`] — the record and the one-way, idempotent masking.
//!  * [`policy`] — the retention period (from configuration/country default; never a code guess) and
//!    the "past retention" decision.
//!  * [`sweep`](mod@sweep) — the bounded, idempotent pass and its daily runner.
//!
//! Scope, deliberately: this enforces the *automatic, time-based* policy over customer/buyer data
//! only. It never touches employee data (there is no employee-behaviour monitoring in this system),
//! and it is **not** the path for an individual's erasure/access/portability request — those are
//! escalated to the Data Protection contact and actioned deliberately, per the organisation's policy.

pub mod policy;
pub mod subject;
pub mod sweep;

pub use policy::RetentionPolicy;
pub use subject::{REDACTION, SubjectRecord};
pub use sweep::{DEFAULT_INTERVAL, RetentionError, SubjectStore, SweepReport, run, sweep};
