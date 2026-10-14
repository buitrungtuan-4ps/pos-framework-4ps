// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What every cloud route module needs, whichever business area it serves
//! ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4,
//! the reporting pilot).
//!
//! - [`auth`]: console sessions, roles and permissions, API keys and the bearer check.
//! - [`audit`]: the audit trail a route writes to and the console reads.
//! - [`paging`]: the page request and page every list route shares.
//! - [`response`]: the AIP-193 error envelope and the responses built from it.
//!
//! `pos-cloud` re-exports [`auth`], [`audit`] and [`paging`] under their old paths, so
//! `pos_cloud::auth::…` still names them. The application state, `CloudApp`, stays in `pos-cloud`:
//! it composes every store the binary runs, which is what a route module should not depend on.

pub mod audit;
pub mod auth;
pub mod console;
pub mod csv_file;
pub mod error_schema;
pub mod paging;
mod persistence;
pub mod response;
