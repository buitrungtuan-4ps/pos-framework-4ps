// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud as a store's gateway to the internet
//! ([ADR-0198](../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)).
//!
//! A store never calls the internet. An external act goes from the till to its edge, from the edge
//! to the cloud, and from the cloud to the target, behind the webhook guard. This module holds the
//! targets a tenant declares ([`endpoints`]): an `https` URL, a method, the typed fields a request is
//! built from and a response is read into, a deadline, and secret headers sealed the moment they
//! arrive. It is data, never code.

pub mod call;
pub mod calls;
pub mod endpoints;
mod persistence;
pub mod sync;
