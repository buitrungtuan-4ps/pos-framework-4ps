// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What every cloud route module needs, whichever business area it serves
//! ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4,
//! the reporting pilot).
//!
//! - [`auth`]: console sessions, roles and permissions, API keys and the bearer check.
//! - [`audit`]: the audit trail a route writes to and the console reads.
//! - [`health`]: the background loops' heartbeats, which the health route and alerts read
//!   (ADR-0068).
//! - [`paging`]: the page request and page every list route shares, and [`paged`]: the paging
//!   params a list route reads and the page it answers with.
//! - [`response`]: the AIP-193 error envelope and the responses built from it, and [`refusal`]:
//!   the refusals a route sends for a request it cannot act on.
//! - [`version`] and [`conditional`]: a record's version, `If-Match`, and the responses that carry
//!   a version (optimistic concurrency, ADR-0094 and ADR-0095).
//!
//! `pos-cloud` re-exports [`auth`], [`audit`], [`health`], [`paging`] and [`version`] under their
//! old paths, so `pos_cloud::auth::…` still names them. The application state, `CloudApp`, stays in
//! `pos-cloud`: it composes every store the binary runs, which is what a route module should not
//! depend on.

pub mod audit;
pub mod auth;
pub mod conditional;
pub mod console;
pub mod csv_file;
pub mod error_schema;
pub mod health;
pub mod paged;
pub mod paging;
mod persistence;
pub mod refusal;
pub mod response;
pub mod version;
