// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! E-invoices in the cloud: the connector side of
//! [ADR-0225](../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md).
//!
//! A store numbers its invoice in the sale's own transaction; the cloud submits it through the
//! tenant's one `einvoice.<vendor>` connection, which implements `Fiscalization` (decision 8). No
//! vendor is chosen yet (an open owner decision), so [`sandbox`] is the provider every build carries.

pub mod automatic;
pub mod corrections;
pub mod feed;
pub mod sandbox;
pub mod submitter;
pub mod symbols;
